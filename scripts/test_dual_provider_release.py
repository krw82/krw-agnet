#!/usr/bin/env python3
"""Offline contract checks for the two-provider release layout.

The test deliberately never contacts either provider and never starts a daemon.
It verifies that GLM and DeepSeek bundles use the closed provider/model mapping,
that the common artifacts are byte-identical, and that a staging directory is
not accidentally treated as a sealed production release.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import pathlib
import tempfile
import unittest

from release_provider import PROVIDER_MODELS, RELEASE_SCHEMA_VERSION
from verify_standalone_release import canonical_bytes, content_hash, verify_bundle


PROVIDERS = tuple(PROVIDER_MODELS)
PROVIDER_FILES = {
    "deployments/deployment-binding.yaml",
    "deployments/endpoint-registry.yaml",
    "deployments/model-registry.yaml",
    "release-manifest.json",
    "public-release.json",
    "release-authorization.json",
    "release-trust-registry.json",
    "frontend-runtime.env",
}
SEALED_FILES = {
    "public-release.json",
    "release-authorization.json",
    "release-trust-registry.json",
}


def file_hash(path: pathlib.Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def write_fixture_bundle(root: pathlib.Path, provider: str) -> None:
    payload = root / "bin" / "krw-agent"
    payload.parent.mkdir(parents=True, exist_ok=True)
    payload.write_bytes(b"same common binary")
    registry = root / "deployments" / "model-registry.yaml"
    registry.parent.mkdir(parents=True, exist_ok=True)
    registry.write_text(f"provider: {provider}\nmodel: {PROVIDER_MODELS[provider]}\n")
    files = []
    for path in sorted(root.rglob("*")):
        if path.is_file():
            files.append(
                {
                    "path": path.relative_to(root).as_posix(),
                    "bytes": path.stat().st_size,
                    "content_hash": content_hash(path.read_bytes()),
                }
            )
    manifest = {
        "schema_version": RELEASE_SCHEMA_VERSION,
        "git_commit": "a" * 40,
        "git_tree": "b" * 40,
        "provider_id": provider,
        "physical_models": [PROVIDER_MODELS[provider]],
        "files": files,
    }
    manifest["manifest_hash"] = content_hash(canonical_bytes(manifest))
    (root / "release-manifest.json").write_text(
        json.dumps(manifest, ensure_ascii=False, sort_keys=True, indent=2) + "\n",
        encoding="utf-8",
    )


def verify_dual_root(root: pathlib.Path, expect_sealed: bool) -> dict[str, object]:
    allowed_root_entries = set(PROVIDERS) | {"dual-release-index.json"}
    unexpected_root_entries = {
        entry.name for entry in root.iterdir() if entry.name not in allowed_root_entries
    }
    if unexpected_root_entries:
        raise ValueError("dual release root contains unexpected entries")
    reports = {
        provider: verify_bundle(root / provider) for provider in PROVIDERS
    }
    glm = root / "glm"
    deepseek = root / "deepseek"
    common_paths = set(
        path.relative_to(glm).as_posix()
        for path in glm.rglob("*")
        if path.is_file()
    ) & {
        path.relative_to(deepseek).as_posix()
        for path in deepseek.rglob("*")
        if path.is_file()
    }
    common_paths -= PROVIDER_FILES
    for relative in common_paths:
        if file_hash(glm / relative) != file_hash(deepseek / relative):
            raise ValueError(f"common artifact differs: {relative}")
    for provider in PROVIDERS:
        present = {
            path.relative_to(root / provider).as_posix()
            for path in (root / provider).rglob("*")
            if path.is_file()
        }
        if expect_sealed and not SEALED_FILES <= present:
            raise ValueError(f"sealed files missing for {provider}")
        if not expect_sealed and present & SEALED_FILES:
            raise ValueError(f"unsigned bundle contains sealed files for {provider}")
    return reports


class DualProviderReleaseTest(unittest.TestCase):
    def test_provider_models_are_closed(self) -> None:
        self.assertEqual(PROVIDER_MODELS, {
            "glm": "glm-5.2",
            "deepseek": "deepseek-v4-flash",
        })

    def test_fixture_dual_root_is_verified_and_common_bytes_match(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            for provider in PROVIDERS:
                write_fixture_bundle(root / provider, provider)
            reports = verify_dual_root(root, expect_sealed=False)
            self.assertEqual(reports["glm"]["provider_id"], "glm")
            self.assertEqual(reports["deepseek"]["provider_id"], "deepseek")

    def test_mixed_provider_manifest_fails(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            write_fixture_bundle(root / "deepseek", "deepseek")
            manifest_path = root / "deepseek" / "release-manifest.json"
            manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
            manifest["provider_id"] = "glm"
            manifest["manifest_hash"] = content_hash(
                canonical_bytes({
                    key: value for key, value in manifest.items()
                    if key != "manifest_hash"
                })
            )
            manifest_path.write_text(
                json.dumps(manifest, sort_keys=True, indent=2) + "\n",
                encoding="utf-8",
            )
            with self.assertRaises(ValueError):
                verify_bundle(root / "deepseek")

    def test_common_staging_directory_is_not_a_release_root_entry(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            for provider in PROVIDERS:
                write_fixture_bundle(root / provider, provider)
            (root / "common").mkdir()
            with self.assertRaisesRegex(ValueError, "unexpected entries"):
                verify_dual_root(root, expect_sealed=False)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--staging-root", type=pathlib.Path)
    parser.add_argument(
        "--expect-sealed",
        choices=("true", "false"),
        default="false",
    )
    args = parser.parse_args()
    if args.staging_root:
        verify_dual_root(args.staging_root, args.expect_sealed == "true")
        print(json.dumps({"status": "verified", "root": str(args.staging_root)}))
        return 0
    result = unittest.main(argv=[__file__], exit=False)
    return 0 if result.result.wasSuccessful() else 1


if __name__ == "__main__":
    raise SystemExit(main())
