#!/usr/bin/env bash
set -euo pipefail

# Observation-layer end-to-end smoke against a real local agent gateway stack.
#
# Boots one isolated `start_local_agent_gateway_stack.sh` stack (PostgreSQL,
# capabilityd, TLS MCP proxy, agentd, host gateway) pointed at the schema-v3
# v2-dev ontology release, then verifies through the stack's TLS MCP boundary:
#
#   1. tools/list serves the 30-tool registry including the new
#      `krw_market_series` / `krw_macro_series` tools, and the readiness
#      document pins the observation tool-schema bundle hash.
#   2. `krw_market_series(ticker=SO, metric=last_price)` returns the clean
#      no-store payload (`status` unavailable/no_data, `advisory_only` true,
#      research-only usage, no vendor identifiers, no exception).
#   3. `krw_macro_series(metric=cpi_yoy)` returns the same clean shape.
#   4. `krw_ontology_query_context` serves a gold-style SearchPlan from the
#      v3 release normally (regression guard for the rest of the registry).
#
# The stack uses its own state directory and pid-offset ports, so it never
# touches the persistent default dev stack, the original repositories, or the
# immutable release tree (served read-only through env, never written).
#
# Environment:
#   KRW_OBSERVATION_E2E_RELEASE_ROOT  target release (default: the v2-dev
#                                     schema-v3 release 20260830_193811)
#   KRW_OBSERVATION_E2E_STATE_DIR     stack state dir (default:
#                                     .local/agent-gateway-observation-e2e)
#   KRW_OBSERVATION_E2E_BOOT_TIMEOUT  readiness budget in seconds (default 300)
#   KRW_AGENT_PROVIDER                provider lane (default glm)
#
# Usage: scripts/test_observation_e2e.sh [--keep-stack]
#   --keep-stack  leave a successful stack running for follow-on quality runs

krw_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
krw_start_script="$krw_root/scripts/start_local_agent_gateway_stack.sh"
krw_release_root=${KRW_OBSERVATION_E2E_RELEASE_ROOT:-"$HOME/krw-ontology-data/releases/v2-dev/dev/20260830_193811"}
krw_state=${KRW_OBSERVATION_E2E_STATE_DIR:-"$krw_root/.local/agent-gateway-observation-e2e"}
krw_boot_timeout=${KRW_OBSERVATION_E2E_BOOT_TIMEOUT:-300}
krw_provider=${KRW_AGENT_PROVIDER:-glm}
krw_keep_stack=0
for krw_arg in "$@"; do
  case "$krw_arg" in
    --keep-stack) krw_keep_stack=1 ;;
    *) printf 'unknown argument: %s\n' "$krw_arg" >&2; exit 2 ;;
  esac
done

# The observation tool surface is a pinned ABI; a mismatch here means the
# capability registry changed without re-pinning the deployment bindings.
readonly krw_expected_tool_schema_sha256=sha256:e0ed0b62b6c7ed98d168fe0a0f03c95099f7a2f2a0e2e13e50aac86e3d6da095
readonly krw_expected_tool_count=30

# Offset ports derived from the test pid keep concurrent runs isolated from
# each other and from the default dev-stack ports (4318/55432/19432/20432).
krw_nonce=$(( $$ % 1000 ))
krw_gateway_port=${KRW_OBSERVATION_E2E_GATEWAY_PORT:-$(( 14318 + krw_nonce ))}
krw_pg_port=${KRW_OBSERVATION_E2E_POSTGRES_PORT:-$(( 56000 + krw_nonce ))}
krw_capability_port=${KRW_OBSERVATION_E2E_CAPABILITY_PORT:-$(( 21500 + krw_nonce ))}
krw_tls_port=${KRW_OBSERVATION_E2E_TLS_PORT:-$(( 22500 + krw_nonce ))}
krw_pid_file="$krw_state/dev-stack.pid"
krw_lock_dir="$krw_state/e2e.lock"
krw_log_file="$krw_state/logs/dev-stack.log"
krw_supervisor_pid=''

fail() {
  printf 'observation e2e: %s\n' "$1" >&2
  exit 1
}

note() {
  printf '[observation-e2e] %s\n' "$1"
}

# --- preflight ---------------------------------------------------------------

[[ -d "$krw_release_root" && -f "$krw_release_root/manifest.json" ]] || {
  fail "release root is not a v3 release directory: $krw_release_root"
}
krw_release_id=$(python3 -c '
import json, sys
manifest = json.load(open(sys.argv[1], encoding="utf-8"))
print(manifest.get("release_id") or "")
' "$krw_release_root/manifest.json")
[[ -n "$krw_release_id" ]] || fail "release manifest has no release_id"
# The schema-v3 dev releases carry manifest env=dev, so the sidecar must be
# admitted with the dev ontology env (no env/current pointer contract).
krw_manifest_env=$(python3 -c '
import json, sys
manifest = json.load(open(sys.argv[1], encoding="utf-8"))
print(manifest.get("env") or "")
' "$krw_release_root/manifest.json")
[[ "$krw_manifest_env" == "dev" ]] || {
  fail "expected a manifest env=dev release (serving contract of this smoke); observed env=$krw_manifest_env"
}
if [[ -f "$krw_release_root/indexes/observations.sqlite" ]]; then
  note "release $krw_release_id carries an observations store; series checks accept available/no_data too"
else
  note "release $krw_release_id has no observations store; series checks expect the clean no-store path"
fi

[[ -x "$krw_root/target/debug/krw-agent" && -x "$krw_root/target/debug/krw-agentd" ]] || {
  fail "missing local Rust binaries; run: scripts/dev-stack.sh prepare"
}
"$krw_root/target/debug/krw-agentd" --help 2>&1 | grep -q -- '--database-tls-mode' || {
  fail "krw-agentd is stale (no --database-tls-mode); run: scripts/dev-stack.sh prepare"
}
[[ -x "$krw_root/packages/host-ts/node_modules/.bin/tsx" ]] || {
  fail "gateway npm dependencies are missing; run: (cd packages/host-ts && npm ci)"
}
for krw_binary in openssl curl uv python3 node npm /opt/homebrew/bin/initdb /opt/homebrew/bin/pg_ctl /opt/homebrew/bin/psql; do
  if [[ "$krw_binary" == /* ]]; then
    [[ -x "$krw_binary" ]] || fail "missing required binary: $krw_binary"
  else
    command -v "$krw_binary" >/dev/null 2>&1 || fail "missing required binary: $krw_binary"
  fi
done

# The agentd endpoint registry resolves the external feed/filings MCP gateways
# and the web news adapter through operator-provided environment NAMES. The
# values never live in this script or the repository; export them in the
# invoking shell (optionally keep the pinned CA as
# $KRW_OBSERVATION_E2E_STATE_DIR/mcp-gateways-ca.pem, which the stack reads).
# KRW_MCP_GATEWAYS_RELEASE_MANIFEST_SHA256 pins the external gateways' own
# release manifest so their bindings do not inherit the ontology data hash.
for krw_env_name in \
  KRW_FEED_MCP_URL KRW_FEED_MCP_READY_URL KRW_FEED_MCP_TOKEN \
  KRW_FILINGS_MCP_URL KRW_FILINGS_MCP_READY_URL KRW_FILINGS_MCP_TOKEN \
  KRW_WEB_NEWS_API_KEY KRW_WEB_NEWS_API_BASE \
  KRW_MCP_GATEWAYS_RELEASE_MANIFEST_SHA256; do
  [[ -n "${!krw_env_name:-}" ]] || fail "export $krw_env_name (external MCP gateway env) before running; values stay in the operator shell"
done

# Ports for this run must be free before boot.
python3 - "$krw_gateway_port" "$krw_pg_port" "$krw_capability_port" "$krw_tls_port" <<'PY'
import socket
import sys

for raw in sys.argv[1:]:
    port = int(raw)
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as probe:
        probe.settimeout(0.5)
        if probe.connect_ex(("127.0.0.1", port)) == 0:
            raise SystemExit(f"port {port} is already in use; pass offset ports via KRW_OBSERVATION_E2E_*_PORT")
PY

# Stale-pid cleanup: reuse nothing, refuse a live stack, drop dead pid files.
mkdir -p "$krw_state" "$krw_state/logs"
chmod 700 "$krw_state" "$krw_state/logs"
if [[ -f "$krw_pid_file" ]]; then
  krw_stale_pid=$(<"$krw_pid_file")
  if [[ "$krw_stale_pid" =~ ^[0-9]+$ ]] && kill -0 "$krw_stale_pid" 2>/dev/null; then
    fail "state dir already has a live stack (pid=$krw_stale_pid); stop it before re-running"
  fi
  rm -f "$krw_pid_file"
fi
if mkdir "$krw_lock_dir" 2>/dev/null; then
  printf '%s\n' $$ >"$krw_lock_dir/pid"
else
  fail "another observation e2e run holds $krw_lock_dir"
fi

teardown() {
  local status=$?
  set +e
  if [[ -n "$krw_supervisor_pid" ]] && kill -0 "$krw_supervisor_pid" 2>/dev/null; then
    kill -TERM "$krw_supervisor_pid" 2>/dev/null
    local index
    for index in $(seq 1 120); do
      kill -0 "$krw_supervisor_pid" 2>/dev/null || break
      sleep 0.5
    done
    kill -KILL "$krw_supervisor_pid" 2>/dev/null
    wait "$krw_supervisor_pid" 2>/dev/null
  fi
  # The supervisor trap tears its children down; sweep only this run's own
  # ports so a leaked listener can never outlive the smoke.
  local port pid
  for port in "$krw_capability_port" "$krw_tls_port" "$krw_gateway_port" "$krw_pg_port"; do
    for pid in $(lsof -nP -t -iTCP:"$port" -sTCP:LISTEN 2>/dev/null); do
      kill -TERM "$pid" 2>/dev/null
    done
  done
  sleep 1
  for port in "$krw_capability_port" "$krw_tls_port" "$krw_gateway_port" "$krw_pg_port"; do
    for pid in $(lsof -nP -t -iTCP:"$port" -sTCP:LISTEN 2>/dev/null); do
      kill -KILL "$pid" 2>/dev/null
    done
  done
  rm -f "$krw_pid_file"
  rm -rf "$krw_lock_dir"
  if (( status == 0 )) && (( krw_keep_stack == 1 )); then
    return 0
  fi
  return 0
}

# Boot the stack in a new session so it survives this script only when
# --keep-stack is requested; otherwise the EXIT trap below stops it.
trap teardown EXIT INT TERM

# --- boot --------------------------------------------------------------------

note "booting stack: release=$krw_release_id state=$krw_state"
note "ports: gateway=$krw_gateway_port postgres=$krw_pg_port capabilityd=$krw_capability_port tls=$krw_tls_port"

export KRW_AGENT_LOCAL_STATE_DIR="$krw_state"
export KRW_AGENT_LOCAL_ONTOLOGY_RELEASE_ROOT="$krw_release_root"
export KRW_ONTOLOGY_ENV=dev
export KRW_AGENT_GATEWAY_PORT="$krw_gateway_port"
export KRW_AGENT_LOCAL_POSTGRES_PORT="$krw_pg_port"
export KRW_AGENT_LOCAL_CAPABILITY_PORT="$krw_capability_port"
export KRW_AGENT_LOCAL_MCP_TLS_PORT="$krw_tls_port"
export KRW_AGENT_POSTGRES_LIFECYCLE=ephemeral
export KRW_AGENT_PROVIDER="$krw_provider"

krw_supervisor_pid=$(python3 - "$krw_start_script" "$krw_state" "$krw_log_file" <<'PY'
import os
import subprocess
import sys

script, state, log_path = sys.argv[1:]
env = os.environ.copy()
env["KRW_AGENT_LOCAL_STATE_DIR"] = state
with open(log_path, "ab", buffering=0) as log:
    child = subprocess.Popen(
        [script],
        stdin=subprocess.DEVNULL,
        stdout=log,
        stderr=subprocess.STDOUT,
        env=env,
        start_new_session=True,
        close_fds=True,
    )
print(child.pid)
PY
)
printf '%s\n' "$krw_supervisor_pid" >"$krw_pid_file"

supervisor_alive() {
  [[ -n "$krw_supervisor_pid" ]] && kill -0 "$krw_supervisor_pid" 2>/dev/null
}

wait_http() {
  local url=$1 label=$2 cacert=${3:-}
  local deadline=$(( $(date +%s) + krw_boot_timeout ))
  while (( $(date +%s) < deadline )); do
    supervisor_alive || {
      printf 'stack supervisor exited while waiting for %s\n' "$label" >&2
      return 1
    }
    if [[ -n "$cacert" ]]; then
      curl --fail --silent --max-time 3 --cacert "$cacert" "$url" >/dev/null 2>&1 && return 0
    else
      curl --fail --silent --max-time 3 "$url" >/dev/null 2>&1 && return 0
    fi
    sleep 1
  done
  return 1
}

note "waiting for capabilityd readiness (budget ${krw_boot_timeout}s)"
wait_http "http://127.0.0.1:$krw_capability_port/healthz" capabilityd || {
  note "capabilityd did not become ready; log tails follow"
  tail -n 40 "$krw_state/logs/capabilityd.log" 2>/dev/null || true
  tail -n 40 "$krw_log_file" 2>/dev/null || true
  fail "capabilityd readiness timeout"
}
note "waiting for TLS MCP proxy readiness"
wait_http "https://127.0.0.1:$krw_tls_port/healthz" tls-proxy "$krw_state/ca.pem" || {
  tail -n 40 "$krw_state/logs/mcp-tls-proxy.log" 2>/dev/null || true
  fail "TLS MCP proxy readiness timeout"
}
note "waiting for gateway readiness (agent images + release sign happen here)"
wait_http "http://127.0.0.1:$krw_gateway_port/healthz" gateway || {
  note "gateway did not become ready; log tails follow"
  tail -n 40 "$krw_state/logs/agentd.log" 2>/dev/null || true
  tail -n 40 "$krw_state/logs/gateway.log" 2>/dev/null || true
  tail -n 40 "$krw_log_file" 2>/dev/null || true
  fail "gateway readiness timeout"
}
note "stack ready (supervisor pid=$krw_supervisor_pid)"

# --- checks ------------------------------------------------------------------

krw_checks_status=0
python3 - "$krw_tls_port" "$krw_state/ca.pem" "$krw_expected_tool_schema_sha256" \
  "$krw_expected_tool_count" "$krw_release_id" \
  >"$krw_state/logs/e2e-checks.log" <<'PY_CHECKS' || krw_checks_status=$?
"""TLS-MCP smoke checks against the stack's capability runtime.

Stdlib only. Prints one PASS/FAIL line per check plus verbatim payload
evidence, and exits non-zero when any check fails.
"""
import json
import ssl
import sys
import urllib.error
import urllib.request

tls_port, ca_path, expected_schema_hash, expected_tool_count, expected_release_id = sys.argv[1:6]
expected_tool_count = int(expected_tool_count)
base_url = f"https://127.0.0.1:{tls_port}"
# The stack pins KRW_ONTOLOGY_MCP_URL to .../mcp/ ; the streamable-HTTP app
# answers POST at the trailing-slash route (POST /mcp is a 307).
mcp_url = base_url + "/mcp/"
ssl_context = ssl.create_default_context(cafile=ca_path)
vendor_tokens = ("fmp", "fred", "polygon")
echo_fields = ("ticker", "canonical_metric")


class TransportError(RuntimeError):
    pass


results = []


def record(name: str, ok: bool, detail: str = "") -> None:
    results.append(ok)
    line = ("PASS" if ok else "FAIL") + f" {name}"
    if detail:
        line += f" :: {detail}"
    print(line, flush=True)


def request(path: str, body: dict | None = None, session_id: str | None = None, timeout: float = 120.0):
    headers = {
        "Accept": "application/json, text/event-stream",
        "Mcp-Protocol-Version": "2025-06-18",
    }
    data = None
    url = mcp_url if body is not None else base_url + path
    if body is not None:
        headers["Content-Type"] = "application/json"
        data = json.dumps(body, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
    if session_id:
        headers["Mcp-Session-Id"] = session_id
    req = urllib.request.Request(url, data=data, headers=headers, method="POST" if body is not None else "GET")
    try:
        with urllib.request.urlopen(req, context=ssl_context, timeout=timeout) as response:
            raw = response.read(16 * 1024 * 1024).decode("utf-8", "replace")
            return response.status, {key.lower(): value for key, value in response.headers.items()}, raw
    except urllib.error.HTTPError as exc:
        raise TransportError(f"HTTP {exc.code} for {url}") from exc
    except (urllib.error.URLError, OSError) as exc:
        raise TransportError(f"transport failure for {url}: {exc}") from exc


def parse_message(raw: str) -> dict:
    for line in raw.splitlines():
        if line.startswith("data: "):
            try:
                value = json.loads(line[6:])
            except json.JSONDecodeError:
                continue
            if isinstance(value, dict):
                return value
    value = json.loads(raw)
    if not isinstance(value, dict):
        raise RuntimeError("MCP response envelope is not an object")
    return value


def structured(message: dict) -> dict | None:
    result = message.get("result")
    if not isinstance(result, dict):
        return None
    payload = result.get("structuredContent")
    if isinstance(payload, dict):
        return payload
    for block in result.get("content", []):
        if isinstance(block, dict) and block.get("type") == "text":
            try:
                value = json.loads(block.get("text", ""))
            except (TypeError, json.JSONDecodeError):
                continue
            if isinstance(value, dict):
                return value
    return None


# Check 1: readiness schema-bundle pin (tool_schema_sha256 + tool_count).
status, _, raw = request("/healthz")
readiness = json.loads(raw)
record(
    "readiness_schema_bundle_hash",
    status == 200
    and readiness.get("ok") is True
    and readiness.get("tool_schema_sha256") == expected_schema_hash
    and readiness.get("tool_count") == expected_tool_count
    and readiness.get("release_manifest_sha256", "").startswith("sha256:"),
    f"tool_schema_sha256={readiness.get('tool_schema_sha256')} tool_count={readiness.get('tool_count')} "
    f"release_manifest_sha256={readiness.get('release_manifest_sha256')}",
)

# Stateless streamable-HTTP MCP session.
try:
    init = {
        "jsonrpc": "2.0",
        "id": "observation-e2e-init",
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "krw-agent-observation-e2e", "version": "1"},
        },
    }
    status, headers, raw = request("", init)
    init_message = parse_message(raw)
    if status != 200 or init_message.get("error") is not None:
        raise TransportError(f"initialize failed: HTTP {status}")
    session_id = headers.get("mcp-session-id")
except TransportError as exc:
    # The remaining checks all depend on the MCP session; record them as
    # explicit failures instead of crashing without evidence.
    for name in (
        "tools_list_observation_tools",
        "market_series_clean_no_store",
        "macro_series_clean_no_store",
        "ontology_query_context_regression",
    ):
        record(name, False, f"session unavailable: {exc}")
    print(f"checks_passed={sum(1 for ok in results if ok)}/{len(results)}", flush=True)
    sys.exit(1)

status, _, raw = request("/mcp", {"jsonrpc": "2.0", "id": "observation-e2e-list", "method": "tools/list", "params": {}}, session_id)
listed = parse_message(raw)
tools = (listed.get("result") or {}).get("tools")
names = sorted(tool.get("name") for tool in tools if isinstance(tool, dict) and isinstance(tool.get("name"), str)) if isinstance(tools, list) else []
record(
    "tools_list_observation_tools",
    status == 200
    and isinstance(tools, list)
    and len(names) == expected_tool_count
    and "krw_market_series" in names
    and "krw_macro_series" in names,
    f"count={len(names)} new_tools={[name for name in names if name in ('krw_market_series', 'krw_macro_series')]}",
)


def call_tool(name: str, arguments: dict) -> dict:
    status, _, raw = request(
        "/mcp",
        {"jsonrpc": "2.0", "id": f"observation-e2e-{name}", "method": "tools/call", "params": {"name": name, "arguments": arguments}},
        session_id,
    )
    message = parse_message(raw)
    result = message.get("result")
    return {
        "http": status,
        "json_rpc_error": message.get("error"),
        "is_error": bool(result.get("isError")) if isinstance(result, dict) else True,
        "payload": structured(message),
    }


def series_check(name: str, call: dict) -> None:
    payload = call.get("payload")
    clean_status = isinstance(payload, dict) and payload.get("status") in {"unavailable", "no_data"}
    advisory = isinstance(payload, dict) and payload.get("advisory_only") is True
    research_only = isinstance(payload, dict) and payload.get("source_usage") == "research_only"
    no_points = isinstance(payload, dict) and payload.get("points") == []
    scanned = {key: value for key, value in (payload or {}).items() if key not in echo_fields}
    canonical = json.dumps(scanned, ensure_ascii=False, sort_keys=True, separators=(",", ":")).lower()
    leaked = [token for token in vendor_tokens if token in canonical]
    no_exception = call.get("http") == 200 and call.get("json_rpc_error") is None and call.get("is_error") is False
    record(
        name,
        bool(no_exception and clean_status and advisory and research_only and no_points and not leaked),
        "payload=" + json.dumps(payload, ensure_ascii=False, sort_keys=True) + (f" leaked={leaked}" if leaked else ""),
    )


series_check(
    "market_series_clean_no_store",
    call_tool("krw_market_series", {"ticker": "SO", "metric": "last_price"}),
)
series_check(
    "macro_series_clean_no_store",
    call_tool("krw_macro_series", {"metric": "cpi_yoy"}),
)

# Check 4: ontology regression — a gold-style SearchPlan served by the same
# v3 release through the same stack (mirrors router_planned_gold_v2 shapes).
gold_style_plan = {
    "question": "Which company discloses both App Store regulation risk and outsourcing delivery risk?",
    "intent": "risk_discovery",
    "universe": "covered",
    "uncertainty": "low",
    "limit_tickers": 20,
    "limit_results": 12,
    "clauses": [
        {
            "clause_id": "regulation",
            "retrieval_query": "App Store regulatory changes sales revenue",
            "required_concepts": ["App Store regulatory changes sales revenue"],
            "required": True,
        },
        {
            "clause_id": "supply",
            "retrieval_query": "outsourcing shipment delays product delivery",
            "required_concepts": ["outsourcing shipment delays product delivery"],
            "required": True,
        },
    ],
}
call = call_tool("krw_ontology_query_context", gold_style_plan)
payload = call.get("payload") or {}
evidence_units = payload.get("evidence_units")
evidence_tickers = sorted(
    {
        unit.get("ticker")
        for unit in evidence_units
        if isinstance(unit, dict) and isinstance(unit.get("ticker"), str)
    }
) if isinstance(evidence_units, list) else []
no_exception = call.get("http") == 200 and call.get("json_rpc_error") is None and call.get("is_error") is False
record(
    "ontology_query_context_regression",
    bool(
        no_exception
        and payload.get("contract_version") == "research-state/v2"
        and payload.get("release_id") == expected_release_id
        and isinstance(evidence_units, list)
        and len(evidence_units) > 0
        and "AAPL" in evidence_tickers
    ),
    f"contract={payload.get('contract_version')} release_id={payload.get('release_id')} "
    f"evidence_units={len(evidence_units) if isinstance(evidence_units, list) else 'missing'} tickers={evidence_tickers}",
)

print(f"checks_passed={sum(1 for ok in results if ok)}/{len(results)}", flush=True)
sys.exit(0 if all(results) else 1)
PY_CHECKS
cat "$krw_state/logs/e2e-checks.log"

# --- evidence ----------------------------------------------------------------

note "capabilityd log tail:"
tail -n 6 "$krw_state/logs/capabilityd.log" 2>/dev/null || true
note "agentd log tail:"
tail -n 6 "$krw_state/logs/agentd.log" 2>/dev/null || true
note "gateway log tail:"
tail -n 6 "$krw_state/logs/gateway.log" 2>/dev/null || true
note "stack boot log tail:"
tail -n 6 "$krw_log_file" 2>/dev/null || true

if (( krw_checks_status != 0 )); then
  fail "observation e2e checks failed (see $krw_state/logs/e2e-checks.log)"
fi

# --- teardown ----------------------------------------------------------------

krw_teardown_status=0
if (( krw_keep_stack == 1 )); then
  note "keeping stack running: supervisor pid=$krw_supervisor_pid state=$krw_state"
  note "stop later with: kill $krw_supervisor_pid"
  # Keep the pid file so a later stop path can find the supervisor; drop only
  # the run lock.
  rm -rf "$krw_lock_dir"
  trap - EXIT INT TERM
else
  note "tearing down the stack"
  krw_keep_stack=0
  teardown
  trap - EXIT INT TERM
  python3 - "$krw_gateway_port" "$krw_pg_port" "$krw_capability_port" "$krw_tls_port" <<'PY' || krw_teardown_status=$?
import socket
import sys

for raw in sys.argv[1:]:
    port = int(raw)
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as probe:
        probe.settimeout(0.5)
        if probe.connect_ex(("127.0.0.1", port)) == 0:
            raise SystemExit(f"port {port} still listening after teardown")
PY
fi
(( krw_teardown_status == 0 )) || fail "stack teardown left a listener behind"

note "observation e2e: all checks passed"
