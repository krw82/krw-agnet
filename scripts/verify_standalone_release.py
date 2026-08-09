#!/usr/bin/env python3
"""Verify a secret-free standalone KRW Agent bundle manifest offline.

The bundle writer records hashes, but an operator also needs a fail-closed
reader before installing a directory from removable media or an artifact
store.  This verifier accepts no network input and rejects symlinks, missing
or extra files, non-canonical manifest hashes, unsafe paths, and every model
other than the exact physical DeepSeek Flash model.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import stat
import sys
from typing import Any


SCHEMA_VERSION = "krw-standalone-release/v1"
MODEL_ID = "glm-5.2"
MAX_MANIFEST_BYTES = 16 * 1024 * 1024
MAX_FILE_BYTES = 4 * 1024 * 1024 * 1024


def canonical_bytes(value: Any) -> bytes:
    return json.dumps(
        value, ensure_ascii=False, sort_keys=True, separators=(",", ":"), allow_nan=False
    ).encode("utf-8")


def content_hash(value: bytes) -> str:
    return "sha256:" + hashlib.sha256(value).hexdigest()


def open_regular_file(path: pathlib.Path, maximum_bytes: int) -> int:
    flags = (
        os.O_RDONLY
        | getattr(os, "O_NOFOLLOW", 0)
        | getattr(os, "O_CLOEXEC", 0)
        | getattr(os, "O_NONBLOCK", 0)
    )
    try:
        descriptor = os.open(path, flags)
    except OSError as error:
        raise ValueError("bundle file is unreadable or unsafe") from error
    try:
        metadata = os.fstat(descriptor)
        if not stat.S_ISREG(metadata.st_mode) or metadata.st_size <= 0 or metadata.st_size > maximum_bytes:
            raise ValueError("bundle file is outside the allowed byte bound")
    except BaseException:
        os.close(descriptor)
        raise
    return descriptor


def file_content_hash(path: pathlib.Path, maximum_bytes: int) -> tuple[str, int]:
    digest = hashlib.sha256()
    total = 0
    with os.fdopen(open_regular_file(path, maximum_bytes), "rb") as source:
        while chunk := source.read(1024 * 1024):
            total += len(chunk)
            if total > maximum_bytes:
                raise ValueError("bundle file exceeds the byte bound")
            digest.update(chunk)
    return "sha256:" + digest.hexdigest(), total


def valid_hash(value: object) -> bool:
    return (
        isinstance(value, str)
        and len(value) == 71
        and value.startswith("sha256:")
        and all(character in "0123456789abcdef" for character in value[7:])
    )


def safe_relative_path(value: object) -> pathlib.PurePosixPath:
    if not isinstance(value, str) or not value or "\\" in value:
        raise ValueError("manifest path is invalid")
    path = pathlib.PurePosixPath(value)
    if path.is_absolute() or any(part in ("", ".", "..") for part in path.parts):
        raise ValueError("manifest path is unsafe")
    return path


def regular_files(root: pathlib.Path) -> set[str]:
    files: set[str] = set()
    for current, directories, names in os.walk(root, followlinks=False):
        current_path = pathlib.Path(current)
        for name in directories:
            candidate = current_path / name
            if candidate.is_symlink():
                raise ValueError("bundle contains a directory symlink")
        for name in names:
            candidate = current_path / name
            if candidate.is_symlink() or not candidate.is_file():
                raise ValueError("bundle contains an unsafe non-regular file")
            relative = candidate.relative_to(root).as_posix()
            if relative != "release-manifest.json":
                files.add(relative)
    return files


def read_manifest(root: pathlib.Path) -> dict[str, Any]:
    path = root / "release-manifest.json"
    with os.fdopen(open_regular_file(path, MAX_MANIFEST_BYTES), "rb") as source:
        value = json.loads(source.read())
    if not isinstance(value, dict):
        raise ValueError("release manifest must be an object")
    return value


def verify_bundle(root: pathlib.Path) -> dict[str, Any]:
    if root.is_symlink() or not root.is_dir() or root.resolve() == pathlib.Path("/"):
        raise ValueError("bundle root must be a real non-root directory")
    root = root.resolve()
    manifest = read_manifest(root)
    allowed = {
        "schema_version",
        "git_commit",
        "git_tree",
        "physical_models",
        "files",
        "manifest_hash",
    }
    if set(manifest) != allowed or manifest.get("schema_version") != SCHEMA_VERSION:
        raise ValueError("release manifest schema is invalid")
    if manifest.get("physical_models") != [MODEL_ID]:
        raise ValueError("bundle physical model policy is invalid")
    for identifier in (manifest.get("git_commit"), manifest.get("git_tree")):
        if (
            not isinstance(identifier, str)
            or len(identifier) not in (40, 64)
            or not all(character in "0123456789abcdef" for character in identifier)
        ):
            raise ValueError("bundle Git identity is invalid")
    expected_manifest_hash = manifest.pop("manifest_hash")
    if not valid_hash(expected_manifest_hash) or content_hash(canonical_bytes(manifest)) != expected_manifest_hash:
        raise ValueError("release manifest hash mismatch")
    manifest["manifest_hash"] = expected_manifest_hash

    entries = manifest.get("files")
    if not isinstance(entries, list) or not entries:
        raise ValueError("release manifest has no files")
    declared: set[str] = set()
    previous = ""
    for entry in entries:
        if not isinstance(entry, dict) or set(entry) != {"path", "bytes", "content_hash"}:
            raise ValueError("release manifest file entry is invalid")
        relative = safe_relative_path(entry["path"]).as_posix()
        if relative <= previous or relative in declared:
            raise ValueError("release manifest entries are not uniquely sorted")
        previous = relative
        declared.add(relative)
        size = entry.get("bytes")
        if not isinstance(size, int) or isinstance(size, bool) or size <= 0 or size > MAX_FILE_BYTES:
            raise ValueError("release manifest file size is invalid")
        if not valid_hash(entry.get("content_hash")):
            raise ValueError("release manifest file hash is invalid")
        candidate = root.joinpath(*pathlib.PurePosixPath(relative).parts)
        if candidate.is_symlink() or not candidate.is_file() or candidate.stat().st_size != size:
            raise ValueError("bundle file is missing or size-mismatched")
        observed_hash, observed_size = file_content_hash(candidate, MAX_FILE_BYTES)
        if observed_size != size or observed_hash != entry["content_hash"]:
            raise ValueError("bundle file content hash mismatch")
    if declared != regular_files(root):
        raise ValueError("bundle files differ from manifest inventory")

    return {
        "status": "verified",
        "schema_version": SCHEMA_VERSION,
        "manifest_hash": expected_manifest_hash,
        "file_count": len(declared),
        "physical_models": [MODEL_ID],
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=pathlib.Path, required=True)
    args = parser.parse_args()
    try:
        report = verify_bundle(args.root)
    except (OSError, ValueError, json.JSONDecodeError) as error:
        # Artifact names and operator paths are not useful in generic logs.
        print(content_hash(str(error).encode("utf-8")), file=sys.stderr)
        return 1
    print(json.dumps(report, ensure_ascii=False, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
