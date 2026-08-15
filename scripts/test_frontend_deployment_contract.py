#!/usr/bin/env python3
from __future__ import annotations

import json
import pathlib
import subprocess
import tempfile
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[1]
VERIFIER = ROOT / "scripts" / "verify_frontend_deployment_contract.py"


def valid_contract() -> dict[str, object]:
    return {
        "schema_version": 1,
        "contract_id": "krw.agent/frontend-deployment-contract/v1",
        "agent_abi": "agent_v1_v7",
        "executor_type": "rust_agent",
        "session_backend": "rust_agent",
        "run_submission_contract": "agent-v1-run-request/v1",
        "final_projection_contract": "agent-final-projection/v1",
        "required_projection_fields": [
            "run_id",
            "answer_bundle_hash",
            "final_output_hash",
            "markdown",
        ],
        "optional_projection_fields": [
            "visualizations",
            "usage",
            "evidence_ledger_hash",
            "memory_revision",
            "memory_frontier_hash",
        ],
        "visualization_failure_policy": "omit_artifact_keep_answer",
        "admission_policy": "open_after_exact_release_heartbeat",
    }


class FrontendDeploymentContractTest(unittest.TestCase):
    def verify(self, value: dict[str, object]) -> subprocess.CompletedProcess[str]:
        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory) / "contract.json"
            path.write_text(json.dumps(value), encoding="utf-8")
            return subprocess.run(
                ["python3", str(VERIFIER), "--contract", str(path)],
                cwd=ROOT,
                text=True,
                capture_output=True,
                check=False,
            )

    def test_accepts_the_exact_frontend_agent_boundary(self) -> None:
        result = self.verify(valid_contract())
        self.assertEqual(result.returncode, 0, result.stderr)
        receipt = json.loads(result.stdout)
        self.assertEqual(receipt["status"], "ready")
        self.assertRegex(receipt["contract_hash"], r"^sha256:[0-9a-f]{64}$")

    def test_rejects_a_contract_that_cannot_return_core_markdown(self) -> None:
        contract = valid_contract()
        contract["required_projection_fields"] = [
            "run_id",
            "answer_bundle_hash",
            "final_output_hash",
        ]
        result = self.verify(contract)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("required_projection_fields_mismatch", result.stderr)

    def test_rejects_visualizations_that_can_fail_the_core_answer(self) -> None:
        contract = valid_contract()
        contract["visualization_failure_policy"] = "fail_projection"
        result = self.verify(contract)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("visualization_failure_policy_invalid", result.stderr)

    def test_rejects_a_symlinked_contract_boundary(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            directory_path = pathlib.Path(directory)
            real_path = directory_path / "real-contract.json"
            link_path = directory_path / "contract.json"
            real_path.write_text(json.dumps(valid_contract()), encoding="utf-8")
            link_path.symlink_to(real_path)

            result = subprocess.run(
                ["python3", str(VERIFIER), "--contract", str(link_path)],
                cwd=ROOT,
                text=True,
                capture_output=True,
                check=False,
            )

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("frontend_contract_missing_or_unsafe", result.stderr)

    def test_accepts_additive_optional_projection_metadata(self) -> None:
        contract = valid_contract()
        contract["optional_projection_fields"] = [
            *contract["optional_projection_fields"],
            "runtime_timings",
        ]
        contract["description"] = "non-authoritative operator metadata"

        result = self.verify(contract)

        self.assertEqual(result.returncode, 0, result.stderr)


if __name__ == "__main__":
    unittest.main()
