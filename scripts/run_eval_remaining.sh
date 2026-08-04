#!/usr/bin/env bash
set -euo pipefail

krw_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$krw_root"

source .local/agent-gateway/secrets.env
export KRW_AGENT_GATEWAY_TOKEN

GATEWAY="http://127.0.0.1:4318/v1/agent"
RESULTS_DIR=".local/eval-results"
mkdir -p "$RESULTS_DIR"
TIMESTAMP="20260804_173829"

# Q6 부터 재개 (Q1-Q5 완료됨)
QUESTIONS=(
  # Q6
  "MSFT|hard|마이크로소프트의 자산 부채 비율 변화와 재무 건전성을 분석해줘."
  # Q7-9 GOOGL
  "GOOGL|easy|알파벳의 2024년 총수익은 얼마야?"
  "GOOGL|medium|알파벳의 영업이익률이 전년 대비 어떻게 변했어?"
  "GOOGL|hard|알파벳의 자본적 지출 증가와 클라우드 성장의 관계를 분석해줘."
  # Q10-12 NVDA
  "NVDA|easy|엔비디아 2024 회계연도 매출은?"
  "NVDA|medium|엔비디아의 매출 성장률이 얼마나 되고 왜 그런지 설명해줘."
  "NVDA|hard|엔비디아의 순이익률 급증 원인과 지속 가능성을 분석해줘."
  # Q13-15 TSLA
  "TSLA|easy|테슬라 2024년 매출은 얼마야?"
  "TSLA|medium|테슬라의 영업이익률 변화 추이를 분석해줘."
  "TSLA|hard|테슬라의 자유현금흐름과 재무 투자 전략을 분석해줘."
  # Q16-17 AMZN
  "AMZN|easy|아마존 2024년 순이익은?"
  "AMZN|medium|아마존의 매출 성장과 영업비용 증가율을 비교해줘."
  # Q18-19 META
  "META|easy|메타 2024년 총수익은?"
  "META|medium|메타의 연구개발 투자와 매출 성장의 관계를 분석해줘."
)

START_NUM=6
TOTAL=$((START_NUM + ${#QUESTIONS[@]} - 1))

for i in "${!QUESTIONS[@]}"; do
  IFS='|' read -r ticker difficulty question <<< "${QUESTIONS[$i]}"
  num=$((START_NUM + i))
  printf '\n[%02d/%02d] %s | %s | %s\n' "$num" "$TOTAL" "$ticker" "$difficulty" "$question"

  OUTPUT_FILE="$RESULTS_DIR/q${num}_${ticker}_${difficulty}_${TIMESTAMP}.txt"

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
    STATUS="PASS"
  elif [ $EXIT_CODE -eq 124 ]; then
    STATUS="TIMEOUT"
  else
    STATUS="FAIL"
  fi

  PREVIEW=$(grep -v "^$" "$OUTPUT_FILE" | head -3 | tr '\n' ' ' | cut -c1-100)
  printf '  → %s (%ds) %s\n' "$STATUS" "$ELAPSED" "${PREVIEW:0:80}"
done

printf '\n=== Q6-Q19 완료 ===\n'
