#!/usr/bin/env python3
"""Offline tests for the safe terminal action trace in the quality runner.

Run with:
  python3 scripts/test_live_quality_matrix.py
"""

from __future__ import annotations

import importlib.util
import sys
import unittest
from pathlib import Path


SCRIPT_PATH = Path(__file__).with_name("run_live_quality_matrix.py")
SPEC = importlib.util.spec_from_file_location("live_quality_matrix", SCRIPT_PATH)
assert SPEC is not None and SPEC.loader is not None
quality = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = quality
SPEC.loader.exec_module(quality)


class TerminalActionTraceTest(unittest.TestCase):
    session_id = "ses_qualitytrace01"
    run_id = "run_qualitytrace01"
    result_hash = "sha256:" + "a" * 64

    def trace(self) -> dict[str, object]:
        return {
            "schema_version": 1,
            "session_id": self.session_id,
            "run_id": self.run_id,
            "state": "final",
            "actions": [
                {
                    "capability_id": "ontology.query_context",
                    "stage": "accepted",
                    "result_hash": self.result_hash,
                },
                {
                    "capability_id": "ontology.chain",
                    "stage": "accepted",
                    "result_hash": self.result_hash,
                },
            ],
        }

    def test_accepts_only_safe_action_metadata_and_marks_chain(self) -> None:
        actions = quality.validate_terminal_trace(
            self.trace(), self.session_id, self.run_id, "final"
        )
        trace = quality.action_trace_block(actions)
        self.assertEqual(trace["status"], "available")
        self.assertEqual(trace["action_count"], 2)
        self.assertTrue(trace["chain_capability_called"])
        self.assertEqual(trace["private_reasoning"], "not_collected")
        self.assertNotIn("arguments", trace)
        self.assertNotIn("result", trace)

    def test_rejects_raw_artifact_or_identity_tampering(self) -> None:
        leaking = self.trace()
        actions = leaking["actions"]
        assert isinstance(actions, list)
        first = actions[0]
        assert isinstance(first, dict)
        first["arguments_artifact_ref"] = "must-not-leak"
        with self.assertRaisesRegex(quality.GatewayProblem, "terminal_trace_action_shape_invalid"):
            quality.validate_terminal_trace(leaking, self.session_id, self.run_id, "final")

        wrong_state = self.trace()
        wrong_state["state"] = "cancelled"
        with self.assertRaisesRegex(quality.GatewayProblem, "terminal_trace_identity_invalid"):
            quality.validate_terminal_trace(wrong_state, self.session_id, self.run_id, "final")

    def test_terminal_status_carries_credit_usage_or_markdown_retry_guidance(self) -> None:
        final = {
            "schema_version": 1,
            "session_id": self.session_id,
            "run_id": self.run_id,
            "state": "final",
            "final_output": {"markdown": "## 답변", "final_output_hash": self.result_hash},
            "usage": {
                "provider_turns": 3,
                "capability_calls": 2,
                "repairs": 1,
                "input_tokens": 1200,
                "output_tokens": 800,
                "total_tokens": 2000,
                "provider_total_ms": 1000,
                "capability_total_ms": 120,
            },
            "retry_message": None,
        }
        _, _, state, answer, _, usage, retry = quality.validate_status(
            final, self.session_id, self.run_id
        )
        self.assertEqual(state, "final")
        self.assertEqual(answer, "## 답변")
        self.assertEqual(usage["total_tokens"], 2000)
        self.assertIsNone(retry)

        failed = {
            "schema_version": 1,
            "session_id": self.session_id,
            "run_id": self.run_id,
            "state": "failed",
            "final_output": None,
            "usage": None,
            "retry_message": {
                "markdown": "## 분석을 완료하지 못했습니다\n\n다시 요청해 주세요.",
                "category": "data_connection",
                "retry_recommended": True,
            },
        }
        _, _, state, answer, _, usage, retry = quality.validate_status(
            failed, self.session_id, self.run_id
        )
        self.assertEqual(state, "failed")
        self.assertIsNone(answer)
        self.assertIsNone(usage)
        self.assertEqual(retry["category"], "data_connection")


if __name__ == "__main__":
    unittest.main()
