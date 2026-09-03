#!/usr/bin/env bash
# Run 50-question quality + speed test against local gateway.
set -uo pipefail

krw_root="${HOME}/krw-agnet"
gateway="http://127.0.0.1:4318"
questions_file="$krw_root/.local/test-questions-50.json"
gt_file="$krw_root/evals/numeric-accuracy/ground_truth.json"

# The gateway token is the only credential this client needs. Provider keys
# remain confined to the daemon process and are never recovered via `ps`.
token="${KRW_AGENT_GATEWAY_TOKEN:?KRW_AGENT_GATEWAY_TOKEN must be set}"

outdir="$krw_root/.local/test50-$(date +%s)"
mkdir -p "$outdir"
summary="$outdir/summary.txt"
echo "=== 50-Question Quality + Speed Test ===" | tee "$summary"
echo "Started: $(date)" | tee -a "$summary"
echo "Gateway: $gateway" | tee -a "$summary"
echo "" | tee -a "$summary"

# Read questions and loop
total=50
pass=0
fail=0
declare -a times=()
declare -a cats_pass=()
declare -a cats_fail=()

# Process each question
python3 -c "
import json
qs = json.load(open('$questions_file'))
for q in qs:
    print(f\"{q['id']}|{q['ticker']}|{q['cat']}|{q['q']}|{q['kr']}\")
" | while IFS='|' read -r qid ticker cat question qkr; do
  echo "--- $qid ($ticker/$cat): $qkr ---" | tee -a "$summary"

  # Use Korean question
  body=$(python3 -c "import json; print(json.dumps({'schema_version':1,'question':'$qkr','ticker':'$ticker'}))")
  t0=$(python3 -c "import time;print(time.time())")

  # Enqueue
  enqueue=$(curl -s --max-time 10 -X POST "$gateway/v1/agent/runs" \
    -H "Authorization: Bearer $token" -H "Content-Type: application/json" -d "$body" 2>&1)
  run_id=$(echo "$enqueue" | python3 -c "import sys,json;print(json.load(sys.stdin).get('run_id',''))" 2>/dev/null || echo "")
  if [[ -z "$run_id" ]]; then
    echo "  ENQUEUE FAILED" | tee -a "$summary"
    continue
  fi

  # Poll (max 290s)
  state=""
  for i in $(seq 1 58); do
    sleep 5
    resp=$(curl -s --max-time 5 "$gateway/v1/agent/runs/$run_id" -H "Authorization: Bearer $token" 2>&1)
    state=$(echo "$resp" | python3 -c "import sys,json;d=json.load(sys.stdin);print(d.get('state',''))" 2>/dev/null || echo "")
    if [[ "$state" == "final" || "$state" == "failed" || "$state" == "cancelled" ]]; then
      break
    fi
  done

  t1=$(python3 -c "import time;print(time.time())")
  elapsed=$(python3 -c "print(f'{$t1-$t0:.1f}')")
  echo "  E2E: ${elapsed}s  state=$state" | tee -a "$summary"

  # Save answer
  ans_file="$outdir/${qid}_${ticker}.txt"
  echo "$resp" | python3 -c "
import sys,json
d=json.load(sys.stdin)
fo=d.get('final_output',{}) or {}
md=fo.get('markdown','(no markdown)')
print(md)
" > "$ans_file" 2>/dev/null || echo "(parse error)" > "$ans_file"

  # Record result
  if [[ "$state" == "final" ]]; then
    anslen=$(wc -c < "$ans_file" | tr -d ' ')
    preview=$(head -c 100 "$ans_file" | tr '\n' ' ')
    echo "  PASS (${anslen} chars): ${preview}..." | tee -a "$summary"
  else
    echo "  FAIL (state=$state)" | tee -a "$summary"
  fi
  echo "" | tee -a "$summary"
done

# Stats
echo "=== Computing stats ===" | tee -a "$summary"
python3 -c "
import json, os, glob

outdir = '$outdir'
files = glob.glob(os.path.join(outdir, '*_*.txt'))
results = []
for f in files:
    name = os.path.basename(f).replace('.txt','')
    qid = name.split('_')[0]
    ticker = name.split('_',1)[1]
    content = open(f).read()
    is_fail = '(no markdown)' in content or '(parse error)' in content or len(content) < 20
    results.append({'qid': qid, 'ticker': ticker, 'pass': not is_fail, 'chars': len(content)})

qs = {q['id']: q for q in json.load(open('$questions_file'))}

total = len(results)
passed = sum(1 for r in results if r['pass'])
failed = total - passed

print(f'=== FINAL RESULTS ===')
print(f'Total: {total}')
print(f'Pass: {passed} ({passed*100//total}%)')
print(f'Fail: {failed} ({failed*100//total}%)')
print()

# By category
cats = {}
for r in results:
    cat = qs.get(r['qid'],{}).get('cat','?')
    cats.setdefault(cat, {'pass':0,'fail':0})
    if r['pass']:
        cats[cat]['pass'] += 1
    else:
        cats[cat]['fail'] += 1

print('By category:')
for cat in sorted(cats):
    c = cats[cat]
    t = c['pass'] + c['fail']
    print(f'  {cat:12s}: {c[\"pass\"]}/{t} ({c[\"pass\"]*100//t}%)')

print()

# By ticker
tickers = {}
for r in results:
    t = r['ticker']
    if t not in tickers:
        tickers[t] = {'pass':0,'fail':0}
    if r['pass']:
        tickers[t]['pass'] += 1
    else:
        tickers[t]['fail'] += 1

print('By ticker:')
for t in sorted(tickers):
    c = tickers[t]
    total_t = c['pass'] + c['fail']
    print(f'  {t:6s}: {c[\"pass\"]}/{total_t}')

# Failed questions
failed_qs = [r['qid'] for r in results if not r['pass']]
if failed_qs:
    print(f'\\nFailed questions: {\", \".join(sorted(failed_qs))}')
" 2>&1 | tee -a "$summary"

echo "" | tee -a "$summary"
echo "Output dir: $outdir" | tee -a "$summary"
