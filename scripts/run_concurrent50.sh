#!/usr/bin/env bash
# Concurrent 50-question test: enqueue all at once, then poll.
set -uo pipefail

krw_root="${HOME}/krw-agnet"
gateway="http://127.0.0.1:4318"
questions_file="$krw_root/.local/test-questions-50.json"

# The gateway token is the only credential this client needs. Provider keys
# remain confined to the daemon process and are never recovered via `ps`.
token="${KRW_AGENT_GATEWAY_TOKEN:?KRW_AGENT_GATEWAY_TOKEN must be set}"

outdir="$krw_root/.local/concurrent50-$(date +%s)"
mkdir -p "$outdir"
summary="$outdir/summary.txt"

echo "=== Concurrent 50-Question Test ===" | tee "$summary"
echo "Started: $(date)" | tee -a "$summary"
echo "Strategy: enqueue all 50 at once, then poll until all terminal" | tee -a "$summary"
echo "" | tee -a "$summary"

# Step 1: Enqueue all 50 at once
echo "--- Enqueuing all 50 questions ---" | tee -a "$summary"
declare -A RUN_IDS

python3 -c "
import json
qs = json.load(open('$questions_file'))
for q in qs:
    print(f\"{q['id']}|{q['ticker']}|{q['cat']}|{q['q']}|{q['kr']}\")
" | while IFS='|' read -r qid ticker cat question qkr; do
  body=$(python3 -c "import json; print(json.dumps({'schema_version':1,'question':'$qkr','ticker':'$ticker'}))")
  resp=$(curl -s --max-time 10 -X POST "$gateway/v1/agent/runs" \
    -H "Authorization: Bearer $token" -H "Content-Type: application/json" -d "$body" 2>&1)
  run_id=$(echo "$resp" | python3 -c "import sys,json;print(json.load(sys.stdin).get('run_id',''))" 2>/dev/null || echo "")
  echo "$qid|$ticker|$cat|$run_id" >> "$outdir/run_ids.txt"
done

ENQUEUE_END=$(python3 -c "import time;print(time.time())")
RUN_COUNT=$(wc -l < "$outdir/run_ids.txt" | tr -d ' ')
echo "Enqueued $RUN_COUNT runs" | tee -a "$summary"
echo "" | tee -a "$summary"

# Step 2: Poll all until terminal
echo "--- Polling until all terminal (max 5 min) ---" | tee -a "$summary"
POLL_START=$(python3 -c "import time;print(time.time())")

# Initialize status tracking
python3 -c "
import time, json, os, urllib.request

run_ids = {}
for line in open('$outdir/run_ids.txt'):
    parts = line.strip().split('|')
    if len(parts) == 4:
        run_ids[parts[3]] = {'qid': parts[0], 'ticker': parts[1], 'cat': parts[2], 'state': 'unknown'}

token = '$token'
gateway = '$gateway'
outdir = '$outdir'
poll_start = time.time()
max_wait = 900  # 15 minutes (GLM coding plan is slower under concurrency)

while True:
    elapsed = time.time() - poll_start
    pending = 0
    for run_id, info in run_ids.items():
        if info['state'] in ('final', 'failed', 'cancelled'):
            continue
        pending += 1
        try:
            req = urllib.request.Request(
                f'{gateway}/v1/agent/runs/{run_id}',
                headers={'Authorization': f'Bearer {token}'}
            )
            with urllib.request.urlopen(req, timeout=5) as resp:
                data = json.loads(resp.read())
                info['state'] = data.get('state', 'unknown')
                if info['state'] == 'final':
                    fo = data.get('final_output') or {}
                    md = fo.get('markdown', '(no markdown)')
                    with open(f\"{outdir}/{info['qid']}_{info['ticker']}.txt\", 'w') as f:
                        f.write(md)
                    info['e2e'] = time.time() - poll_start
                elif info['state'] in ('failed', 'cancelled'):
                    with open(f\"{outdir}/{info['qid']}_{info['ticker']}.txt\", 'w') as f:
                        f.write('(failed)')
                    info['e2e'] = time.time() - poll_start
        except Exception:
            pass

    done = sum(1 for i in run_ids.values() if i['state'] in ('final','failed','cancelled'))
    print(f'  [{elapsed:.0f}s] done={done}/{len(run_ids)} pending={pending}', flush=True)

    if pending == 0 or elapsed > max_wait:
        break
    time.sleep(5)

# Final report
print()
print('=== RESULTS ===')
total = len(run_ids)
passed = sum(1 for i in run_ids.values() if i['state'] == 'final')
failed = sum(1 for i in run_ids.values() if i['state'] in ('failed','cancelled'))
timeout = sum(1 for i in run_ids.values() if i['state'] not in ('final','failed','cancelled'))
print(f'Total: {total}')
print(f'Pass (final): {passed} ({passed*100//total}%)')
print(f'Fail (failed): {failed} ({failed*100//total}%)')
print(f'Timeout: {timeout} ({timeout*100//total}%)')

# By category
qs = {q['id']: q for q in json.load(open('$questions_file'))}
cats = {}
for info in run_ids.values():
    cat = qs.get(info['qid'], {}).get('cat', '?')
    cats.setdefault(cat, {'pass':0,'fail':0,'timeout':0})
    if info['state'] == 'final':
        cats[cat]['pass'] += 1
    elif info['state'] in ('failed','cancelled'):
        cats[cat]['fail'] += 1
    else:
        cats[cat]['timeout'] += 1

print()
print('By category:')
for cat in sorted(cats):
    c = cats[cat]
    t = c['pass'] + c['fail'] + c['timeout']
    print(f'  {cat:12s}: {c[\"pass\"]}/{t} pass, {c[\"fail\"]} fail, {c[\"timeout\"]} timeout')

# By ticker
tickers = {}
for info in run_ids.values():
    t = info['ticker']
    if t not in tickers:
        tickers[t] = {'pass':0,'fail':0}
    if info['state'] == 'final':
        tickers[t]['pass'] += 1
    else:
        tickers[t]['fail'] += 1

print()
print('By ticker:')
for t in sorted(tickers):
    c = tickers[t]
    print(f'  {t:6s}: {c[\"pass\"]}/{c[\"pass\"]+c[\"fail\"]}')

# Failed questions
failed_qs = [f\"{i['qid']}({i['ticker']}/{i['state']})\" for i in run_ids.values() if i['state'] != 'final']
if failed_qs:
    print(f'\\nNon-final: {\", \".join(sorted(failed_qs))}')

# Timing stats for successful runs
times = [i.get('e2e', 0) for i in run_ids.values() if 'e2e' in i and i['state'] == 'final']
if times:
    times.sort()
    n = len(times)
    print(f'\\nE2E latency (concurrent, from poll start):')
    print(f'  min={times[0]:.1f}s  p50={times[n//2]:.1f}s  p90={times[int(n*0.9)]:.1f}s  max={times[-1]:.1f}s  n={n}')
" 2>&1 | tee -a "$summary"

echo "" | tee -a "$summary"
echo "Finished: $(date)" | tee -a "$summary"
echo "Output dir: $outdir" | tee -a "$summary"
