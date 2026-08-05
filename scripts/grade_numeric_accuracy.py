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


# ── Metric keyword map for scoped matching ──────────────────────────────

# Maps metric identifiers (as used in ground_truth.json `metric` field) to a list
# of English + Korean keywords that identify the metric in answer text.
# Matching is case-insensitive substring match within a sentence.
# Unknown metrics fall back to an empty keyword list, which makes scope-matching
# fail closed (no keyword → no sentence matches → metric_context_not_found).
METRIC_KEYWORD_MAP: dict[str, list[str]] = {
    "revenue": ["revenue", "sales", "매출", "총수익", "매출액"],
    "operating_income": ["operating income", "operating profit", "영업이익", "영업 이익"],
    "net_income": ["net income", "순이익", "net earnings", "당기순이익"],
    "gross_income": ["gross income", "gross profit", "매출총이익"],
    "eps": ["eps", "earnings per share", "주당순이익", "주당 순이익"],
    "fcf": ["fcf", "free cash flow", "자유현금흐름", "잉여현금흐름", "free cash"],
    "operating_cash_flow": [
        "operating cash flow",
        "영업현금흐름",
        "영업 현금흐름",
        "cash from operations",
        "영업활동 현금흐름",
    ],
    "market_cap": ["market cap", "market capitalization", "시가총액"],
    "debt": ["debt", "부채", "borrowings", "차입금"],
    "total_liabilities": ["total liabilities", "부채총계", "총부채", "liabilities"],
    "cash": ["cash", "현금", "cash and equivalents"],
    "margin": ["margin", "마진", "이익률"],
    "gross_margin": [
        "gross margin",
        "매출총이익률",
        "총이익률",
        "gross profit margin",
    ],
    "operating_margin": ["operating margin", "영업이익률", "영업 마진"],
    "net_margin": ["net margin", "순이익률", "net profit margin"],
    "revenue_growth": [
        "revenue growth",
        "매출 성장",
        "매출 성장률",
        "sales growth",
        "매출증가율",
    ],
    "research_and_development": [
        "research and development",
        "r&d",
        "rnd",
        "연구개발",
        "연구 개발",
        "research & development",
    ],
    "total_assets": ["total assets", "자산총계", "총자산", "assets"],
}


def get_metric_keywords(metric: str) -> list[str]:
    """Return keyword list for a metric id. Empty list if unknown.

    Also tries prefix and underscore-insensitive matches against the map keys
    so e.g. 'net_income_2024' or 'op_income' still resolve.
    """
    if metric in METRIC_KEYWORD_MAP:
        return METRIC_KEYWORD_MAP[metric]
    # Try heuristic substring match against map keys (both directions).
    m_lower = metric.lower()
    for key, kws in METRIC_KEYWORD_MAP.items():
        # Replace underscores/spaces in key for a loose comparison.
        key_norm = key.replace("_", "")
        m_norm = m_lower.replace("_", "")
        if key_norm in m_norm or m_norm in key_norm:
            return kws
    return []


def _split_sentences(text: str) -> list[str]:
    """Split text into sentences using Korean + English boundaries.

    Boundaries: '. ', '다.', '\\n', '! ', '? '. Keeps non-empty stripped
    sentences. Conservative: we deliberately match on the boundary characters
    around the terminator to avoid breaking numbers like '3.5'.
    """
    # Replace boundaries with a sentinel, then split on the sentinel.
    SENTINEL = "\x00"
    s = text
    s = s.replace("\r", "\n")
    # Normalize '다.' end-of-sentence (Korean): match 다 followed by '.' and
    # optional space/newline.
    s = re.sub(r"다\.\s*", "다." + SENTINEL, s)
    # English period + space
    s = re.sub(r"\.\s+", "." + SENTINEL, s)
    # Exclamation / question + space
    s = re.sub(r"[!?]\s+", lambda m: m.group(0)[0] + SENTINEL, s)
    # Newlines
    s = s.replace("\n", SENTINEL)
    parts = [p.strip() for p in s.split(SENTINEL)]
    return [p for p in parts if p]


def _year_patterns(fiscal_year) -> list[str]:
    """Return case-insensitive substring patterns that identify a fiscal year.

    Covers '2024', 'FY2024', '2024년', 'fiscal 2024', '2024 fiscal'.
    The bare 4-digit year is the dominant signal.
    """
    y = str(int(fiscal_year))
    return [
        y,
        f"fy{y}",
        f"fy {y}",
        f"{y}년",
        f"fiscal {y}",
        f"{y} fiscal",
        f"fiscal year {y}",
    ]


def _split_paragraphs(text: str) -> list[str]:
    """Split text into paragraphs on blank lines. Falls back to the whole
    text as one paragraph if no blank-line boundaries exist (common for
    markdown table-heavy answers)."""
    parts = re.split(r"\n\s*\n", text)
    return [p.strip() for p in parts if p.strip()]


def extract_numbers_near(
    text: str,
    metric_keywords: list[str],
    fiscal_year,
) -> list[float]:
    """Extract numbers from regions that mention the metric AND the year.

    Uses two-pass scoping and MERGES results from both passes:
    1. Sentence-level: keyword + year in the SAME sentence (strict).
    2. Paragraph-level: keyword appears anywhere in the paragraph
       AND year appears anywhere in the same paragraph. This catches the
       common pattern of an overview sentence with the keyword followed by
       a data sentence/table row with the year and numbers.
    Both passes run unconditionally — a sentence-level match in one region
    doesn't prevent a paragraph-level match in another region that contains
    the actual target number. Returns deduplicated sorted numbers.
    """
    if not metric_keywords:
        return []
    sentences = _split_sentences(text)
    year_pats = _year_patterns(fiscal_year)
    keyword_pats = [k.lower() for k in metric_keywords]
    collected: list[float] = []

    # Pass 1: strict sentence-level match (keyword + year in same sentence)
    for sent in sentences:
        sl = sent.lower()
        has_keyword = any(k in sl for k in keyword_pats)
        if not has_keyword:
            continue
        has_year = any(p.lower() in sl for p in year_pats)
        if not has_year:
            continue
        collected.extend(extract_korean_units(sent))
        collected.extend(extract_raw_large_numbers(sent))

    # Pass 2: paragraph-level (keyword anywhere + year anywhere in same paragraph)
    for para in _split_paragraphs(text):
        pl = para.lower()
        has_keyword = any(k in pl for k in keyword_pats)
        has_year = any(p.lower() in pl for p in year_pats)
        if not (has_keyword and has_year):
            continue
        collected.extend(extract_korean_units(para))
        collected.extend(extract_raw_large_numbers(para))

    return sorted(set(collected))


def extract_percentages_near(
    text: str,
    metric_keywords: list[str],
    fiscal_year,
) -> list[float]:
    """Extract percentages from regions mentioning the metric AND the year.

    Two-pass scoping (sentence-level + paragraph-level), results MERGED and
    deduplicated. Both passes always run.
    """
    if not metric_keywords:
        return []
    sentences = _split_sentences(text)
    year_pats = _year_patterns(fiscal_year)
    keyword_pats = [k.lower() for k in metric_keywords]
    collected: list[float] = []

    # Pass 1: strict sentence-level
    for sent in sentences:
        sl = sent.lower()
        if not any(k in sl for k in keyword_pats):
            continue
        if not any(p.lower() in sl for p in year_pats):
            continue
        collected.extend(extract_percentages(sent))

    # Pass 2: paragraph-level
    for para in _split_paragraphs(text):
        pl = para.lower()
        has_keyword = any(k in pl for k in keyword_pats)
        has_year = any(p.lower() in pl for p in year_pats)
        if not (has_keyword and has_year):
            continue
        collected.extend(extract_percentages(para))

    return sorted(set(collected))


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
    reason: Optional[str] = None  # short machine code for failures

    def __str__(self) -> str:
        status = "✅" if self.passed else "❌"
        found_str = f"{self.found:,.0f}" if self.found is not None else "NOT FOUND"
        err_str = f" (오차 {self.error_pct:.1f}%)" if self.error_pct is not None else ""
        reason_str = f" [{self.reason}]" if (not self.passed and self.reason) else ""
        return (
            f"  {status} {self.metric} FY{self.fiscal_year}: "
            f"기대값 {self.expected:,.0f} | 답변 {found_str}{err_str}{reason_str}"
        )


def check_metric(
    text: str,
    metric: str,
    fiscal_year: int,
    expected: float,
    unit: str,
    tolerance_pct: float,
) -> MetricCheck:
    """Check if the answer text contains a value matching the expected metric.

    Numbers are scoped to sentences that mention BOTH a metric keyword AND the
    fiscal year. This prevents a number from a different metric or year from
    passing. If no sentence matches, the check fails with
    ``metric_context_not_found`` (it does NOT pass).
    """
    keywords = get_metric_keywords(metric)

    if unit == "percent":
        percents = extract_percentages_near(text, keywords, fiscal_year)
        if percents:
            best = min(percents, key=lambda p: abs(p - expected))
            err = abs(best - expected) / max(abs(expected), 0.01) * 100
            passed = err <= tolerance_pct
            return MetricCheck(
                metric, fiscal_year, expected, best, unit, tolerance_pct,
                passed, err,
                reason=None if passed else "numeric_mismatch",
            )
        # No percentage found in scoped sentences
        return MetricCheck(
            metric, fiscal_year, expected, None, unit, tolerance_pct,
            False, None, reason="metric_context_not_found",
        )
    else:
        numbers = extract_numbers_near(text, keywords, fiscal_year)
        if numbers:
            best = min(numbers, key=lambda n: abs(n - expected) / max(abs(expected), 1))
            err = abs(best - expected) / abs(expected) * 100
            passed = err <= tolerance_pct
            return MetricCheck(
                metric, fiscal_year, expected, best, unit, tolerance_pct,
                passed, err,
                reason=None if passed else "numeric_mismatch",
            )
        return MetricCheck(
            metric, fiscal_year, expected, None, unit, tolerance_pct,
            False, None, reason="metric_context_not_found",
        )


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


def validate_ground_truth(path: str) -> int:
    """Validate ground_truth.json schema. Returns 0 if valid, 1 otherwise.

    Checks:
    - JSON is parseable.
    - Top-level: dict with ``description`` (str) and ``questions`` (list).
    - Each question has ``id`` (str), ``ticker`` (str), ``difficulty`` (str),
      ``question`` (str), ``expected_metrics`` (list).
    - No duplicate question ids.
    - Each expected_metric: ``metric`` (str), ``fiscal_year`` (int),
      ``value`` (number), ``unit`` in {percent, USD, KRW, absolute},
      ``tolerance_pct`` in [0, 100].
    """
    gt_path = Path(path)
    if not gt_path.exists():
        print(f"[validate] file not found: {gt_path}", file=sys.stderr)
        return 1
    try:
        with open(gt_path, encoding="utf-8") as f:
            gt = json.load(f)
    except json.JSONDecodeError as e:
        print(f"[validate] JSON parse error: {e}", file=sys.stderr)
        return 1

    errors: list[str] = []
    if not isinstance(gt, dict):
        errors.append("top-level value must be an object")
        _emit_validate_errors(errors)
        return 1
    if not isinstance(gt.get("description"), str):
        errors.append("missing/invalid 'description' (must be string)")
    if not isinstance(gt.get("questions"), list):
        errors.append("missing/invalid 'questions' (must be array)")
        _emit_validate_errors(errors)
        return 1

    valid_units = {"percent", "absolute", "USD", "KRW"}
    seen_ids: set[str] = set()
    for i, q in enumerate(gt["questions"]):
        ctx = f"questions[{i}]"
        if not isinstance(q, dict):
            errors.append(f"{ctx}: not an object")
            continue
        qid = q.get("id")
        if not isinstance(qid, str) or not qid:
            errors.append(f"{ctx}: missing/invalid 'id'")
        elif qid in seen_ids:
            errors.append(f"{ctx}: duplicate id '{qid}'")
        else:
            seen_ids.add(qid)
        for field in ("ticker", "difficulty", "question"):
            if not isinstance(q.get(field), str):
                errors.append(f"{ctx} ({qid}): missing/invalid '{field}'")
        em = q.get("expected_metrics")
        if not isinstance(em, list):
            errors.append(f"{ctx} ({qid}): 'expected_metrics' must be array")
            continue
        # Empty expected_metrics is allowed: such questions are graded on
        # state=final alone (see scripts/run_eval_v2.sh and evals/README.md).
        # Task 9 introduces qualitative / insufficient-evidence / false-premise
        # / conflict-disclosure categories that intentionally have no numeric
        # expected values.
        for j, m in enumerate(em):
            mctx = f"{ctx}.expected_metrics[{j}] ({qid})"
            if not isinstance(m, dict):
                errors.append(f"{mctx}: not an object")
                continue
            if not isinstance(m.get("metric"), str) or not m["metric"]:
                errors.append(f"{mctx}: missing/invalid 'metric'")
            if not isinstance(m.get("fiscal_year"), int) or isinstance(m.get("fiscal_year"), bool):
                errors.append(f"{mctx}: 'fiscal_year' must be int")
            v = m.get("value")
            if isinstance(v, bool) or not isinstance(v, (int, float)):
                errors.append(f"{mctx}: 'value' must be a number")
            u = m.get("unit")
            if u not in valid_units:
                errors.append(f"{mctx}: 'unit' must be one of {sorted(valid_units)} (got {u!r})")
            t = m.get("tolerance_pct")
            if isinstance(t, bool) or not isinstance(t, (int, float)):
                errors.append(f"{mctx}: 'tolerance_pct' must be a number")
            elif not (0 <= t <= 100):
                errors.append(f"{mctx}: 'tolerance_pct' must be in [0, 100] (got {t})")

    if errors:
        _emit_validate_errors(errors)
        return 1
    n_questions = len(gt["questions"])
    n_metrics = sum(len(q.get("expected_metrics", [])) for q in gt["questions"])
    n_empty = sum(1 for q in gt["questions"] if not q.get("expected_metrics"))
    print(
        f"[validate] OK: {n_questions} questions, "
        f"{n_metrics} metrics, "
        f"{n_empty} qualitative (expected_metrics=[])"
    )
    return 0


def _emit_validate_errors(errors: list[str]) -> None:
    print(f"[validate] FAILED ({len(errors)} error(s))", file=sys.stderr)
    for e in errors:
        print(f"  - {e}", file=sys.stderr)


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
    parser.add_argument(
        "--validate",
        metavar="GROUND_TRUTH_JSON",
        help="Validate the given ground_truth.json schema and exit",
    )
    args = parser.parse_args()

    # ── validate mode ──────────────────────────────────────────────────
    if args.validate:
        return validate_ground_truth(args.validate)

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
