#!/usr/bin/env bash
set -euo pipefail

# Small, persistent development harness.
#
# `start_local_agent_gateway_stack.sh` remains the cold-start implementation.
# This wrapper makes the expensive cold start an explicit, reusable service:
# `up` starts it once, subsequent calls reuse the same Gateway/MCP/agentd
# processes, and `quality` only sends requests through the already-running
# Gateway. No external process manager is required.

krw_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
krw_state=${KRW_AGENT_LOCAL_STATE_DIR:-"$krw_root/.local/agent-gateway"}
krw_logs="$krw_state/logs"
krw_pid_file="$krw_state/dev-stack.pid"
krw_log_file="$krw_logs/dev-stack.log"
krw_gateway_port=${KRW_AGENT_GATEWAY_PORT:-4318}
krw_health_url=${KRW_AGENT_LOCAL_GATEWAY_HEALTH_URL:-"http://127.0.0.1:$krw_gateway_port/healthz"}
krw_start_script="$krw_root/scripts/start_local_agent_gateway_stack.sh"
krw_cargo_bin=${KRW_AGENT_CARGO_BIN:-cargo}
if [[ "$krw_cargo_bin" == cargo ]] && ! command -v cargo >/dev/null 2>&1; then
  if command -v rustup >/dev/null 2>&1; then
    krw_rustup_cargo=$(rustup which cargo 2>/dev/null || true)
    if [[ -n "$krw_rustup_cargo" && -x "$krw_rustup_cargo" ]]; then
      krw_cargo_bin="$krw_rustup_cargo"
    fi
  fi
  if [[ "$krw_cargo_bin" == cargo && -x /opt/homebrew/opt/rustup/bin/cargo ]]; then
    krw_cargo_bin=/opt/homebrew/opt/rustup/bin/cargo
  fi
fi
krw_cargo_dir=$(CDPATH= cd -- "$(dirname -- "$krw_cargo_bin")" 2>/dev/null && pwd || printf '.')

usage() {
  cat >&2 <<'EOF'
usage: scripts/dev-stack.sh <command> [args...]

commands:
  prepare             Build the Rust binaries needed by the local stack.
  up                  Start the cold stack once, or reuse a ready stack.
  status              Show supervisor and Gateway health without secrets.
  down                Stop only the stack started by this wrapper.
  reload              Restart the complete local stack after a code/image change.
  test core [args]    Run run-engine tests without HTTP/Postgres features.
  test adapter [args] Run provider-wire tests without HTTP by default.
  test smoke [args]   Run one case from the existing Gateway quality matrix.
  test quality [args] Run the existing quality matrix against the existing Gateway.
EOF
}

ensure_state() {
  mkdir -p "$krw_state" "$krw_logs"
  chmod 700 "$krw_state" "$krw_logs"
}

read_pid() {
  [[ -f "$krw_pid_file" ]] || return 1
  local pid
  pid=$(<"$krw_pid_file")
  [[ "$pid" =~ ^[0-9]+$ && "$pid" -gt 1 ]] || return 1
  printf '%s\n' "$pid"
}

stack_command_matches() {
  local pid=$1
  local command
  command=$(ps -p "$pid" -o command= 2>/dev/null || true)
  [[ "$command" == *"start_local_agent_gateway_stack.sh"* ]]
}

stack_running() {
  local pid
  pid=$(read_pid) || return 1
  kill -0 "$pid" 2>/dev/null || return 1
  stack_command_matches "$pid"
}

gateway_ready() {
  curl --fail --silent --show-error --max-time 2 "$krw_health_url" >/dev/null 2>&1
}

agentd_ready() {
  # The local daemon is intentionally a direct child of the cold-start
  # script. Match its executable plus one stable argument, not arbitrary user
  # processes or a stale PID file.
  pgrep -f -- "$krw_root/target/debug/krw-agentd --image-dir" >/dev/null 2>&1
}

stack_ready() {
  gateway_ready && agentd_ready
}

wait_for_gateway() {
  local attempts=${1:-120}
  local index
  for index in $(seq 1 "$attempts"); do
    stack_ready && return 0
    stack_running || return 1
    sleep 0.5
  done
  return 1
}

start_stack() {
  ensure_state
  if stack_running; then
    if stack_ready; then
      printf 'dev stack already ready: %s\n' "$krw_health_url"
      return 0
    fi
    if gateway_ready; then
      printf 'Gateway is up but krw-agentd is absent; refusing to reuse a stale stack\n' >&2
      return 1
    fi
    printf 'dev stack is already starting; waiting for Gateway\n'
    if wait_for_gateway 120; then
      printf 'dev stack ready: %s\n' "$krw_health_url"
      return 0
    fi
    printf 'existing dev stack did not become ready; inspect %s\n' "$krw_log_file" >&2
    return 1
  fi

  # A stack started by the older standalone script may not have our PID file.
  # Reuse a healthy Gateway instead of launching a second set of processes on
  # the same ports. It is intentionally treated as externally supervised, so
  # `down` will never terminate it.
  if gateway_ready && agentd_ready; then
    printf 'existing externally supervised Gateway is ready: %s\n' "$krw_health_url"
    return 0
  fi
  if gateway_ready; then
    printf 'Gateway is up but krw-agentd is absent; stop the stale external stack before retrying\n' >&2
    return 1
  fi

  if [[ -f "$krw_pid_file" ]]; then
    rm -f "$krw_pid_file"
  fi
  if [[ "${KRW_AGENT_SKIP_PREPARE:-0}" != 1 ]]; then
    prepare_binaries
  fi
  printf 'starting cold dev stack; log=%s\n' "$krw_log_file"
  nohup env KRW_AGENT_LOCAL_STATE_DIR="$krw_state" \
    "$krw_start_script" >>"$krw_log_file" 2>&1 </dev/null &
  local pid=$!
  printf '%s\n' "$pid" >"$krw_pid_file"
  if wait_for_gateway 240; then
    printf 'dev stack ready: %s\n' "$krw_health_url"
    return 0
  fi
  printf 'dev stack failed to become ready; inspect %s\n' "$krw_log_file" >&2
  tail -n 80 "$krw_log_file" >&2 || true
  return 1
}

stop_stack() {
  ensure_state
  local pid
  pid=$(read_pid) || {
    rm -f "$krw_pid_file"
    if gateway_ready && agentd_ready; then
      printf 'external dev stack is still serving; leaving it running\n'
    elif gateway_ready; then
      printf 'stale external Gateway is still serving; leaving it untouched\n'
    else
      printf 'dev stack is not running\n'
    fi
    return 0
  }
  if ! kill -0 "$pid" 2>/dev/null; then
    rm -f "$krw_pid_file"
    printf 'dev stack process is already gone\n'
    return 0
  fi
  if ! stack_command_matches "$pid"; then
    printf 'refusing to stop unexpected process recorded in %s\n' "$krw_pid_file" >&2
    return 1
  fi
  kill -TERM "$pid"
  local index
  for index in $(seq 1 120); do
    if ! kill -0 "$pid" 2>/dev/null; then
      rm -f "$krw_pid_file"
      printf 'dev stack stopped\n'
      return 0
    fi
    sleep 0.5
  done
  printf 'dev stack did not stop within 60 seconds; leaving it for inspection\n' >&2
  return 1
}

show_status() {
  ensure_state
  local pid=''
  if pid=$(read_pid) && kill -0 "$pid" 2>/dev/null && stack_command_matches "$pid"; then
    printf 'supervisor=running pid=%s\n' "$pid"
    if stack_ready; then
      printf 'gateway=ready url=%s\n' "$krw_health_url"
      printf 'agentd=ready\n'
    else
      if gateway_ready; then
        printf 'gateway=ready agentd=missing_or_stopped url=%s\n' "$krw_health_url"
      else
        printf 'gateway=starting_or_unhealthy url=%s\n' "$krw_health_url"
      fi
      return 1
    fi
  else
    printf 'supervisor=stopped\n'
    if gateway_ready && agentd_ready; then
      printf 'gateway=ready (external supervisor) url=%s\n' "$krw_health_url"
      printf 'agentd=ready (external supervisor)\n'
      return 0
    elif gateway_ready; then
      printf 'gateway=ready but agentd=missing_or_stopped (stale external stack)\n'
      return 1
    fi
    return 1
  fi
}

prepare_binaries() {
  ensure_state
  # Keep this marker in the generated target tree so Spotlight does not treat
  # every compiler artifact as a user document. It is harmless if indexing is
  # already disabled or the directory does not exist yet.
  mkdir -p "$krw_root/target"
  touch "$krw_root/target/.metadata_never_index"
  CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-${KRW_AGENT_BUILD_JOBS:-4}}" \
    PATH="$krw_cargo_dir:$PATH" \
    "$krw_cargo_bin" build --locked \
    -p krw-agent -p krw-agentd
  printf 'Rust binaries prepared\n'
}

run_quality() {
  local arg
  local dry_run=0
  for arg in "$@"; do
    [[ "$arg" == "--dry-run" ]] && dry_run=1
  done
  [[ "${KRW_LIVE_QUALITY_DRY_RUN:-0}" == 1 ]] && dry_run=1
  if [[ "$dry_run" == 0 ]]; then
    show_status >/dev/null
  fi
  exec python3 "$krw_root/scripts/run_live_quality_matrix.py" "$@"
}

run_smoke() {
  local arg
  local dry_run=0
  for arg in "$@"; do
    [[ "$arg" == "--dry-run" ]] && dry_run=1
  done
  [[ "${KRW_LIVE_QUALITY_DRY_RUN:-0}" == 1 ]] && dry_run=1
  if [[ "$dry_run" == 0 ]]; then
    show_status >/dev/null
  fi
  exec python3 "$krw_root/scripts/run_live_quality_matrix.py" \
    --per-bucket 1 --parallelism 1 "$@"
}

run_test() {
  local lane=${1:-}
  shift || true
  case "$lane" in
    core)
      CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-${KRW_AGENT_BUILD_JOBS:-4}}" \
        PATH="$krw_cargo_dir:$PATH" \
        "$krw_cargo_bin" test \
        -p krw-agent-run-engine --no-default-features --locked \
        --lib "$@"
      ;;
    adapter)
      CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-${KRW_AGENT_BUILD_JOBS:-4}}" \
        PATH="$krw_cargo_dir:$PATH" \
        "$krw_cargo_bin" test \
        -p krw-agent-provider-wire --no-default-features --locked \
        --lib "$@"
      ;;
    smoke)
      run_smoke "$@"
      ;;
    quality)
      run_quality "$@"
      ;;
    *)
      printf 'unknown test lane: %s\n' "$lane" >&2
      usage
      return 2
      ;;
  esac
}

main() {
  local command=${1:-}
  shift || true
  case "$command" in
    prepare) prepare_binaries "$@" ;;
    up) start_stack "$@" ;;
    status) show_status "$@" ;;
    down) stop_stack "$@" ;;
    reload)
      stop_stack
      start_stack "$@"
      ;;
    test) run_test "$@" ;;
    help|-h|--help) usage ;;
    *) usage; return 2 ;;
  esac
}

main "$@"
