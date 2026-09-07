#!/usr/bin/env python3
"""Offline checks for the final dual-provider evidence gate."""

from __future__ import annotations

import hashlib
import json
import pathlib
import subprocess
import sys
import tempfile
import unittest

from write_dual_provider_evidence_index import (
    validate_acceptance,
    validate_canary,
    validate_db,
    validate_sealed_bundle,
)


HASH = "sha256:" + "a" * 64


def bundle(provider: str) -> dict[str, str]:
    return {
        "provider_id": provider,
        "physical_model": "glm-5.3" if provider == "glm" else "deepseek-v4-flash",
        "manifest_hash": HASH,
        "descriptor_artifact_hash": HASH,
        "release_set_hash": HASH,
        "git_commit": "b" * 40,
        "git_tree": "c" * 40,
    }


def acceptance(provider: str) -> dict[str, object]:
    return {
        "schema_version": "krw-live-acceptance-run/v1",
        "status": "pass",
        "provider_id": provider,
        "physical_model": bundle(provider)["physical_model"],
        "release": {
            "manifest_hash": HASH,
            "descriptor_artifact_hash": HASH,
            "release_set_hash": HASH,
        },
        "transport_failed": 0,
        "quality_report_hash": HASH,
        "completed": 9,
        "manual_quality_reviews_required": 9,
    }


class ProductionEvidenceIndexTest(unittest.TestCase):
    def test_sealed_bundle_requires_descriptor_entries_for_selected_model(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory) / "deepseek"
            root.mkdir()
            descriptor = {
                "entries": [{"execution": {"resolved_model": "deepseek-v4-flash"}}],
                "release_set_hash": HASH,
                "schema_version": 3,
            }
            descriptor_raw = json.dumps(
                descriptor, ensure_ascii=False, sort_keys=True, separators=(",", ":")
            ).encode("utf-8")
            (root / "public-release.json").write_bytes(descriptor_raw)
            (root / "release-authorization.json").write_text("{}", encoding="utf-8")
            (root / "release-trust-registry.json").write_text("{}", encoding="utf-8")
            descriptor_hash = "sha256:" + hashlib.sha256(descriptor_raw).hexdigest()
            (root / "frontend-runtime.env").write_text(
                "\n".join(
                    (
                        "KRW_AGENT_PROVIDER=deepseek",
                        f"KRW_AGENT_RELEASE_ARTIFACT_HASH={descriptor_hash}",
                        f"KRW_AGENT_RELEASE_SET_HASH={HASH}",
                    )
                )
                + "\n",
                encoding="utf-8",
            )
            (root / "payload.bin").write_bytes(b"sealed-test-payload")
            writer = pathlib.Path(__file__).with_name("write_release_manifest.py")
            completed = subprocess.run(
                [
                    sys.executable,
                    str(writer),
                    "--root",
                    str(root),
                    "--git-commit",
                    "b" * 40,
                    "--git-tree",
                    "c" * 40,
                    "--provider",
                    "deepseek",
                ],
                check=False,
                capture_output=True,
                text=True,
            )
            self.assertEqual(completed.returncode, 0, completed.stderr)
            result = validate_sealed_bundle(root, "deepseek")
            self.assertEqual(result["physical_model"], "deepseek-v4-flash")

            descriptor["entries"][0]["execution"]["resolved_model"] = "glm-5.3"
            descriptor_raw = json.dumps(
                descriptor, ensure_ascii=False, sort_keys=True, separators=(",", ":")
            ).encode("utf-8")
            (root / "public-release.json").write_bytes(descriptor_raw)
            descriptor_hash = "sha256:" + hashlib.sha256(descriptor_raw).hexdigest()
            (root / "frontend-runtime.env").write_text(
                "\n".join(
                    (
                        "KRW_AGENT_PROVIDER=deepseek",
                        f"KRW_AGENT_RELEASE_ARTIFACT_HASH={descriptor_hash}",
                        f"KRW_AGENT_RELEASE_SET_HASH={HASH}",
                    )
                )
                + "\n",
                encoding="utf-8",
            )
            completed = subprocess.run(
                [
                    sys.executable,
                    str(writer),
                    "--root",
                    str(root),
                    "--git-commit",
                    "b" * 40,
                    "--git-tree",
                    "c" * 40,
                    "--provider",
                    "deepseek",
                ],
                check=False,
                capture_output=True,
                text=True,
            )
            self.assertEqual(completed.returncode, 0, completed.stderr)
            with self.assertRaisesRegex(ValueError, "model does not match"):
                validate_sealed_bundle(root, "deepseek")

    def test_provider_acceptance_is_bound_to_exact_bundle(self) -> None:
        self.assertEqual(
            validate_acceptance(acceptance("deepseek"), "deepseek", bundle("deepseek"))["quality_report_hash"],
            HASH,
        )

    def test_provider_mismatch_is_rejected(self) -> None:
        with self.assertRaises(ValueError):
            validate_acceptance(acceptance("glm"), "deepseek", bundle("deepseek"))

    def test_database_and_canary_require_pass_fields(self) -> None:
        self.assertEqual(validate_db({"ok": True, "checked": 18})["status"], "passed")
        canary = {
            "status": "pass",
            "provider_id": "deepseek",
            "physical_model": "deepseek-v4-flash",
            "manifest_hash": HASH,
            "release_set_hash": HASH,
            "durable_final_projection": True,
            "non_empty_answer": True,
            "same_session_followup": True,
            "cross_session_isolation": True,
        }
        self.assertEqual(validate_canary(canary, bundle("deepseek"))["status"], "passed")


if __name__ == "__main__":
    unittest.main()
