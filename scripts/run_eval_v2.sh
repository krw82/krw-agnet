#!/usr/bin/env bash
set -euo pipefail

krw_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$krw_root"

source .local/agent-gateway/secrets.env
export KRW_AGENT_GATEWAY_TOKEN

GATEWAY="http://127.0.0.1:4318/v1/agent"
RESULTS_DIR=".local/eval-results-v2"
mkdir -p "$RESULTS_DIR"
TIMESTAMP=$(date +%Y%m%d_%H%M%S)
SUMMARY_FILE="$RESULTS_DIR/summary_${TIMESTAMP}.txt"

QUESTIONS=(
  "AAPL|easy|q1|애플의 2024년 매출은 얼마야?"
  "AAPL|medium|q2|애플의 2024 회계연도 순이익과 전년 대비 변화를 알려줘."
  "AAPL|hard|q3|애플의 매출총이익률 추이와 그 원인을 분석해줘."
  "MSFT|easy|q4|마이크로소프트 2024년 영업이익은?"
  "MSFT|medium|q5|마이크로소프트의 연구개발비 증가율과 매출 성장의 관계를 설명해줘."
  "MSFT|hard|q6|마이크로소프트의 자산 부채 비율 변화와 재무 건전성을 분석해줘."
  "GOOGL|easy|q7|알파벳의 2024년 총수익은 얼마야?"
  "GOOGL|medium|q8|알파벳의 영업이익률이 전년 대비 어떻게 변했어?"
  "GOOGL|hard|q9|알파벳의 자본적 지출 증가와 클라우드 성장의 관계를 분석해줘."
  "NVDA|easy|q10|엔비디아 2024 회계연도 매출은?"
  "NVDA|medium|q11|엔비디아의 매출 성장률이 얼마나 되고 왜 그런지 설명해줘."
  "NVDA|hard|q12|엔비디아의 순이익률 급증 원인과 지속 가능성을 분석해줘."
  "TSLA|easy|q13|테슬라 2024년 매출은 얼마야?"
  "TSLA|medium|q14|테슬라의 영업이익률 변화 추이를 분석해줘."
  "TSLA|hard|q15|테슬라의 자유현금흐름과 재무 투자 전략을 분석해줘."
  "AMZN|easy|q16|아마존 2024년 순이익은?"
  "AMZN|medium|q17|아마존의 매출 성장과 영업비용 증가율을 비교해줘."
  "META|easy|q18|메타 2024년 총수익은?"
  "META|medium|q19|메타의 연구개발 투자와 매출 성장의 관계를 분석해줘."
)

TOTAL=${#QUESTIONS[@]}
PASS=0
FAIL=0

printf '평가 v2 시작: %s (총 %d문항)\n' "$(date)" "$TOTAL" | tee "$SUMMARY_FILE"

for i in "${!QUESTIONS[@]}"; do
  IFS='|' read -r ticker difficulty qid question <<< "${QUESTIONS[$i]}"
  num=$((i + 1))
  printf '\n[%02d/%02d] %s | %s | %s\n' "$num" "$TOTAL" "$ticker" "$difficulty" "$question" | tee -a "$SUMMARY_FILE"

  OUTPUT_FILE="$RESULTS_DIR/${qid}_${ticker}_${difficulty}_${TIMESTAMP}.txt"

  START_TIME=$(date +%s)
  set +e
  timeout 290 ./scripts/with_local_env.sh ./target/debug/krw-agent run \
    --gateway-url "$GATEWAY" \
    --ticker "$ticker" \
    --question "$question" \
    --wait > "$OUTPUT_FILE" 2>&1
  EXIT_CODE=$?
  set -e
  END_TIME=$(date +%s)
  ELAPSED=$((END_TIME - START_TIME))

  if grep -q "state=final" "$OUTPUT_FILE"; then
    # state=final is the prerequisite. Numeric grading now gates PASS for
    # qids that have expected_metrics in ground_truth.json. If the qid has
    # no metrics (or isn't in ground_truth), state=final alone is enough.
    HAS_METRICS=$(python3 - "$qid" "$krw_root/evals/numeric-accuracy/ground_truth.json" <<'PY' 2>/dev/null || echo "0"
import json, sys
qid, gt_path = sys.argv[1], sys.argv[2]
try:
    with open(gt_path, encoding="utf-8") as f:
        gt = json.load(f)
except Exception:
    print("0"); sys.exit(0)
for q in gt.get("questions", []):
    if q.get("id") == qid:
        print("1" if q.get("expected_metrics") else "0")
        sys.exit(0)
print("0")
PY
)
    if [[ "$HAS_METRICS" == "1" ]]; then
      # Numeric grader gates PASS: exit 0 = all pass, 1 = some fail.
      set +e
      ACC=$(python3 "$krw_root/scripts/grade_numeric_accuracy.py" \
        "$OUTPUT_FILE" --question-id "$qid" \
        --ground-truth "$krw_root/evals/numeric-accuracy/ground_truth.json" 2>&1)
      ACC_EXIT=$?
      set -e
      ACC_LINE=$(echo "$ACC" | grep "^정확도:" || true)
      if [[ -n "$ACC_LINE" ]]; then
        printf '  %s\n' "$ACC_LINE" | tee -a "$SUMMARY_FILE"
      fi
      if [[ $ACC_EXIT -eq 0 ]]; then
        STATUS="PASS"
        PASS=$((PASS + 1))
      else
        STATUS="FAIL"
        FAIL=$((FAIL + 1))
        printf '  사유: numeric_mismatch\n' | tee -a "$SUMMARY_FILE"
      fi
    else
      STATUS="PASS"
      PASS=$((PASS + 1))
    fi
  elif [ $EXIT_CODE -eq 124 ]; then
    STATUS="TIMEOUT"
    FAIL=$((FAIL + 1))
  else
    STATUS="FAIL"
    FAIL=$((FAIL + 1))
  fi

  PREVIEW=$(grep -v "^$" "$OUTPUT_FILE" | head -5 | tr '\n' ' ' | cut -c1-120)
  printf '  → %s (%ds)\n' "$STATUS" "$ELAPSED" | tee -a "$SUMMARY_FILE"
  printf '  미리보기: %s...\n' "${PREVIEW:0:100}" | tee -a "$SUMMARY_FILE"
done

printf '\n%s\n' "================================================" | tee -a "$SUMMARY_FILE"
printf '완료: PASS %d / FAIL %d (총 %d)\n' "$PASS" "$FAIL" "$TOTAL" | tee -a "$SUMMARY_FILE"

# 전체 정확도 채점
printf '\n=== 전체 정확도 채점 ===\n' | tee -a "$SUMMARY_FILE"
python3 "$krw_root/scripts/grade_numeric_accuracy.py" \
  --dir "$RESULTS_DIR" --ground-truth "$krw_root/evals/numeric-accuracy/ground_truth.json" 2>&1 | tee -a "$SUMMARY_FILE"
