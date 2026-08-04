#!/usr/bin/env bash
set -euo pipefail

krw_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$krw_root"

source .local/agent-gateway/secrets.env
export KRW_AGENT_GATEWAY_TOKEN

GATEWAY="http://127.0.0.1:4318/v1/agent"
RESULTS_DIR=".local/eval-results-v2"
TIMESTAMP="20260804_222350"

# Q6 부터 재개
QUESTIONS=(
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

PASS=0
FAIL=0
TOTAL=$((5 + ${#QUESTIONS[@]}))  # Q1-Q5 already done
CUR=5

for i in "${!QUESTIONS[@]}"; do
  IFS='|' read -r ticker difficulty qid question <<< "${QUESTIONS[$i]}"
  CUR=$((CUR + 1))
  printf '\n[%02d/%02d] %s | %s | %s\n' "$CUR" "$TOTAL" "$ticker" "$difficulty" "$question"

  OUTPUT_FILE="$RESULTS_DIR/${qid}_${ticker}_${difficulty}_${TIMESTAMP}.txt"
  START_TIME=$(date +%s)
  set +e
  timeout 290 ./scripts/with_local_env.sh ./target/debug/krw-agent run \
    --gateway-url "$GATEWAY" --ticker "$ticker" --question "$question" \
    --wait > "$OUTPUT_FILE" 2>&1
  EXIT_CODE=$?
  set -e
  END_TIME=$(date +%s)
  ELAPSED=$((END_TIME - START_TIME))

  if grep -q "state=final" "$OUTPUT_FILE"; then
    STATUS="PASS"; PASS=$((PASS + 1))
  elif [ $EXIT_CODE -eq 124 ]; then
    STATUS="TIMEOUT"; FAIL=$((FAIL + 1))
  else
    STATUS="FAIL"; FAIL=$((FAIL + 1))
  fi

  PREVIEW=$(grep -v "^$" "$OUTPUT_FILE" | head -3 | tr '\n' ' ' | cut -c1-100)
  printf '  → %s (%ds) %s\n' "$STATUS" "$ELAPSED" "${PREVIEW:0:80}"

  if [[ "$STATUS" == "PASS" ]]; then
    ACC=$(python3 "$krw_root/scripts/grade_numeric_accuracy.py" \
      "$OUTPUT_FILE" --question-id "$qid" 2>&1)
    ACC_LINE=$(echo "$ACC" | grep "^정확도:" || true)
    [[ -n "$ACC_LINE" ]] && printf '  정확도: %s\n' "$ACC_LINE"
  fi
done

printf '\n=== Q6-Q19 완료: PASS %d FAIL %d ===\n' "$PASS" "$FAIL"
