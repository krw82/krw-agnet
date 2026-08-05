#!/usr/bin/env python3
"""Unit tests for scoped numeric grading in grade_numeric_accuracy.py.

These tests pin the behaviors introduced in Task 8 of the runtime-perf-eval
plan:
  * ``check_metric`` only looks at numbers in sentences that mention BOTH the
    metric keyword AND the fiscal year (so a number from a different metric or
    a different year cannot pass).
  * If no sentence matches, the check fails with ``metric_context_not_found``
    (it is NOT a pass).
  * ``--validate`` mode enforces the ground_truth.json schema and detects
    duplicate question ids.

Run with:
    python3 -m unittest scripts.test_grade_numeric
or:
    python3 scripts/test_grade_numeric.py
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

# Make the sibling module importable when run as a script or via -m.
SCRIPTS_DIR = Path(__file__).resolve().parent
if str(SCRIPTS_DIR) not in sys.path:
    sys.path.insert(0, str(SCRIPTS_DIR))

import grade_numeric_accuracy as g  # noqa: E402


REPO_ROOT = SCRIPTS_DIR.parent
GT_PATH = REPO_ROOT / "evals" / "numeric-accuracy" / "ground_truth.json"
GRADER = SCRIPTS_DIR / "grade_numeric_accuracy.py"


class ScopedCheckMetricTest(unittest.TestCase):
    """Cases 1-4 from the task brief."""

    # AAPL FY2024 revenue ground truth is 391,035,000,000 (q1).
    REVENUE_2024 = 391_035_000_000
    # Operating margin FY2024 = 32% (GOOGL q8).
    OP_MARGIN_2024 = 32

    # ── Case 1: correct 2024 revenue in a Korean sentence ───────────────
    def test_correct_2024_revenue_passes(self) -> None:
        text = (
            "애플의 2024년 매출은 3,943억 5,000만 달러로 기록되었습니다.\n"
            "state=final\n"
        )
        c = g.check_metric(
            text, "revenue", 2024, self.REVENUE_2024, "USD", tolerance_pct=3
        )
        self.assertTrue(c.passed, f"should pass: {c}")
        self.assertIsNotNone(c.found)
        self.assertIsNone(c.reason)

    # ── Case 2: only 2023 revenue present; 2024 not found → fail ────────
    def test_wrong_year_2023_does_not_pass_as_2024(self) -> None:
        text = (
            "애플의 2023년 매출은 3,832억 달러였습니다.\n"
            "state=final\n"
        )
        c = g.check_metric(
            text, "revenue", 2024, self.REVENUE_2024, "USD", tolerance_pct=3
        )
        self.assertFalse(c.passed)
        self.assertIsNone(c.found)
        self.assertEqual(c.reason, "metric_context_not_found")

    # ── Case 3: revenue + unrelated metric in a DIFFERENT sentence ──────
    # The picker should only see the revenue-context number, not the
    # net_income-context number that happens to live in another sentence.
    def test_picks_revenue_context_not_unrelated_metric(self) -> None:
        text = (
            "애플의 2024년 매출은 3,943억 달러입니다.\n"
            "2024년 순이익은 937억 달러였습니다.\n"
            "state=final\n"
        )
        c = g.check_metric(
            text, "revenue", 2024, self.REVENUE_2024, "USD", tolerance_pct=3
        )
        self.assertTrue(c.passed, f"should pass on revenue-context number: {c}")
        # Must be the ~3.943e11 revenue number, not the ~9.37e10 net income.
        self.assertGreater(c.found, 3e11)
        self.assertLess(c.found, 4e11)

        # And the symmetric direction: net_income should pick its own context.
        c2 = g.check_metric(
            text, "net_income", 2024, 93_736_000_000, "USD", tolerance_pct=3
        )
        self.assertTrue(c2.passed, f"net_income should pass: {c2}")
        self.assertLess(c2.found, 1e11)

    # ── Case 4: percent metric without the metric keyword → fail ────────
    def test_percent_without_metric_keyword_fails(self) -> None:
        # The percent value 32% appears but no operating-margin keyword.
        text = (
            "2024년에 회사는 전반적으로 32%를 기록했습니다.\n"
            "state=final\n"
        )
        c = g.check_metric(
            text,
            "operating_margin",
            2024,
            self.OP_MARGIN_2024,
            "percent",
            tolerance_pct=15,
        )
        self.assertFalse(c.passed)
        self.assertIsNone(c.found)
        self.assertEqual(c.reason, "metric_context_not_found")

    # ── Bonus: percent metric WITH keyword in same sentence → pass ──────
    def test_percent_with_metric_keyword_passes(self) -> None:
        text = "2024년 영업이익률은 32%였습니다.\nstate=final\n"
        c = g.check_metric(
            text,
            "operating_margin",
            2024,
            self.OP_MARGIN_2024,
            "percent",
            tolerance_pct=15,
        )
        self.assertTrue(c.passed, f"should pass: {c}")
        self.assertEqual(c.found, 32.0)

    # ── Bonus: unknown metric id falls closed (no keyword → fail) ───────
    def test_unknown_metric_fails_closed(self) -> None:
        text = "매출 2024년 3,943억 달러. state=final"
        c = g.check_metric(
            text, "totally_unknown_metric_xyz", 2024, 1e11, "USD", tolerance_pct=5
        )
        self.assertFalse(c.passed)
        self.assertEqual(c.reason, "metric_context_not_found")


class ValidateModeTest(unittest.TestCase):
    """Case 5 from the task brief: --validate mode."""

    def _run_validate(self, gt_json: str) -> int:
        with tempfile.NamedTemporaryFile(
            "w", suffix=".json", delete=False, encoding="utf-8"
        ) as tf:
            tf.write(gt_json)
            path = tf.name
        try:
            return subprocess.run(
                [sys.executable, str(GRADER), "--validate", path],
                capture_output=True,
                text=True,
            ).returncode
        finally:
            os.unlink(path)

    def test_real_ground_truth_is_valid(self) -> None:
        # The shipped ground_truth.json must validate cleanly.
        rc = subprocess.run(
            [sys.executable, str(GRADER), "--validate", str(GT_PATH)],
            capture_output=True,
            text=True,
        ).returncode
        self.assertEqual(rc, 0, "shipped ground_truth.json must be valid")

    def test_valid_minimal_ground_truth_returns_0(self) -> None:
        gt = {
            "description": "minimal valid fixture",
            "questions": [
                {
                    "id": "q1",
                    "ticker": "AAPL",
                    "difficulty": "easy",
                    "question": "q",
                    "expected_metrics": [
                        {
                            "metric": "revenue",
                            "fiscal_year": 2024,
                            "value": 391035000000,
                            "unit": "USD",
                            "tolerance_pct": 3,
                        }
                    ],
                }
            ],
        }
        self.assertEqual(self._run_validate(json.dumps(gt)), 0)

    def test_duplicate_id_returns_1(self) -> None:
        gt = {
            "description": "dup",
            "questions": [
                {
                    "id": "q1",
                    "ticker": "AAPL",
                    "difficulty": "easy",
                    "question": "q",
                    "expected_metrics": [
                        {
                            "metric": "revenue",
                            "fiscal_year": 2024,
                            "value": 1,
                            "unit": "USD",
                            "tolerance_pct": 3,
                        }
                    ],
                },
                {
                    "id": "q1",  # duplicate
                    "ticker": "MSFT",
                    "difficulty": "easy",
                    "question": "q",
                    "expected_metrics": [
                        {
                            "metric": "revenue",
                            "fiscal_year": 2024,
                            "value": 1,
                            "unit": "USD",
                            "tolerance_pct": 3,
                        }
                    ],
                },
            ],
        }
        self.assertEqual(self._run_validate(json.dumps(gt)), 1)

    def test_bad_unit_returns_1(self) -> None:
        gt = {
            "description": "bad unit",
            "questions": [
                {
                    "id": "q1",
                    "ticker": "AAPL",
                    "difficulty": "easy",
                    "question": "q",
                    "expected_metrics": [
                        {
                            "metric": "revenue",
                            "fiscal_year": 2024,
                            "value": 1,
                            "unit": "dollars",  # not allowed
                            "tolerance_pct": 3,
                        }
                    ],
                }
            ],
        }
        self.assertEqual(self._run_validate(json.dumps(gt)), 1)

    def test_bad_tolerance_returns_1(self) -> None:
        gt = {
            "description": "bad tol",
            "questions": [
                {
                    "id": "q1",
                    "ticker": "AAPL",
                    "difficulty": "easy",
                    "question": "q",
                    "expected_metrics": [
                        {
                            "metric": "revenue",
                            "fiscal_year": 2024,
                            "value": 1,
                            "unit": "USD",
                            "tolerance_pct": 200,  # out of range
                        }
                    ],
                }
            ],
        }
        self.assertEqual(self._run_validate(json.dumps(gt)), 1)


class SentenceSplitTest(unittest.TestCase):
    """Sanity checks for the sentence splitter used by scoped matching."""

    def test_korean_da_boundary(self) -> None:
        sents = g._split_sentences("매출은 100억이다. 순이익은 50억이다.")
        self.assertEqual(len(sents), 2)

    def test_newline_boundary(self) -> None:
        sents = g._split_sentences("line one\nline two\nline three")
        self.assertEqual(len(sents), 3)

    def test_does_not_break_decimal_numbers(self) -> None:
        # "3.5%" should NOT be split into "3" and "5%"; '3.5' has no trailing
        # space after the period, so the english-boundary regex must not fire.
        sents = g._split_sentences("margin was 3.5% in 2024.")
        # The trailing '. ' after 2024 may or may not produce a trailing empty
        # element after stripping; assert the value itself stays together.
        joined = " ".join(sents)
        self.assertIn("3.5%", joined)


if __name__ == "__main__":
    unittest.main(verbosity=2)
