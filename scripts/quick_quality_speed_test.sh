#!/usr/bin/env bash
# Quick quality + speed test against a running local gateway.
# Sends N questions, measures E2E latency, runs the scoped grader.
set -euo pipefail

krw_root="${HOME}/krw-agnet"
gateway="http://127.0.0.1:4318"
gt_file="$krw_root/evals/numeric-accuracy/ground_truth.json"

# The gateway token is the only credential this client needs. Provider keys
# remain confined to the daemon process and are never recovered via `ps`.
token="${KRW_AGENT_GATEWAY_TOKEN:?KRW_AGENT_GATEWAY_TOKEN must be set}"

# Questions pulled VERBATIM from ground_truth.json to avoid qid mismatch.
QUESTIONS=()
while IFS='|' read -r qid ticker question; do
  QUESTIONS+=("$qid|$ticker|$question")
done < <(python3 -c "
import json
gt = json.load(open('$gt_file'))
# Pick a representative sample: numeric + qualitative + attack
sample = ['q1','q2','q3','q20','q70']
for q in gt['questions']:
    if q['id'] in sample:
        print(f\"{q['id']}|{q['ticker']}|{q['question']}\")
")

outdir="$krw_root/.local/quick-test-$(date +%s)"
mkdir -p "$outdir"
summary="$outdir/summary.txt"
echo "=== Quick Quality + Speed Test ===" | tee "$summary"
echo "Gateway: $gateway" | tee -a "$summary"
echo "Started: $(date)" | tee -a "$summary"
echo "" | tee -a "$summary"

total_pass=0
total_fail=0
declare -a times=()

for entry in "${QUESTIONS[@]}"; do
  IFS='|' read -r qid ticker question <<< "$entry"
  echo "--- $qid ($ticker): $question ---" | tee -a "$summary"

  body=$(python3 -c "import json;print(json.dumps({'schema_version':1,'question':'$question','ticker':'$ticker'}))")
  t0=$(python3 -c "import time;print(time.time())")

  # Enqueue
  enqueue=$(curl -s --max-time 10 -X POST "$gateway/v1/agent/runs" \
    -H "Authorization: Bearer $token" -H "Content-Type: application/json" -d "$body" 2>&1)
  run_id=$(echo "$enqueue" | python3 -c "import sys,json;print(json.load(sys.stdin).get('run_id',''))" 2>/dev/null || echo "")
  if [[ -z "$run_id" ]]; then
    echo "  ENQUEUE FAILED: $enqueue" | tee -a "$summary"
    total_fail=$((total_fail+1))
    continue
  fi

  # Poll (max 280s — under the 290 curl timeout)
  state=""
  final_resp=""
  for i in $(seq 1 56); do
    sleep 5
    resp=$(curl -s --max-time 5 "$gateway/v1/agent/runs/$run_id" -H "Authorization: Bearer $token" 2>&1)
    state=$(echo "$resp" | python3 -c "import sys,json;d=json.load(sys.stdin);print(d.get('state',''))" 2>/dev/null || echo "")
    if [[ "$state" == "final" || "$state" == "failed" || "$state" == "cancelled" ]]; then
      final_resp="$resp"
      break
    fi
  done

  t1=$(python3 -c "import time;print(time.time())")
  elapsed=$(python3 -c "print(f'{$t1-$t0:.1f}')")
  times+=("$elapsed")

  echo "  E2E: ${elapsed}s  state=$state" | tee -a "$summary"

  # Save answer
  ans_file="$outdir/${qid}_${ticker}.txt"
  echo "$final_resp" | python3 -c "
import sys,json
d=json.load(sys.stdin)
fo=d.get('final_output',{}) or {}
print(fo.get('markdown','(no markdown)'))
" > "$ans_file" 2>/dev/null || echo "(parse error)" > "$ans_file"

  # Grade if qid has expected_metrics
  has_metrics=$(python3 -c "
import json
gt=json.load(open('$gt_file'))
qs={q['id']:q for q in gt['questions']}
q=qs.get('$qid',{})
print('1' if q.get('expected_metrics') else '0')
" 2>/dev/null || echo "0")

  if [[ "$has_metrics" == "1" && "$state" == "final" ]]; then
    grade=$(python3 "$krw_root/scripts/grade_numeric_accuracy.py" "$ans_file" \
      --question-id "$qid" --ground-truth "$gt_file" 2>&1) || true
    acc_line=$(echo "$grade" | grep "^정확도:" || echo "정확도: (grader error)")
    echo "  $acc_line" | tee -a "$summary"
    case "$acc_line" in
      *1/1*) total_pass=$((total_pass+1)) ;;
      *) total_fail=$((total_fail+1)) ;;
    esac
  elif [[ "$state" == "final" ]]; then
    echo "  정확도: N/A (qualitative question, PASS on state=final)" | tee -a "$summary"
    total_pass=$((total_pass+1))
  else
    echo "  정확도: FAIL (state=$state)" | tee -a "$summary"
    total_fail=$((total_fail+1))
  fi
  echo "  Answer preview: $(head -c 120 "$ans_file")..." | tee -a "$summary"
  echo "" | tee -a "$summary"
done

# Stats
echo "=== Summary ===" | tee -a "$summary"
echo "PASS: $total_pass / FAIL: $total_fail" | tee -a "$summary"
if [[ ${#times[@]} -gt 0 ]]; then
  python3 -c "
ts=[${times[*]// /, }]
ts.sort()
n=len(ts)
print(f'E2E latency (s): min={ts[0]:.1f} median={ts[n//2]:.1f} max={ts[-1]:.1f} mean={sum(ts)/n:.1f} n={n}')
" | tee -a "$summary"
fi
echo "Output dir: $outdir" | tee -a "$summary"
cat "$summary"
