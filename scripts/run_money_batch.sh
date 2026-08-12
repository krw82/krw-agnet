#!/usr/bin/env bash
# Batch-submit company_research "how does this company make money" runs to the
# local Agent Gateway and collect each final Markdown answer per ticker.
#
# Target stack: .local/agent-gateway-front-dev (GLM, port 15419). That is the
# only running stack whose worker daemon (PID-paired) actually claims runs; the
# default 4318 gateway has no worker and leaves runs forever "queued".
set -euo pipefail

krw_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
secrets="$krw_root/.local/agent-gateway-front-dev/secrets.env"
# shellcheck disable=SC1090
source "$secrets"

GW="${KRW_BATCH_GATEWAY:-http://127.0.0.1:15419/v1/agent}"
OUT="$krw_root/research_money_how"
MAP="$OUT/_runs.tsv"
TICKERS="${KRW_BATCH_TICKERS:-ABNB AFL ALNY ASTS AVB AWK CCL CRDO DLR FTAI FWONK KHC KMB LYV MAA MRNA NUE O PANW PFG RVMD TRV VRSN}"
POLL_DEADLINE_S="${KRW_BATCH_DEADLINE_S:-2400}"
POLL_INTERVAL="${KRW_BATCH_POLL_INTERVAL:-10}"
QUESTION_TEMPLATE="${KRW_BATCH_QUESTION:-{T}는 어떻게 돈을 버나요? 주요 사업 모델과 매출/수익원, 그리고 수익성 구조를 최근 실적 기준으로 정리해줘}"

mkdir -p "$OUT"
: > "$MAP"

echo "== gateway: $GW"
echo "== question template: $QUESTION_TEMPLATE"
echo "== submitting ${TICKERS} =="

submit_one() {
  local t="$1"
  local q="${QUESTION_TEMPLATE/\{T\}/$t}"
  local body
  body=$(python3 -c "import json,sys;print(json.dumps({'schema_version':1,'question':sys.argv[1],'ticker':sys.argv[2]}))" "$q" "$t")
  local resp rid
  resp=$(curl -s --max-time 30 -X POST "$GW/runs" \
    -H "authorization: Bearer $KRW_AGENT_GATEWAY_TOKEN" \
    -H "content-type: application/json" \
    -d "$body") || resp='{"error":"curl_failed"}'
  rid=$(printf '%s' "$resp" | python3 -c "import sys,json;d=json.load(sys.stdin);print(d.get('run_id') or '')" 2>/dev/null || true)
  if [ -z "$rid" ]; then
    printf '%s\tSUBMIT_FAILED\t%s\n' "$t" "$(printf '%s' "$resp" | tr -d '\n' | head -c 200)" >> "$MAP"
    printf '  [FAIL] %-6s %s\n' "$t" "$(printf '%s' "$resp" | head -c 120)"
    return
  fi
  printf '%s\t%s\n' "$t" "$rid" >> "$MAP"
  printf '  [ok]   %-6s -> %s\n' "$t" "$rid"
}

for t in $TICKERS; do submit_one "$t"; done

echo
echo "== polling until terminal (deadline ${POLL_DEADLINE_S}s, every ${POLL_INTERVAL}s) =="

declare -A done_t
start=$(date +%s)
while true; do
  now=$(date +%s); elapsed=$((now - start))
  remaining=0
  while IFS=$'\t' read -r t rid extra; do
    if [ "$rid" = "SUBMIT_FAILED" ]; then done_t[$t]=submit_failed; continue; fi
    [ -n "${done_t[$t]:-}" ] && continue
    remaining=$((remaining + 1))
    body=$(curl -s --max-time 20 "$GW/runs/$rid" -H "authorization: Bearer $KRW_AGENT_GATEWAY_TOKEN" || echo '{}')
    st=$(printf '%s' "$body" | python3 -c "import sys,json;print(json.load(sys.stdin).get('state','?'))" 2>/dev/null || echo '?')
    case "$st" in
      final)
        printf '%s' "$body" | python3 -c "import sys,json;d=json.load(sys.stdin);fo=d.get('final_output') or {};print(fo.get('markdown','') if isinstance(fo,dict) else '')" > "$OUT/$t.md" 2>/dev/null || true
        done_t[$t]=final
        printf '  [done] %-6s (%ss)\n' "$t" "$elapsed"
        ;;
      failed|cancelled)
        printf '%s' "$body" > "$OUT/$t.error.json"
        done_t[$t]=$st
        printf '  [%s] %-6s (%ss)\n' "$st" "$t" "$elapsed"
        ;;
    esac
  done < "$MAP"
  [ "$remaining" -eq 0 ] && break
  if [ "$elapsed" -ge "$POLL_DEADLINE_S" ]; then
    echo "  deadline reached at ${elapsed}s; $remaining still pending"
    break
  fi
  sleep "$POLL_INTERVAL"
done

echo
echo "== summary =="
ok=0; bad=0; pend=0
while IFS=$'\t' read -r t rid extra; do
  s="${done_t[$t]:-pending}"
  case "$s" in final) ok=$((ok+1));; failed|cancelled|submit_failed) bad=$((bad+1));; *) pend=$((pend+1));; esac
done < "$MAP"
printf 'final=%d  failed=%d  pending=%d\n  out=%s\n  map=%s\n' "$ok" "$bad" "$pend" "$OUT" "$MAP"
