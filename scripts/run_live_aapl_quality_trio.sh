#!/usr/bin/env bash
set -euo pipefail

# Runs the three AAPL questions shown in the product mock through the real
# acceptance path. This is intentionally a live quality probe, not a
# deterministic CI fixture: every case gets an isolated PostgreSQL/MCP/TLS
# harness and real DeepSeek Flash response.
#
# Required from the caller:
#   KRW_LIVE_EXPECTED_RELEASE_MANIFEST_SHA256=sha256:...
# Optional:
#   KRW_LIVE_QUALITY_CASE=product_mix_margin
#   KRW_LIVE_QUALITY_CORPUS=/absolute/path/to/corpus.json
#   KRW_LIVE_QUALITY_DRY_RUN=1

krw_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
krw_corpus=${KRW_LIVE_QUALITY_CORPUS:-"$krw_root/fixtures/live-quality/v1/aapl-three-question-corpus.json"}
krw_case_filter=${KRW_LIVE_QUALITY_CASE:-}
krw_dry_run=${KRW_LIVE_QUALITY_DRY_RUN:-0}
krw_report_dir=$(mktemp -d /tmp/krw-agent-live-quality.XXXXXX)
umask 077

[[ -f "$krw_corpus" ]] || {
  printf 'live quality corpus is not a regular file: %s\n' "$krw_corpus" >&2
  exit 2
}
[[ "${KRW_LIVE_EXPECTED_RELEASE_MANIFEST_SHA256:-}" =~ ^sha256:[0-9a-f]{64}$ ]] || {
  printf 'set KRW_LIVE_EXPECTED_RELEASE_MANIFEST_SHA256 to the approved immutable release hash\n' >&2
  exit 2
}
if [[ -n "$krw_case_filter" && ! "$krw_case_filter" =~ ^[a-z0-9][a-z0-9_-]{0,63}$ ]]; then
  printf 'KRW_LIVE_QUALITY_CASE is malformed\n' >&2
  exit 2
fi
if [[ "$krw_dry_run" != 0 && "$krw_dry_run" != 1 ]]; then
  printf 'KRW_LIVE_QUALITY_DRY_RUN must be 0 or 1\n' >&2
  exit 2
fi

printf 'live quality reports: %s\n' "$krw_report_dir"

krw_cases_file="$krw_report_dir/cases.tsv"
krw_results_file="$krw_report_dir/results.tsv"
python3 - "$krw_corpus" "$krw_case_filter" <<'PY' >"$krw_cases_file"
import json
import re
import sys

path, requested = sys.argv[1:]
with open(path, encoding="utf-8") as handle:
    corpus = json.load(handle)
if set(corpus) != {"schema_version", "suite_id", "suite_version", "cases"}:
    raise SystemExit("invalid live quality corpus shape")
if corpus["schema_version"] != 1 or not isinstance(corpus["cases"], list):
    raise SystemExit("invalid live quality corpus version")
seen = set()
for case in corpus["cases"]:
    if set(case) != {"case_id", "ticker", "question", "evaluation_focus"}:
        raise SystemExit("invalid live quality case shape")
    case_id = case["case_id"]
    ticker = case["ticker"]
    question = case["question"]
    if not isinstance(case_id, str) or not re.fullmatch(r"[a-z0-9][a-z0-9_-]{0,63}", case_id):
        raise SystemExit("invalid live quality case id")
    if case_id in seen:
        raise SystemExit("duplicate live quality case id")
    seen.add(case_id)
    if not isinstance(ticker, str) or not re.fullmatch(r"[A-Z0-9][A-Z0-9.-]{0,31}", ticker):
        raise SystemExit("invalid live quality ticker")
    if not isinstance(question, str) or not question or "\x00" in question or "\t" in question or "\n" in question:
        raise SystemExit("invalid live quality question")
    if requested and case_id != requested:
        continue
    print(f"{case_id}\t{ticker}\t{question}")
if requested and requested not in seen:
    raise SystemExit("requested live quality case is unknown")
PY
printf 'case_id\tticker\tstatus\telapsed_seconds\n' >"$krw_results_file"
krw_failures=0
krw_completed=0
while IFS=$'\t' read -r krw_case_id krw_ticker krw_question; do
  [[ -n "$krw_case_id" ]] || continue
  krw_run_id="live-quality-${krw_case_id}"
  krw_log="$krw_report_dir/${krw_case_id}.log"
  printf 'running case=%s ticker=%s\n' "$krw_case_id" "$krw_ticker"
  if [[ "$krw_dry_run" == 1 ]]; then
    printf '%s\t%s\tplanned\t0\n' "$krw_case_id" "$krw_ticker" >>"$krw_results_file"
    krw_completed=$((krw_completed + 1))
    continue
  fi
  krw_started=$(date +%s)
  if KRW_LIVE_E2E_RUN_ID="$krw_run_id" \
    KRW_LIVE_E2E_QUESTION="$krw_question" \
    KRW_LIVE_E2E_TICKER="$krw_ticker" \
    "$krw_root/scripts/run_live_e2e_acceptance.sh" 2>&1 | tee "$krw_log"
  then
    krw_status=passed
  else
    krw_status=failed
    krw_failures=$((krw_failures + 1))
  fi
  krw_elapsed=$(( $(date +%s) - krw_started ))
  printf '%s\t%s\t%s\t%s\n' \
    "$krw_case_id" "$krw_ticker" "$krw_status" "$krw_elapsed" >>"$krw_results_file"
  krw_completed=$((krw_completed + 1))
done <"$krw_cases_file"

printf 'live quality trio completed: cases=%s failures=%s; results=%s; redacted logs: %s\n' \
  "$krw_completed" "$krw_failures" "$krw_results_file" "$krw_report_dir"
if [[ "$krw_failures" -ne 0 ]]; then
  exit 1
fi
