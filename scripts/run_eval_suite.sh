#!/usr/bin/env bash
set -euo pipefail

krw_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$krw_root"

# shellcheck disable=SC1091
source .local/agent-gateway/secrets.env
export KRW_AGENT_GATEWAY_TOKEN

GATEWAY="http://127.0.0.1:4318/v1/agent"
RESULTS_DIR=".local/eval-results"
mkdir -p "$RESULTS_DIR"

# 질문 세트: 종목별 + 복잡도 다양화
# 형식: "ticker|난이도|질문"
QUESTIONS=(
  # === 애플 (AAPL) - 대형주, 다양한 복잡도 ===
  "AAPL|easy|애플의 2024년 매출은 얼마야?"
  "AAPL|medium|애플의 2024 회계연도 순이익과 전년 대비 변화를 알려줘."
  "AAPL|hard|애플의 매출총이익률 추이와 그 원인을 분석해줘."

  # === 마이크로소프트 (MSFT) ===
  "MSFT|easy|마이크로소프트 2024년 영업이익은?"
  "MSFT|medium|마이크로소프트의 연구개발비 증가율과 매출 성장의 관계를 설명해줘."
  "MSFT|hard|마이크로소프트의 자산 부채 비율 변화와 재무 건전성을 분석해줘."

  # === 구글/알파벳 (GOOGL) ===
  "GOOGL|easy|알파벳의 2024년 총수익은 얼마야?"
  "GOOGL|medium|알파벳의 영업이익률이 전년 대비 어떻게 변했어?"
  "GOOGL|hard|알파벳의 자본적 지출 증가와 클라우드 성장의 관계를 분석해줘."

  # === 엔비디아 (NVDA) - 고성장주 ===
  "NVDA|easy|엔비디아 2024 회계연도 매출은?"
  "NVDA|medium|엔비디아의 매출 성장률이 얼마나 되고 왜 그런지 설명해줘."
  "NVDA|hard|엔비디아의 순이익률 급증 원인과 지속 가능성을 분석해줘."

  # === 테슬라 (TSLA) - 변동성 높은 주 ===
  "TSLA|easy|테슬라 2024년 매출은 얼마야?"
  "TSLA|medium|테슬라의 영업이익률 변화 추이를 분석해줘."
  "TSLA|hard|테슬라의 자유현금흐름과 재무 투자 전략을 분석해줘."

  # === 아마존 (AMZN) ===
  "AMZN|easy|아마존 2024년 순이익은?"
  "AMZN|medium|아마존의 매출 성장과 영업비용 증가율을 비교해줘."

  # === 메타 (META) ===
  "META|easy|메타 2024년 총수익은?"
  "META|medium|메타의 연구개발 투자와 매출 성장의 관계를 분석해줘."
)

TOTAL=${#QUESTIONS[@]}
PASS=0
FAIL=0
TIMESTAMP=$(date +%Y%m%d_%H%M%S)
SUMMARY_FILE="$RESULTS_DIR/summary_${TIMESTAMP}.txt"

printf '%s\n' "================================================" | tee "$SUMMARY_FILE"
printf '평가 시작: %s (총 %d문항)\n' "$(date)" "$TOTAL" | tee -a "$SUMMARY_FILE"
printf '%s\n' "================================================" | tee -a "$SUMMARY_FILE"

for i in "${!QUESTIONS[@]}"; do
  IFS='|' read -r ticker difficulty question <<< "${QUESTIONS[$i]}"
  num=$((i + 1))
  printf '\n[%02d/%02d] %s | %s | %s\n' "$num" "$TOTAL" "$ticker" "$difficulty" "$question" | tee -a "$SUMMARY_FILE"

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

  # 결과 판정: state=final 이면 PASS, 그 외 FAIL
  if grep -q "state=final" "$OUTPUT_FILE"; then
    STATUS="PASS"
    PASS=$((PASS + 1))
  elif [ $EXIT_CODE -eq 124 ]; then
    STATUS="TIMEOUT"
    FAIL=$((FAIL + 1))
  else
    STATUS="FAIL"
    FAIL=$((FAIL + 1))
  fi

  # 답변 미리보기 (첫 3줄)
  PREVIEW=$(grep -v "^$" "$OUTPUT_FILE" | head -5 | tr '\n' ' ' | cut -c1-150)

  printf '  → %s (%ds)\n' "$STATUS" "$ELAPSED" | tee -a "$SUMMARY_FILE"
  printf '  미리보기: %s...\n' "${PREVIEW:0:120}" | tee -a "$SUMMARY_FILE"

  # 요약 파일에 전체 답변 저장은 별도 파일에 있음
  printf '  결과 파일: %s\n' "$OUTPUT_FILE" | tee -a "$SUMMARY_FILE"
done

printf '\n%s\n' "================================================" | tee -a "$SUMMARY_FILE"
printf '평가 완료: %s\n' "$(date)" | tee -a "$SUMMARY_FILE"
printf '통과: %d / %d  (실패: %d)\n' "$PASS" "$TOTAL" "$FAIL" | tee -a "$SUMMARY_FILE"
printf '%s\n' "================================================" | tee -a "$SUMMARY_FILE"
