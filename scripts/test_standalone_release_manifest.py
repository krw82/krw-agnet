#!/usr/bin/env python3
"""Small offline regression test for the standalone bundle verifier."""

from __future__ import annotations

import json
import pathlib
import subprocess
import sys
import tempfile
import unittest

from verify_standalone_release import SCHEMA_VERSION, canonical_bytes, content_hash, verify_bundle


class StandaloneReleaseManifestTest(unittest.TestCase):
    def build_bundle(self, root: pathlib.Path) -> pathlib.Path:
        binary = root / "bin" / "krw-agent"
        binary.parent.mkdir()
        binary.write_bytes(b"deterministic test binary")
        files = [
            {
                "path": "bin/krw-agent",
                "bytes": binary.stat().st_size,
                "content_hash": content_hash(binary.read_bytes()),
            }
        ]
        manifest = {
            "schema_version": SCHEMA_VERSION,
            "git_commit": "a" * 40,
            "git_tree": "b" * 40,
            "physical_models": ["glm-5.2"],
            "files": files,
        }
        manifest["manifest_hash"] = content_hash(canonical_bytes(manifest))
        (root / "release-manifest.json").write_bytes(
            json.dumps(manifest, sort_keys=True, indent=2).encode("utf-8")
        )
        return binary

    def test_writer_and_verifier_agree_on_a_real_inventory(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            binary = root / "bin" / "krw-agent"
            binary.parent.mkdir()
            binary.write_bytes(b"writer inventory input")
            writer = pathlib.Path(__file__).with_name("write_release_manifest.py")
            completed = subprocess.run(
                [
                    sys.executable,
                    str(writer),
                    "--root",
                    str(root),
                    "--git-commit",
                    "a" * 40,
                    "--git-tree",
                    "b" * 40,
                    "--model",
                    "glm-5.2",
                ],
                check=False,
                capture_output=True,
                text=True,
            )
            self.assertEqual(completed.returncode, 0, completed.stderr)
            report = verify_bundle(root)
            self.assertEqual(report["file_count"], 1)

    def test_verified_bundle_rejects_tampered_or_extra_files(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            binary = self.build_bundle(root)
            report = verify_bundle(root)
            self.assertEqual(report["status"], "verified")
            binary.write_bytes(b"tampered")
            with self.assertRaises(ValueError):
                verify_bundle(root)

        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            self.build_bundle(root)
            (root / "unexpected.txt").write_text("not in manifest", encoding="utf-8")
            with self.assertRaises(ValueError):
                verify_bundle(root)

        if sys.platform != "win32":
            with tempfile.TemporaryDirectory() as directory:
                root = pathlib.Path(directory)
                binary = self.build_bundle(root)
                (root / "linked-agent").symlink_to(binary)
                with self.assertRaises(ValueError):
                    verify_bundle(root)


if __name__ == "__main__":
    unittest.main()
