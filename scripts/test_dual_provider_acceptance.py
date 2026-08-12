#!/usr/bin/env python3
"""Offline checks for dual-provider acceptance evidence shape."""

from __future__ import annotations

import hashlib
import json
import pathlib
import tempfile
import unittest

from release_provider import validate_public_descriptor
from run_dual_provider_acceptance import validate_session_groups


class AcceptanceEvidenceTest(unittest.TestCase):
    def test_followup_group_must_stay_in_one_session(self) -> None:
        report = {
            "results": [
                {"session_group": "room-a", "execution_trace": {"session_id": "ses_1"}},
                {"session_group": "room-a", "execution_trace": {"session_id": "ses_1"}},
            ]
        }
        self.assertEqual(validate_session_groups(report, live=True), {"room-a": "ses_1"})

    def test_cross_session_followup_is_rejected(self) -> None:
        report = {
            "results": [
                {"session_group": "room-a", "execution_trace": {"session_id": "ses_1"}},
                {"session_group": "room-a", "execution_trace": {"session_id": "ses_2"}},
            ]
        }
        with self.assertRaises(ValueError):
            validate_session_groups(report, live=True)

    def test_dry_run_does_not_require_session_ids(self) -> None:
        report = {
            "results": [
                {"session_group": "room-a", "execution_trace": {"session_id": None}},
            ]
        }
        self.assertEqual(validate_session_groups(report, live=False), {})

    def test_live_descriptor_is_bound_to_selected_provider(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory) / "public-release.json"
            value = {
                "schema_version": 3,
                "release_set_hash": "sha256:" + "b" * 64,
                "runtime_version": "0.1.0",
                "entries": [
                    {"execution": {"resolved_model": "deepseek-v4-flash"}},
                ],
            }
            raw = (json.dumps(value, sort_keys=True) + "\n").encode()
            path.write_bytes(raw)
            artifact_hash = "sha256:" + hashlib.sha256(raw).hexdigest()
            report = validate_public_descriptor(
                path,
                "deepseek",
                expected_artifact_hash=artifact_hash,
                expected_release_set_hash=value["release_set_hash"],
            )
            self.assertEqual(report["physical_model"], "deepseek-v4-flash")

    def test_live_descriptor_cannot_be_labelled_as_another_provider(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory) / "public-release.json"
            value = {
                "schema_version": 3,
                "release_set_hash": "sha256:" + "c" * 64,
                "runtime_version": "0.1.0",
                "entries": [
                    {"execution": {"resolved_model": "glm-5.2"}},
                ],
            }
            raw = (json.dumps(value, sort_keys=True) + "\n").encode()
            path.write_bytes(raw)
            with self.assertRaisesRegex(ValueError, "model does not match"):
                validate_public_descriptor(path, "deepseek")


if __name__ == "__main__":
    unittest.main()
