#!/usr/bin/env bash
set -euo pipefail

# OpenBB Workspace copilot bridge launcher.
#
# Starts the local workspace-bridge (packages/workspace-bridge-ts) next to the
# already-running dev-stack gateway, then a cloudflared quick tunnel, and prints
# the agents.json URL to paste into OpenBB Workspace's copilot agent settings.
#
# Usage:
#   scripts/workspace-bridge.sh start   # bridge + tunnel, prints registration URL
#   scripts/workspace-bridge.sh stop    # stop both
#   scripts/workspace-bridge.sh status

krw_root=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
krw_state=${KRW_AGENT_LOCAL_STATE_DIR:-"$krw_root/.local/agent-gateway"}
krw_logs="$krw_state/logs"
krw_secrets="$krw_state/secrets.env"
krw_ports="$krw_state/runtime-ports.env"
bridge_pid_file="$krw_state/workspace-bridge.pid"
tunnel_pid_file="$krw_state/workspace-tunnel.pid"
bridge_log="$krw_logs/workspace-bridge.log"
tunnel_log="$krw_logs/workspace-tunnel.log"
secret_file="$krw_state/workspace-bridge.secret"

bridge_port=${KRW_WORKSPACE_BRIDGE_PORT:-14790}
gateway_port=${KRW_AGENT_GATEWAY_PORT:-14318}

command="${1:-}"
[[ "$command" == "start" || "$command" == "stop" || "$command" == "status" ]] || {
  printf 'usage: %s start|stop|status\n' "$0" >&2
  exit 2
}

read_persisted_port() {
  [[ -f "$krw_ports" ]] || return 0
  local line
  while IFS= read -r line || [[ -n "$line" ]]; do
    [[ "$line" == KRW_AGENT_GATEWAY_PORT=* ]] && gateway_port=${line#*=}
  done <"$krw_ports"
  return 0
}

bridge_running() {
  [[ -f "$bridge_pid_file" ]] && kill -0 "$(cat "$bridge_pid_file")" 2>/dev/null
}

tunnel_running() {
  [[ -f "$tunnel_pid_file" ]] && kill -0 "$(cat "$tunnel_pid_file")" 2>/dev/null
}

stop_all() {
  local stopped=0
  if tunnel_running; then
    kill "$(cat "$tunnel_pid_file")" 2>/dev/null || true
    rm -f "$tunnel_pid_file"
    printf 'workspace tunnel stopped\n'
    stopped=1
  fi
  if bridge_running; then
    kill "$(cat "$bridge_pid_file")" 2>/dev/null || true
    rm -f "$bridge_pid_file"
    printf 'workspace bridge stopped\n'
    stopped=1
  fi
  if [[ $stopped -eq 0 ]]; then
    printf 'workspace bridge is not running\n'
  fi
  return 0
}

show_status() {
  if bridge_running; then
    printf 'bridge: running (pid %s, port %s)\n' "$(cat "$bridge_pid_file")" "$bridge_port"
  else
    printf 'bridge: stopped\n'
  fi
  if tunnel_running; then
    printf 'tunnel: running (pid %s)\n' "$(cat "$tunnel_pid_file")"
  else
    printf 'tunnel: stopped\n'
  fi
}

start_all() {
  [[ -f "$krw_secrets" ]] || {
    printf 'dev-stack secrets not found at %s — run scripts/dev-stack.sh reload first\n' "$krw_secrets" >&2
    exit 1
  }
  read_persisted_port
  curl -fsS -m 3 "http://127.0.0.1:$gateway_port/healthz" >/dev/null || {
    printf 'gateway on port %s is not healthy — run scripts/dev-stack.sh reload first\n' "$gateway_port" >&2
    exit 1
  }
  command -v cloudflared >/dev/null || {
    printf 'cloudflared not found — brew install cloudflared\n' >&2
    exit 1
  }

  if bridge_running; then
    printf 'bridge already running (pid %s)\n' "$(cat "$bridge_pid_file")"
  else
    if [[ ! -f "$secret_file" ]]; then
      # Path secret: the tunnel URL plus this unguessable path segment is the
      # demo-grade credential (the Workspace sends no auth headers).
      printf '%s\n' "$(openssl rand -hex 24)" >"$secret_file"
      chmod 600 "$secret_file"
    fi
    mkdir -p "$krw_logs"
    (
      set -a
      # shellcheck disable=SC1090
      source "$krw_secrets"
      set +a
      export KRW_AGENT_GATEWAY_URL="http://127.0.0.1:$gateway_port/v1/agent"
      export KRW_WORKSPACE_BRIDGE_PORT="$bridge_port"
      export KRW_WORKSPACE_BRIDGE_PATH_SECRET="$(cat "$secret_file")"
      cd "$krw_root/packages/workspace-bridge-ts"
      exec npx tsx src/bridge.ts
    ) >>"$bridge_log" 2>&1 &
    echo $! >"$bridge_pid_file"
    local waited=0
    until curl -fsS -m 2 "http://127.0.0.1:$bridge_port/$(cat "$secret_file")/agents.json" >/dev/null 2>&1; do
      ((waited += 1)) || true
      [[ $waited -ge 30 ]] && {
        printf 'bridge failed to become healthy — see %s\n' "$bridge_log" >&2
        stop_all
        exit 1
      }
      sleep 1
    done
    printf 'bridge listening on 127.0.0.1:%s (log %s)\n' "$bridge_port" "$bridge_log"
  fi

  if tunnel_running; then
    printf 'tunnel already running (pid %s)\n' "$(cat "$tunnel_pid_file")"
  else
    : >"$tunnel_log"
    nohup cloudflared tunnel --url "http://127.0.0.1:$bridge_port" --no-autoupdate >>"$tunnel_log" 2>&1 &
    echo $! >"$tunnel_pid_file"
  fi

  local tunnel_url=""
  local waited=0
  while [[ $waited -lt 45 ]]; do
    tunnel_url=$(grep -o 'https://[a-z0-9-]*\.trycloudflare\.com' "$tunnel_log" | head -1 || true)
    [[ -n "$tunnel_url" ]] && break
    ((waited += 1)) || true
    sleep 1
  done
  if [[ -z "$tunnel_url" ]]; then
    printf 'tunnel URL not found — see %s\n' "$tunnel_log" >&2
    stop_all
    exit 1
  fi

  printf '\nOpenBB Workspace 등록 방법:\n'
  printf '  1. OpenBB Workspace 코파일럿 패널 하단 → 에이전트 추가(Add custom agent)\n'
  printf '  2. 아래 URL 붙여넣기:\n'
  printf '     %s/%s/agents.json\n' "$tunnel_url" "$(cat "$secret_file")"
  printf '  3. 대시보드 위젯의 "Add to context" 클릭 후 국문으로 질문 (수 분 소요)\n'
  printf '\n노트: quick tunnel URL은 재시작마다 바뀝니다(재등록 필요). 데모 등급 보안 —\n'
  printf '경로 비밀값+터널 URL이 자격증명입니다. 프로덕션 승격 시 GCP+실츠토큰으로 교체.\n'
}

case "$command" in
  start) start_all ;;
  stop) stop_all ;;
  status) show_status ;;
esac
