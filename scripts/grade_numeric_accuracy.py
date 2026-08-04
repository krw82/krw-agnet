#!/usr/bin/env python3
"""Grade numeric accuracy of agent answers against ground truth values.

Extracts numbers written in Korean units (억/만/조) or raw digits from answer
text, then compares them against expected values from ground_truth.json within
a per-metric tolerance.

Usage:
    python3 scripts/grade_numeric_accuracy.py <answer_file> [--ground-truth evals/numeric-accuracy/ground_truth.json] [--question-id q1]

    # Grade all answer files in a directory:
    python3 scripts/grade_numeric_accuracy.py --dir .local/eval-results --ground-truth evals/numeric-accuracy/ground_truth.json
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Optional


# ── Korean unit parsing ──────────────────────────────────────────────────

# Matches: "3,943억 달러", "592억 4,800만 달러", "60.9억", "3조 2천억"
# Group 1 = 억 number, Group 2 = optional 만 number
_EOK_MAN = re.compile(
    r"(\d{1,4}(?:,\d{3})*(?:\.\d+)?)\s*억"
    r"(?:\s*(\d{1,4}(?:,\d{3})*(?:\.\d+)?)\s*만)?"
)
# Matches standalone "4,800만"
_MAN_ONLY = re.compile(r"(\d{1,4}(?:,\d{3})*(?:\.\d+)?)\s*만\s*(?:달러|원|USD|KRW)?")
# Matches "3조" or "3조 2천억"
_JO_EOK = re.compile(
    r"(\d{1,4}(?:,\d{3})*(?:\.\d+)?)\s*조"
    r"(?:\s*(\d{1,4}(?:,\d{3})*(?:\.\d+)?)\s*천)?"
    r"(?:\s*(\d{1,4}(?:,\d{3})*(?:\.\d+)?)\s*억)?"
)
# Matches raw large numbers: "391,035,000,000" or "391035000000"
_RAW_LARGE = re.compile(r"(\d{1,3}(?:,\d{3}){2,}|\d{10,})")
# Matches percentages: "114%", "2.0%"
_PERCENT = re.compile(r"(\d{1,3}(?:\.\d+)?)\s*%")
# Matches "약 X억" — same as _EOK_MAN, "약" is just ignored by regex


def _parse_number(s: str) -> float:
    """Remove commas and convert to float."""
    return float(s.replace(",", ""))


def extract_korean_units(text: str) -> list[float]:
    """Extract all monetary values expressed in Korean units (억/만/조).

    Returns absolute values (e.g., '3,943억' -> 394_300_000_000.0).
    Deduplicates overlapping matches, keeping the most specific (largest) parse.
    """
    results: list[float] = []

    # Try 조+천+억 pattern first (most specific)
    for m in _JO_EOK.finditer(text):
        jo = _parse_number(m.group(1)) if m.group(1) else 0
        cheon = _parse_number(m.group(2)) if m.group(2) else 0
        eok = _parse_number(m.group(3)) if m.group(3) else 0
        val = jo * 1e12 + cheon * 1e11 + eok * 1e8
        results.append(val)

    # 억 + optional 만
    for m in _EOK_MAN.finditer(text):
        eok = _parse_number(m.group(1)) if m.group(1) else 0
        man = _parse_number(m.group(2)) if m.group(2) else 0
        val = eok * 1e8 + man * 1e4
        results.append(val)

    # Standalone 만 (only if not already captured by 억+만)
    # We check if the match position overlaps with an 억+만 match
    eok_man_spans = [(m.start(), m.end()) for m in _EOK_MAN.finditer(text)]
    for m in _MAN_ONLY.finditer(text):
        # Skip if this 만 is part of an 억+만 compound
        if any(s <= m.start() < e for s, e in eok_man_spans):
            continue
        results.append(_parse_number(m.group(1)) * 1e4)

    return results


def extract_percentages(text: str) -> list[float]:
    """Extract percentage values. Returns list of the number before %."""
    return [float(m.group(1)) for m in _PERCENT.finditer(text)]


def extract_raw_large_numbers(text: str) -> list[float]:
    """Extract raw large numbers (10+ digits or comma-grouped 3+ groups)."""
    results = []
    for m in _RAW_LARGE.finditer(text):
        results.append(_parse_number(m.group(1)))
    return results


def extract_all_numbers(text: str) -> list[float]:
    """Extract all monetary/numeric values from text."""
    return (
        extract_korean_units(text)
        + extract_raw_large_numbers(text)
    )


# ── Grading logic ────────────────────────────────────────────────────────


@dataclass
class MetricCheck:
    metric: str
    fiscal_year: int
    expected: float
    found: Optional[float]
    unit: str
    tolerance_pct: float
    passed: bool
    error_pct: Optional[float]

    def __str__(self) -> str:
        status = "✅" if self.passed else "❌"
        found_str = f"{self.found:,.0f}" if self.found is not None else "NOT FOUND"
        err_str = f" (오차 {self.error_pct:.1f}%)" if self.error_pct is not None else ""
        return (
            f"  {status} {self.metric} FY{self.fiscal_year}: "
            f"기대값 {self.expected:,.0f} | 답변 {found_str}{err_str}"
        )


def check_metric(
    text: str,
    metric: str,
    fiscal_year: int,
    expected: float,
    unit: str,
    tolerance_pct: float,
) -> MetricCheck:
    """Check if the answer text contains a value matching the expected metric."""

    if unit == "percent":
        # For percent metrics, look for percentages near the fiscal year or metric context
        percents = extract_percentages(text)
        if percents:
            # Find the closest percentage to expected
            best = min(percents, key=lambda p: abs(p - expected))
            err = abs(best - expected) / max(abs(expected), 0.01) * 100
            passed = err <= tolerance_pct
            return MetricCheck(metric, fiscal_year, expected, best, unit, tolerance_pct, passed, err)
        # No percentage found — treat as not found
        return MetricCheck(metric, fiscal_year, expected, None, unit, tolerance_pct, False, None)
    else:
        # For absolute values, look for Korean units or raw large numbers
        numbers = extract_all_numbers(text)
        if numbers:
            best = min(numbers, key=lambda n: abs(n - expected) / max(abs(expected), 1))
            err = abs(best - expected) / abs(expected) * 100
            passed = err <= tolerance_pct
            return MetricCheck(metric, fiscal_year, expected, best, unit, tolerance_pct, passed, err)
        return MetricCheck(metric, fiscal_year, expected, None, unit, tolerance_pct, False, None)


def grade_answer(
    answer_text: str,
    question: dict,
) -> tuple[int, int, list[MetricCheck]]:
    """Grade a single answer against expected metrics.

    Returns (passed_count, total_count, checks).
    """
    checks = []
    for exp in question["expected_metrics"]:
        check = check_metric(
            answer_text,
            exp["metric"],
            exp["fiscal_year"],
            float(exp["value"]),
            exp["unit"],
            float(exp["tolerance_pct"]),
        )
        checks.append(check)

    passed = sum(1 for c in checks if c.passed)
    total = len(checks)
    return passed, total, checks


# ── CLI ──────────────────────────────────────────────────────────────────


def find_question(ground_truth: dict, question_id: str) -> Optional[dict]:
    for q in ground_truth["questions"]:
        if q["id"] == question_id:
            return q
    return None


def match_answer_file_to_question(filepath: str, ground_truth: dict) -> Optional[dict]:
    """Try to match an answer filename like q1_AAPL_easy_*.txt to a question id."""
    name = Path(filepath).name
    # Extract q number from filename
    m = re.match(r"q(\d+)_", name)
    if m:
        qid = f"q{m.group(1)}"
        return find_question(ground_truth, qid)
    return None


def main() -> int:
    parser = argparse.ArgumentParser(description="Grade numeric accuracy of agent answers")
    parser.add_argument("answer_file", nargs="?", help="Path to agent answer text file")
    parser.add_argument("--dir", help="Directory of answer files to grade")
    parser.add_argument(
        "--ground-truth",
        default="evals/numeric-accuracy/ground_truth.json",
        help="Path to ground_truth.json",
    )
    parser.add_argument("--question-id", help="Force question id (e.g. q1)")
    args = parser.parse_args()

    ground_truth_path = Path(args.ground_truth)
    if not ground_truth_path.exists():
        print(f"Ground truth file not found: {ground_truth_path}", file=sys.stderr)
        return 1

    with open(ground_truth_path) as f:
        ground_truth = json.load(f)

    if args.dir:
        # Grade all files in directory
        answer_dir = Path(args.dir)
        files = sorted(answer_dir.glob("q*_*.txt"))
        if not files:
            print(f"No answer files found in {answer_dir}", file=sys.stderr)
            return 1

        total_passed_questions = 0
        total_questions = 0
        total_metrics_passed = 0
        total_metrics = 0

        for fpath in files:
            question = match_answer_file_to_question(str(fpath), ground_truth)
            if not question:
                print(f"[SKIP] {fpath.name} — no matching question")
                continue

            text = fpath.read_text(encoding="utf-8")
            # Check if run completed (state=final)
            if "state=final" not in text:
                print(f"[FAIL] {fpath.name} — run did not complete")
                total_questions += 1
                continue

            passed, total, checks = grade_answer(text, question)
            total_metrics_passed += passed
            total_metrics += total
            total_questions += 1
            if passed == total:
                total_passed_questions += 1

            status = "✅" if passed == total else "⚠️" if passed > 0 else "❌"
            qid = question["id"]
            print(f"[{status}] {qid} ({question['ticker']}) {passed}/{total}")
            for c in checks:
                print(c)

        print(f"\n{'='*60}")
        print(f"정확도: {total_passed_questions}/{total_questions} 질문 완전 통과")
        print(f"메트릭: {total_metrics_passed}/{total_metrics} 개별 메트릭 통과")
        return 0

    # Single file mode
    if not args.answer_file:
        parser.error("answer_file or --dir required")

    answer_path = Path(args.answer_file)
    if not answer_path.exists():
        print(f"Answer file not found: {answer_path}", file=sys.stderr)
        return 1

    text = answer_path.read_text(encoding="utf-8")

    question = None
    if args.question_id:
        question = find_question(ground_truth, args.question_id)
    else:
        question = match_answer_file_to_question(str(answer_path), ground_truth)

    if not question:
        print(f"Could not match answer file to a question. Use --question-id.", file=sys.stderr)
        return 1

    passed, total, checks = grade_answer(text, question)
    print(f"질문: {question['question']}")
    print(f"정확도: {passed}/{total}")
    for c in checks:
        print(c)
    return 0 if passed == total else 1


if __name__ == "__main__":
    sys.exit(main())
