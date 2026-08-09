#!/usr/bin/env python3
"""Write the deterministic public file inventory for a standalone bundle."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import tempfile


def sha256(data: bytes) -> str:
    return "sha256:" + hashlib.sha256(data).hexdigest()


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", required=True, type=pathlib.Path)
    parser.add_argument("--git-commit", required=True)
    parser.add_argument("--git-tree", required=True)
    parser.add_argument("--model", required=True)
    return parser.parse_args()


def main() -> None:
    args = parse_args()
    root = args.root.resolve()
    if not root.is_dir() or root == pathlib.Path("/") or root.is_symlink():
        raise SystemExit("bundle root must be a real non-root directory")
    if args.model != "glm-5.2":
        raise SystemExit("standalone bundle supports only glm-5.2")
    files = []
    for path in sorted(root.rglob("*"), key=lambda item: item.relative_to(root).as_posix()):
        if not path.is_file() or path.is_symlink() or path.name == "release-manifest.json":
            continue
        data = path.read_bytes()
        files.append(
            {
                "path": path.relative_to(root).as_posix(),
                "bytes": len(data),
                "content_hash": sha256(data),
            }
        )
    manifest = {
        "schema_version": "krw-standalone-release/v1",
        "git_commit": args.git_commit,
        "git_tree": args.git_tree,
        "physical_models": [args.model],
        "files": files,
    }
    body = json.dumps(
        manifest, ensure_ascii=False, sort_keys=True, separators=(",", ":"), allow_nan=False
    ).encode("utf-8")
    manifest["manifest_hash"] = sha256(body)
    output = root / "release-manifest.json"
    descriptor, temporary_name = tempfile.mkstemp(prefix=".release-manifest.", dir=root)
    temporary = pathlib.Path(temporary_name)
    try:
        with os.fdopen(descriptor, "wb") as file:
            file.write(json.dumps(manifest, ensure_ascii=False, indent=2).encode("utf-8"))
            file.flush()
            os.fsync(file.fileno())
        os.replace(temporary, output)
    finally:
        temporary.unlink(missing_ok=True)


if __name__ == "__main__":
    main()
