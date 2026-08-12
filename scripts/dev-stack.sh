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
krw_lock_dir="$krw_state/dev-stack.lock"
krw_postgres_marker="$krw_state/postgres-lifecycle"
krw_log_file="$krw_logs/dev-stack.log"
krw_gateway_port=${KRW_AGENT_GATEWAY_PORT:-4318}
krw_health_url=${KRW_AGENT_LOCAL_GATEWAY_HEALTH_URL:-"http://127.0.0.1:$krw_gateway_port/healthz"}
krw_provider=${KRW_AGENT_PROVIDER:-glm}
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

case "$krw_provider" in
  glm|deepseek) ;;
  *) printf 'KRW_AGENT_PROVIDER must be glm or deepseek\n' >&2; exit 2 ;;
esac

usage() {
  cat >&2 <<'EOF'
usage: scripts/dev-stack.sh <command> [args...]

commands:
  prepare             Build the Rust binaries needed by the local stack.
  up [--skip-prepare] Start the cold stack once, or reuse a ready stack.
  status              Show supervisor and Gateway health without secrets.
  down                Stop only the stack started by this wrapper.
  reload [--skip-prepare]
                      Restart the complete local stack after a code/image change.
  test core [args]    Run run-engine tests without HTTP/Postgres features.
  test adapter [args] Run provider-wire tests without HTTP by default.
  test smoke [args]   Run one case from the existing Gateway quality matrix.
  test quality [args] Run the existing quality matrix against the existing Gateway.
EOF
}

ensure_state() {
  mkdir -p "$krw_state" "$krw_logs"
  chmod 700 "$krw_state" "$krw_logs"
  if [[ ! -f "$krw_postgres_marker" ]]; then
    printf 'persistent\n' >"$krw_postgres_marker"
    chmod 600 "$krw_postgres_marker"
  fi
}

acquire_stack_lock() {
  if mkdir "$krw_lock_dir" 2>/dev/null; then
    printf '%s\n' "$$" >"$krw_lock_dir/pid"
    trap 'rm -rf "$krw_lock_dir"' EXIT
    return 0
  fi
  local lock_pid=''
  [[ -f "$krw_lock_dir/pid" ]] && lock_pid=$(<"$krw_lock_dir/pid") || true
  if [[ -n "$lock_pid" && "$lock_pid" =~ ^[0-9]+$ ]] && kill -0 "$lock_pid" 2>/dev/null; then
    printf 'another dev-stack operation is already running (pid=%s)\n' "$lock_pid" >&2
    return 1
  fi
  rm -rf "$krw_lock_dir"
  mkdir "$krw_lock_dir"
  printf '%s\n' "$$" >"$krw_lock_dir/pid"
  trap 'rm -rf "$krw_lock_dir"' EXIT
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

provider_matches() {
  local pid command
  # macOS `pgrep -af` returns only PIDs on some versions, so obtain the
  # command line through `ps` for each matched daemon instead of treating the
  # pgrep output as portable text.
  while IFS= read -r pid; do
    [[ "$pid" =~ ^[0-9]+$ ]] || continue
    command=$(ps -p "$pid" -o command= 2>/dev/null || true)
    if [[ "$command" == *"--provider $krw_provider"* ]]; then
      return 0
    fi
    # Older stacks predate the explicit selector and always used the
    # canonical GLM registry. Treat that exact compatibility command as GLM
    # only; a DeepSeek selection still requires an explicit restart.
    if [[ "$krw_provider" == glm && "$command" == *"/deployments/local/model-registry.yaml"* ]]; then
      return 0
    fi
  done < <(pgrep -f -- "$krw_root/target/debug/krw-agentd --image-dir" 2>/dev/null || true)
  return 1
}

stack_ready() {
  gateway_ready && agentd_ready && provider_matches
}

binaries_ready() {
  [[ -x "$krw_root/target/debug/krw-agent" && -x "$krw_root/target/debug/krw-agentd" ]] || return 1
  "$krw_root/target/debug/krw-agentd" --help 2>&1 | grep -q -- '--database-tls-mode'
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
  acquire_stack_lock || return 1
  local skip_prepare=0
  local start_arg
  for start_arg in "$@"; do
    case "$start_arg" in
      --skip-prepare) skip_prepare=1 ;;
      '') ;;
      *) printf 'unknown stack option: %s\n' "$start_arg" >&2; return 2 ;;
    esac
  done
  if stack_running; then
    if stack_ready; then
      printf 'dev stack already ready: %s\n' "$krw_health_url"
      return 0
    fi
    if gateway_ready && agentd_ready && ! provider_matches; then
      printf 'dev stack is running with another provider; run down before switching to %s\n' "$krw_provider" >&2
      return 1
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
  if gateway_ready && agentd_ready && provider_matches; then
    printf 'existing externally supervised Gateway is ready: %s\n' "$krw_health_url"
    return 0
  fi
  if gateway_ready && agentd_ready; then
    printf 'external Gateway is running with another provider; stop it before selecting %s\n' "$krw_provider" >&2
    return 1
  fi
  if gateway_ready; then
    printf 'Gateway is up but krw-agentd is absent; stop the stale external stack before retrying\n' >&2
    return 1
  fi

  if [[ -f "$krw_pid_file" ]]; then
    local stale_pid=''
    stale_pid=$(<"$krw_pid_file") || true
    if [[ ! "$stale_pid" =~ ^[0-9]+$ ]] || ! kill -0 "$stale_pid" 2>/dev/null; then
      rm -f "$krw_pid_file"
    fi
  fi
  if (( skip_prepare == 0 )) && [[ "${KRW_AGENT_SKIP_PREPARE:-0}" != 1 ]]; then
    prepare_binaries
  elif ! binaries_ready; then
    printf 'local Rust binaries are missing or stale; run scripts/dev-stack.sh prepare first\n' >&2
    return 1
  fi
  printf 'starting cold dev stack; log=%s\n' "$krw_log_file"
  # A plain background child can remain in the terminal's process group and
  # be reaped when the invoking shell closes. Spawn the supervisor in a new
  # session so `up` can return while the stack remains available for later
  # quality requests. Python is already a required local runtime for the
  # quality runner, so this avoids adding another process-manager dependency.
  local pid
  pid=$(python3 - "$krw_start_script" "$krw_state" "$krw_log_file" <<'PY'
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
    if gateway_ready && agentd_ready && provider_matches; then
      printf 'external dev stack is still serving; leaving it running\n'
    elif gateway_ready && agentd_ready; then
      printf 'external dev stack is serving another provider; leaving it untouched\n'
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
    if gateway_ready && agentd_ready && provider_matches; then
      printf 'gateway=ready (external supervisor) url=%s\n' "$krw_health_url"
      printf 'agentd=ready (external supervisor)\n'
      return 0
    elif gateway_ready && agentd_ready; then
      printf 'gateway=ready but provider differs from selected %s\n' "$krw_provider"
      return 1
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
    load_runtime_pins
  fi
  exec python3 "$krw_root/scripts/run_live_quality_matrix.py" "$@"
}

load_runtime_pins() {
  local runtime_env="$krw_state/frontend.env"
  [[ -f "$runtime_env" ]] || {
    printf 'local stack release pins are missing: %s\n' "$runtime_env" >&2
    return 1
  }
  # The stack writes this file itself. Still parse an explicit allow-list
  # instead of sourcing arbitrary shell text, so quality commands cannot turn
  # a generated runtime artifact into code execution.
  local line key value
  while IFS= read -r line || [[ -n "$line" ]]; do
    [[ -z "$line" || "${line#\#}" != "$line" ]] && continue
    [[ "$line" == *=* ]] || {
      printf 'invalid local stack runtime pin line\n' >&2
      return 1
    }
    key=${line%%=*}
    value=${line#*=}
    case "$key" in
      KRW_AGENT_BACKEND_MODE|KRW_AGENT_ADMISSION_MODE|KRW_RUNTIME_ENVIRONMENT|\
      KRW_AGENT_TENANT_ID|KRW_AGENT_RELEASE_DESCRIPTOR_PATH|\
      KRW_AGENT_RELEASE_ARTIFACT_HASH|KRW_AGENT_RELEASE_SET_HASH|AGENT_V1_DATABASE_URL)
        # Shell variables cannot contain NUL; reject the line delimiters that
        # can still break the generated environment format.
        [[ "$value" != *$'\n'* && "$value" != *$'\r'* ]] || {
          printf 'invalid local stack runtime pin value\n' >&2
          return 1
        }
        export "$key=$value"
        ;;
      KRW_AGENT_PROVIDER)
        # Provider selection is owned by this wrapper's explicit lane, which
        # has already been checked against the running daemon by show_status.
        ;;
      *)
        printf 'unexpected local stack runtime pin key\n' >&2
        return 1
        ;;
    esac
  done < "$runtime_env"
  export KRW_AGENT_PROVIDER="$krw_provider"
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
    load_runtime_pins
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
      if [[ "${1:-}" == "--skip-prepare" ]]; then
        export KRW_AGENT_SKIP_PREPARE=1
        shift
      fi
      stop_stack
      start_stack "$@"
      ;;
    test) run_test "$@" ;;
    help|-h|--help) usage ;;
    *) usage; return 2 ;;
  esac
}

main "$@"
