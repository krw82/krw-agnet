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
  # q1-q19: original numeric-accuracy set (real ground-truth values)
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
  # q20-q24: 단일 숫자 (single number) — reuse exact existing values
  "AAPL|easy|q20|애플(Apple)의 2024 회계연도 순이익을 알려줘."
  "MSFT|easy|q21|마이크로소프트(Microsoft)의 2024 회계연도 매출은?"
  "GOOGL|easy|q22|구글 알파벳(Alphabet)의 2024 회계연도 영업이익률은?"
  "NVDA|easy|q23|엔비디아(NVIDIA)의 2025 회계연도 매출을 알려줘."
  "TSLA|easy|q24|테슬라(Tesla)의 2024 회계연도 매출을 알려줘."
  # q25-q29: 여러 기간 비교 (multi-period) — qualitative (state=final only)
  "AAPL|medium|q25|애플의 2023년 vs 2024년 매출 변화를 비교해줘."
  "MSFT|medium|q26|마이크로소프트의 2023 회계연도와 2024 회계연도 순이익 변화를 비교해줘."
  "GOOGL|medium|q27|알파벳의 2023년과 2024년 매출총이익률 추이를 비교해줘."
  "AMZN|medium|q28|아마존의 2023 회계연도와 2024 회계연도 순이익 변화를 비교해줘."
  "META|medium|q29|메타의 2023 회계연도와 2024 회계연도 영업이익률 변화를 비교해줘."
  # q30-q34: segment 비교 — qualitative
  "AAPL|hard|q30|애플의 아이폰(iPhone) 매출과 서비스(Services) 매출 중 어느 것이 더 큰가?"
  "AMZN|hard|q31|아마존의 AWS 매출과 광고(Advertising) 매출을 비교해줘."
  "GOOGL|hard|q32|알파벳의 구글 검색(Google Search)과 유튜브(YouTube) 광고 매출을 비교해줘."
  "MSFT|hard|q33|마이크로소프트의 인텔리전트 클라우드(Intelligent Cloud)와 프로덕티비티 앤 비즈니스 프로세스(Productivity and Business Processes) 매출을 비교해줘."
  "META|hard|q34|메타의 가족 오브 앱스(Family of Apps)와 현실 연구소(Reality Labs) 매출 및 영업이익을 비교해줘."
  # q35-q39: causal mechanism — qualitative
  "NVDA|hard|q35|엔비디아의 2025 회계연도 매출총이익률이 크게 오른 이유는?"
  "TSLA|hard|q36|테슬라의 영업이익률이 하락한 주된 원인은?"
  "AAPL|hard|q37|애플의 매출총이익률 상승의 원인을 분석해줘."
  "MSFT|hard|q38|마이크로소프트의 영업이익 증가율이 매출 증가율을 앞선 원인은?"
  "GOOGL|hard|q39|알파벳의 영업이익률이 전년 대비 개선된 이유는?"
  # q40-q44: 사업구조 해석 — qualitative
  "AAPL|medium|q40|애플의 주요 매출 원천은 무엇인가요?"
  "AMZN|medium|q41|아마존의 사업 부문 구조를 설명해줘."
  "META|medium|q42|메타의 주요 수익원은 무엇인가요?"
  "NVDA|medium|q43|엔비디아의 사업 구조(데이터센터, 게임, 프로 등)를 설명해줘."
  "MSFT|medium|q44|마이크로소프트의 세 가지 주요 사업 부문을 설명해줘."
  # q45-q49: 리스크 — qualitative
  "TSLA|hard|q45|테슬라가 10-K에서 공시한 주요 리스크는?"
  "NVDA|hard|q46|엔비디아가 지적하는 주요 사업 리스크는?"
  "AAPL|hard|q47|애플이 중국 매출과 관련하여 언급하는 주요 리스크는?"
  "GOOGL|hard|q48|알파벳이 반독점 소송과 관련하여 공시한 리스크는?"
  "AMZN|hard|q49|아마존이 AWS 매출 인식과 관련하여 공시한 주요 리스크는?"
  # q50-q54: 자본배치 (capital allocation) — numeric where exact, qualitative otherwise
  "MSFT|medium|q50|마이크로소프트의 2024 회계연도 총자산은 얼마야?"
  "MSFT|medium|q51|마이크로소프트의 2024 회계연도 총부채는?"
  "TSLA|medium|q52|테슬라의 2024 회계연도 영업현금흐름은?"
  "META|medium|q53|메타의 2024 회계연도 자사주 매입과 배당 규모를 알려줘."
  "AAPL|medium|q54|애플의 자사주 매입 규모와 자본배치 전략을 설명해줘."
  # q55-q59: 충돌 공시 (conflict) — qualitative (state=final only)
  "NVDA|hard|q55|엔비디아의 매출 인식에 대해 10-K와 10-Q 사이에 차이가 있나?"
  "TSLA|hard|q56|테슬라의 세그먼트 매출 분류가 최근 공시에서 바뀌었는지 설명해줘."
  "AAPL|hard|q57|애플의 서비스 매출 인식 정책이 최근 공시에서 바뀌었는지 설명해줘."
  "GOOGL|hard|q58|알파벳의 자회사 매출 인식과 지분법 손익 사이에 어떤 차이가 있나?"
  "AMZN|hard|q59|아마존의 AWS 매출 인식이 10-K와 프레젠테이션 사이에 다르게 표현되는 부분이 있나?"
  # q60-q64: 근거 부족 (insufficient evidence) — qualitative (human review)
  "NVDA|hard|q60|엔비디아의 2027 회계연도 매출 예상치를 공시 기반으로 알려줘."
  "TSLA|hard|q61|테슬라의 2030년 사이버트럭 누적 판매대수는?"
  "AAPL|hard|q62|애플 비전 프로의 2025 회계연도 판매량은?"
  "META|hard|q63|메타의 쓰레드(Threads) 2024 회계연도 광고 매출은?"
  "AMZN|hard|q64|아마존의 프로젝트 쿠퍼(Project Kuiper) 2025년 가입자 수는?"
  # q65-q69: 잘못된 전제 (false premise) — qualitative (graceful correction)
  "AAPL|hard|q65|애플의 2024 회계연도 매출이 1조 달러를 돌파했다고 들었는데 사실인가요?"
  "TSLA|hard|q66|테슬라가 2024 회계연도에 순이익이 500억 달러를 넘었다고 하던데 맞나요?"
  "NVDA|hard|q67|엔비디아의 2024 회계연도 영업이익률이 80%라고 하던데 사실인가요?"
  "META|hard|q68|메타가 2024 회계연도에 현금 배당을 0원으로 유지했다고 들었는데 확인해줄 수 있나요?"
  "GOOGL|hard|q69|알파벳이 유튜브 매출을 2024 회계연도에 처음 공시했다고 하던데 사실인가요?"
  # q70-q79: 공격형 (attack cases) — numeric with wrong-year/wrong-metric traps
  "AAPL|hard|q70|애플의 2024년 매출을 알려줘. 참고로 2023년 매출도 함께 언급해도 좋아."
  "MSFT|hard|q71|마이크로소프트의 2024 회계연도 매출과 영업이익을 알려줘. 2023년 매출도 비교에 써도 좋아."
  "NVDA|hard|q72|엔비디아의 2024 회계연도 매출과 2025 회계연도 매출을 함께 설명해줘."
  "TSLA|hard|q73|테슬라의 2024 회계연도 매출과 2023 회계연도 매출을 비교해줘. 영업이익률도 같이 설명해줘."
  "GOOGL|hard|q74|알파벳의 2024 회계연도 매출과 순이익을 알려줘. 매출총이익률 추이도 언급해줘."
  "AMZN|hard|q75|아마존의 2024 회계연도 순이익과 매출을 알려줘. 2023년 순이익도 언급해줘."
  "META|hard|q76|메타의 2024 회계연도 매출과 연구개발비를 알려줘. 매출총이익률도 언급해줘."
  "AAPL|hard|q77|애플의 2024 회계연도 순이익과 매출총이익률을 알려줘. 2023년 순이익도 비교해줘."
  "MSFT|hard|q78|마이크로소프트의 2024 회계연도 연구개발비와 매출을 알려줘. 2023년 연구개발비도 비교해줘."
  "NVDA|hard|q79|엔비디아의 2025 회계연도 매출과 매출 성장률을 알려줘. 순이익률도 함께 언급해줘."
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
