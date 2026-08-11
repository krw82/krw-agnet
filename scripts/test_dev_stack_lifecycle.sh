#!/usr/bin/env bash
set -euo pipefail

# No services are started here. This is a shell contract test for the bounded
# state/lease paths used by the reusable local stack.
krw_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
test_state=$(mktemp -d "${TMPDIR:-/tmp}/krw-agent-stack-test.XXXXXX")
cleanup() { rm -rf "$test_state"; }
trap cleanup EXIT INT TERM

bash -n "$krw_root/scripts/dev-stack.sh"
bash -n "$krw_root/scripts/start_local_agent_gateway_stack.sh"

KRW_AGENT_LOCAL_STATE_DIR="$test_state" \
  KRW_AGENT_LOCAL_GATEWAY_HEALTH_URL="http://127.0.0.1:1/healthz" \
  "$krw_root/scripts/dev-stack.sh" status >/dev/null 2>&1 || true

[[ -f "$test_state/postgres-lifecycle" ]]
[[ "$(<"$test_state/postgres-lifecycle")" == persistent ]]

grep -q 'mkdir "\$krw_lock_dir"' "$krw_root/scripts/dev-stack.sh"
grep -q 'postgres-lifecycle' "$krw_root/scripts/start_local_agent_gateway_stack.sh"
grep -q 'KRW_AGENT_POSTGRES_LIFECYCLE=ephemeral' "$krw_root/scripts/start_local_agent_gateway_stack.sh" && {
  printf 'unexpected literal lifecycle assignment in stack script\n' >&2
  exit 1
} || true

printf 'dev stack lifecycle contract: ok\n'
