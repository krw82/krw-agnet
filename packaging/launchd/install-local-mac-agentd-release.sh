#!/usr/bin/env bash
set -euo pipefail

# Install one sealed provider bundle on the local Mac. launchd owns the
# long-lived process while the front and the Rust daemon communicate only
# through the shared PostgreSQL queue. This script never uploads a binary,
# key, or AgentImage to GCP.

usage() {
  cat <<'EOF'
Usage:
  install-local-mac-agentd-release.sh --mode stage \
    --release-root /absolute/sealed-dual-release --release-id ID \
    --provider glm|deepseek

  install-local-mac-agentd-release.sh --mode activate \
    --release-id ID --provider glm|deepseek \
    --env-file /absolute/mac-worker.env [--metrics-port 15520]

  install-local-mac-agentd-release.sh --mode rollback --release-id ID

Optional environment:
  KRW_AGENT_LOCAL_INSTALL_ROOT  (default: ~/.local/share/krw-agent)
EOF
}

fail() {
  printf '%s\n' "$1" >&2
  exit 1
}

valid_release_id() {
  case "$1" in
    ''|*[!A-Za-z0-9._-]*) return 1 ;;
    *) return 0 ;;
  esac
}

valid_provider() {
  case "$1" in glm|deepseek) return 0 ;; *) return 1 ;; esac
}

valid_metrics_port() {
  case "$1" in
    ''|*[!0-9]*) return 1 ;;
    *) [ "$1" -ge 1024 ] && [ "$1" -le 65535 ] ;;
  esac
}

safe_absolute_path() {
  case "$1" in
    /*)
      case "$1" in *$'\n'*|*$'\r'*) return 1 ;; esac
      return 0
      ;;
    *) return 1 ;;
  esac
}

real_directory() {
  safe_absolute_path "$1" && [ -d "$1" ] && [ ! -L "$1" ]
}

real_regular_file() {
  safe_absolute_path "$1" && [ -f "$1" ] && [ ! -L "$1" ]
}

require_command() {
  command -v "$1" >/dev/null 2>&1 || fail "Missing required command: $1"
}

safe_remove_staging() {
  case "$1" in
    "$INSTALL_ROOT"/.staging/*) [ -d "$1" ] && rm -rf -- "$1" ;;
    *) fail "Refusing to remove an unexpected staging directory" ;;
  esac
}

validate_release_root() {
  root=$1
  selected_provider=$2
  real_directory "$root" || fail "Release root is missing or unsafe"
  [ "$(python3 - "$root" <<'PY'
import pathlib, sys
print(pathlib.Path(sys.argv[1]).resolve())
PY
)" != "/" ] || fail "Release root must not be the filesystem root"
  for provider in glm deepseek; do
    bundle="$root/$provider"
    real_directory "$bundle" || fail "Release is missing $provider bundle"
    verifier="$bundle/packaging/verify_standalone_release.py"
    real_regular_file "$verifier" || fail "Release bundle has no standalone verifier"
    python3 "$verifier" --root "$bundle" >/dev/null || fail "Release bundle offline verification failed"
  done
  python3 - "$root" "$selected_provider" <<'PY'
import hashlib
import json
import pathlib
import re
import sys

root = pathlib.Path(sys.argv[1])
selected = sys.argv[2]
providers = {"glm": "glm-5.3", "deepseek": "deepseek-v4-flash"}
digest = re.compile(r"^sha256:[0-9a-f]{64}$")

if {entry.name for entry in root.iterdir()} != {"glm", "deepseek", "dual-release-index.json"}:
    raise SystemExit("dual release root contains unexpected files")

bundles = {}
for provider, model in providers.items():
    bundle = root / provider
    required = (
        "public-release.json",
        "release-authorization.json",
        "release-trust-registry.json",
        "frontend-runtime.env",
        "packaging/launchd/krw-agentd-start-local",
        "packaging/launchd/install-local-mac-agentd-release.sh",
        "packaging/launchd/krw-capabilityd-start-local",
        "packaging/launchd/install-local-mac-capabilityd-release.sh",
        "packaging/local-mcp-gateways/mcp_tls_proxy.mjs",
        "packaging/systemd/krw-agentd-start",
        "capability-runtime/identity.json",
        "capability-runtime/capability-runtime.venv.tar.gz",
    )
    if any(not (bundle / item).is_file() or (bundle / item).is_symlink() for item in required):
        raise SystemExit(f"{provider} bundle is missing a sealed local runtime artifact")
    manifest = json.loads((bundle / "release-manifest.json").read_text(encoding="utf-8"))
    descriptor_path = bundle / "public-release.json"
    descriptor = json.loads(descriptor_path.read_text(encoding="utf-8"))
    if manifest.get("provider_id") != provider or manifest.get("physical_models") != [model]:
        raise SystemExit(f"{provider} manifest provider/model mismatch")
    if descriptor.get("schema_version") != 3 or not isinstance(descriptor.get("entries"), list) or not descriptor["entries"]:
        raise SystemExit(f"{provider} descriptor schema mismatch")
    if any(not isinstance(entry, dict) or entry.get("execution", {}).get("resolved_model") != model for entry in descriptor["entries"]):
        raise SystemExit(f"{provider} descriptor model mismatch")
    descriptor_hash = "sha256:" + hashlib.sha256(descriptor_path.read_bytes()).hexdigest()
    release_hash = descriptor.get("release_set_hash")
    if not isinstance(release_hash, str) or not digest.fullmatch(release_hash):
        raise SystemExit(f"{provider} release-set hash is invalid")
    runtime = (bundle / "frontend-runtime.env").read_text(encoding="utf-8")
    for line in (
        f"KRW_AGENT_PROVIDER={provider}\n",
        "KRW_AGENT_RELEASE_DESCRIPTOR_PATH=/run/krw-agent/public-release.json\n",
        f"KRW_AGENT_RELEASE_ARTIFACT_HASH={descriptor_hash}\n",
        f"KRW_AGENT_RELEASE_SET_HASH={release_hash}\n",
        "KRW_RUNTIME_ENVIRONMENT=prod\n",
    ):
        if line not in runtime:
            raise SystemExit(f"{provider} frontend runtime pin mismatch")
    bundles[provider] = {
        "provider_id": provider,
        "physical_model": model,
        "manifest_hash": manifest.get("manifest_hash"),
        "descriptor_artifact_hash": descriptor_hash,
        "release_set_hash": release_hash,
        "git_commit": manifest.get("git_commit"),
        "git_tree": manifest.get("git_tree"),
    }

if bundles["glm"]["git_commit"] != bundles["deepseek"]["git_commit"] or bundles["glm"]["git_tree"] != bundles["deepseek"]["git_tree"]:
    raise SystemExit("provider bundles do not share one source identity")
index = json.loads((root / "dual-release-index.json").read_text(encoding="utf-8"))
if index.get("schema_version") != 2:
    raise SystemExit("dual release index schema mismatch")
if index.get("git_commit") != bundles[selected]["git_commit"] or index.get("git_tree") != bundles[selected]["git_tree"]:
    raise SystemExit("dual release index source identity mismatch")
for provider, bundle in bundles.items():
    indexed = index.get("bundles", {}).get(provider)
    if not isinstance(indexed, dict) or any(indexed.get(key) != bundle[key] for key in ("provider_id", "physical_model", "manifest_hash", "descriptor_artifact_hash", "release_set_hash")):
        raise SystemExit(f"dual release index {provider} mismatch")
PY
}

write_overlay() {
  overlay=$1
  provider=$2
  temp=$(mktemp "${overlay}.XXXXXX")
  umask 077
  cat > "$temp" <<EOF
KRW_AGENT_CURRENT_DIR=$INSTALL_ROOT/current
KRW_AGENT_PROVIDER=$provider
KRW_AGENT_ARTIFACT_ROOT=$INSTALL_ROOT/artifacts
KRW_AGENT_ARTIFACT_KEY_V1=$ARTIFACT_KEY
KRW_AGENT_WORKER_ID=mac-agentd-$USER_ID
KRW_AGENT_MAX_ACTIVE_RUNS=${KRW_AGENT_MAX_ACTIVE_RUNS:-8}
KRW_AGENT_MCP_POOL_ENTRIES=${KRW_AGENT_MCP_POOL_ENTRIES:-64}
KRW_AGENT_METRICS_BIND=127.0.0.1:$METRICS_PORT
KRW_AGENT_DATABASE_CA_PEM_ENV=KRW_AGENT_DATABASE_CA_PEM
EOF
  chmod 0600 "$temp"
  mv -f "$temp" "$overlay"
}

xml_escape_path() {
  case "$1" in *'&'*|*'<'*|*'>'*|*$'\n'*|*$'\r'*) return 1 ;; esac
  printf '%s' "$1"
}

write_plist() {
  plist=$1
  env_file=$2
  overlay_file=$3
  log_dir=$4
  current_path="$INSTALL_ROOT/current/packaging/launchd/krw-agentd-start-local"
  for value in "$env_file" "$overlay_file" "$log_dir" "$current_path"; do
    xml_escape_path "$value" >/dev/null || fail "Launchd path contains an unsupported XML character"
  done
  temporary=$(mktemp "${plist}.XXXXXX")
  cat > "$temporary" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>$LABEL</string>
  <key>ProgramArguments</key><array>
    <string>/bin/bash</string>
    <string>$current_path</string>
    <string>--env-file</string><string>$env_file</string>
    <string>--overlay-file</string><string>$overlay_file</string>
  </array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
  <key>ThrottleInterval</key><integer>2</integer>
  <key>WorkingDirectory</key><string>$INSTALL_ROOT</string>
  <key>StandardOutPath</key><string>$log_dir/krw-agentd.out.log</string>
  <key>StandardErrorPath</key><string>$log_dir/krw-agentd.err.log</string>
</dict></plist>
EOF
  plutil -lint "$temporary" >/dev/null || { rm -f "$temporary"; fail "Generated launchd plist is invalid"; }
  mv -f "$temporary" "$plist"
}

wait_for_metrics_port() {
  metrics_port=$1
  endpoint="http://127.0.0.1:$metrics_port/metrics"
  attempt=1
  while [ "$attempt" -le 45 ]; do
    if curl -fsS --connect-timeout 1 --max-time 2 "$endpoint" 2>/dev/null | grep -q '^krw_active_runs'; then
      return 0
    fi
    sleep 1
    attempt=$((attempt + 1))
  done
  return 1
}

wait_for_metrics() {
  wait_for_metrics_port "$METRICS_PORT"
}

bootout_label() {
  launchctl bootout "$LAUNCHD_DOMAIN" "$PLIST" >/dev/null 2>&1 || true
}

restore_previous_activation() {
  state_dir=$1
  previous_current="$state_dir/current.previous"
  previous_overlay="$state_dir/overlay.previous"
  previous_plist="$state_dir/plist.previous"
  previous_plist_exists=$(cat "$state_dir/plist.existed" 2>/dev/null || true)
  previous_metrics_port=$METRICS_PORT
  if [ -f "$previous_overlay" ]; then
    previous_metrics_bind=$(awk -F= '$1 == "KRW_AGENT_METRICS_BIND" { print $2; exit }' "$previous_overlay")
    case "$previous_metrics_bind" in
      127.0.0.1:*)
        candidate_port=${previous_metrics_bind#127.0.0.1:}
        if valid_metrics_port "$candidate_port"; then
          previous_metrics_port=$candidate_port
        fi
        ;;
    esac
  fi

  bootout_label
  if [ -f "$previous_current" ]; then
    target=$(cat "$previous_current")
    if [ -n "$target" ]; then
      real_directory "$target" || fail "Previous local release target is unavailable"
      ln -s "$target" "$INSTALL_ROOT/current.next"
      # BSD mv follows a symlink-to-directory unless -h is present. This is
      # the atomic replacement of `current`, not a move into the old bundle.
      mv -h -f "$INSTALL_ROOT/current.next" "$INSTALL_ROOT/current"
    else
      [ ! -L "$INSTALL_ROOT/current" ] || rm -f "$INSTALL_ROOT/current"
    fi
  fi
  if [ -f "$previous_overlay" ]; then
    install -m 0600 "$previous_overlay" "$OVERLAY_FILE"
  else
    rm -f "$OVERLAY_FILE"
  fi
  if [ "$previous_plist_exists" = "1" ] && [ -f "$previous_plist" ]; then
    install -m 0644 "$previous_plist" "$PLIST"
    launchctl bootstrap "$LAUNCHD_DOMAIN" "$PLIST"
    launchctl kickstart -k "$LAUNCHD_DOMAIN/$LABEL"
    wait_for_metrics_port "$previous_metrics_port" || fail "Previous local krw-agentd release did not become ready"
  else
    rm -f "$PLIST"
  fi
}

MODE=''
SOURCE_RELEASE_ROOT=''
RELEASE_ID=''
PROVIDER=''
ENV_FILE=''
METRICS_PORT=${KRW_AGENT_METRICS_PORT:-15520}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --mode|--release-root|--release-id|--provider|--env-file|--metrics-port)
      option=$1
      [[ $# -ge 2 ]] || fail "$option requires a value"
      case "$option" in
        --mode) MODE=$2 ;;
        --release-root) SOURCE_RELEASE_ROOT=$2 ;;
        --release-id) RELEASE_ID=$2 ;;
        --provider) PROVIDER=$2 ;;
        --env-file) ENV_FILE=$2 ;;
        --metrics-port) METRICS_PORT=$2 ;;
      esac
      shift 2
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *) fail "Unknown argument: $1" ;;
  esac
done

case "$MODE" in stage|activate|rollback) ;; *) usage >&2; exit 2 ;; esac
valid_release_id "$RELEASE_ID" || fail "Invalid release id"
valid_metrics_port "$METRICS_PORT" || fail "Metrics port must be in 1024..65535"
case "$MODE" in
  stage|activate) valid_provider "$PROVIDER" || fail "Provider must be glm or deepseek" ;;
esac

INSTALL_ROOT=${KRW_AGENT_LOCAL_INSTALL_ROOT:-"$HOME/.local/share/krw-agent"}
safe_absolute_path "$INSTALL_ROOT" || fail "KRW_AGENT_LOCAL_INSTALL_ROOT must be absolute"
[ "$INSTALL_ROOT" != "/" ] || fail "KRW_AGENT_LOCAL_INSTALL_ROOT must not be the filesystem root"
USER_ID=$(id -u)
LABEL=com.krw.agentd
LAUNCHD_DOMAIN="gui/$USER_ID"
PLIST_DIR="$HOME/Library/LaunchAgents"
PLIST="$PLIST_DIR/$LABEL.plist"
RELEASES_ROOT="$INSTALL_ROOT/releases"
STAGING_ROOT="$INSTALL_ROOT/.staging"
STATE_ROOT="$INSTALL_ROOT/deploy-state"
RUNTIME_ROOT="$INSTALL_ROOT/runtime"
OVERLAY_FILE="$RUNTIME_ROOT/agentd-overlay.env"
ARTIFACT_KEY_FILE="$RUNTIME_ROOT/agentd-artifact-key.env"
LOG_DIR="$HOME/Library/Logs/krw-agent"
TARGET_RELEASE_ROOT="$RELEASES_ROOT/$RELEASE_ID"
STATE_DIR="$STATE_ROOT/$RELEASE_ID"

stage_release() {
  real_directory "$SOURCE_RELEASE_ROOT" || fail "Source release root is missing or unsafe"
  mkdir -p "$RELEASES_ROOT" "$STAGING_ROOT" "$RUNTIME_ROOT" "$LOG_DIR"
  [ ! -e "$TARGET_RELEASE_ROOT" ] || fail "Local release id is already installed"
  stage=$(mktemp -d "$STAGING_ROOT/$RELEASE_ID.XXXXXX")
  cleanup_stage() {
    status=$?
    safe_remove_staging "$stage"
    exit "$status"
  }
  trap cleanup_stage EXIT INT TERM
  cp -R "$SOURCE_RELEASE_ROOT/." "$stage/release"
  validate_release_root "$stage/release" "$PROVIDER"
  for provider in glm deepseek; do
    chmod 0755 \
      "$stage/release/$provider/bin/krw-agent" \
      "$stage/release/$provider/bin/krw-agentd" \
      "$stage/release/$provider/packaging/systemd/krw-agentd-start" \
      "$stage/release/$provider/packaging/launchd/krw-agentd-start-local" \
      "$stage/release/$provider/packaging/launchd/install-local-mac-agentd-release.sh" \
      "$stage/release/$provider/packaging/verify_standalone_release.py"
  done
  mv "$stage/release" "$TARGET_RELEASE_ROOT"
  rmdir "$stage"
  trap - EXIT INT TERM
  printf '%s\n' "Staged local krw-agent release: $RELEASE_ID ($PROVIDER)"
}

activate_release() {
  require_command launchctl
  require_command plutil
  require_command curl
  require_command openssl
  real_directory "$TARGET_RELEASE_ROOT" || fail "Staged local release is unavailable"
  real_regular_file "$ENV_FILE" || fail "Mac worker environment file is missing or unsafe"
  validate_release_root "$TARGET_RELEASE_ROOT" "$PROVIDER"
  bundle="$TARGET_RELEASE_ROOT/$PROVIDER"
  umask 077
  mkdir -p "$PLIST_DIR" "$RUNTIME_ROOT" "$STATE_ROOT" "$LOG_DIR" "$INSTALL_ROOT/artifacts"
  chmod 0700 "$RUNTIME_ROOT" "$STATE_ROOT" "$LOG_DIR" "$INSTALL_ROOT/artifacts"
  [ ! -e "$STATE_DIR" ] || fail "Activation state already exists for this release"
  mkdir -p "$STATE_DIR"
  chmod 0700 "$STATE_DIR"

  if [ -f "$ARTIFACT_KEY_FILE" ]; then
    ARTIFACT_KEY=$(awk -F= '$1 == "KRW_AGENT_ARTIFACT_KEY_V1" { print $2; exit }' "$ARTIFACT_KEY_FILE")
  else
    ARTIFACT_KEY=$(openssl rand -hex 32)
    printf '%s\n' "KRW_AGENT_ARTIFACT_KEY_V1=$ARTIFACT_KEY" > "$ARTIFACT_KEY_FILE"
    chmod 0600 "$ARTIFACT_KEY_FILE"
  fi
  printf '%s' "$ARTIFACT_KEY" | grep -Eq '^[0-9a-f]{64}$' || fail "Local artifact key is invalid"

  if [ -L "$INSTALL_ROOT/current" ]; then
    previous_target=$(python3 - "$INSTALL_ROOT/current" <<'PY'
import pathlib, sys
print(pathlib.Path(sys.argv[1]).resolve())
PY
)
    real_directory "$previous_target" || fail "Existing current release target is unavailable"
  elif [ -e "$INSTALL_ROOT/current" ]; then
    fail "Existing current release path is not a symlink"
  else
    previous_target=''
  fi
  printf '%s\n' "$previous_target" > "$STATE_DIR/current.previous"
  if [ -f "$OVERLAY_FILE" ]; then
    install -m 0600 "$OVERLAY_FILE" "$STATE_DIR/overlay.previous"
  fi
  if [ -f "$PLIST" ]; then
    install -m 0644 "$PLIST" "$STATE_DIR/plist.previous"
    printf '%s\n' 1 > "$STATE_DIR/plist.existed"
  else
    printf '%s\n' 0 > "$STATE_DIR/plist.existed"
  fi
  chmod 0600 "$STATE_DIR"/*

  activation_armed=1
  rollback_activation() {
    status=$?
    if [ "$activation_armed" = "1" ]; then
      activation_armed=0
      set +e
      restore_previous_activation "$STATE_DIR"
    fi
    exit "$status"
  }
  trap rollback_activation EXIT INT TERM

  # Stop the old daemon before changing the symlink, overlay, or plist that it
  # reads. This lets launchd deliver SIGTERM to the old release and prevents a
  # single process from ever observing a mixed release path and environment.
  bootout_label
  write_overlay "$OVERLAY_FILE" "$PROVIDER"
  write_plist "$PLIST" "$ENV_FILE" "$OVERLAY_FILE" "$LOG_DIR"
  ln -s "$bundle" "$INSTALL_ROOT/current.next"
  mv -h -f "$INSTALL_ROOT/current.next" "$INSTALL_ROOT/current"
  launchctl bootstrap "$LAUNCHD_DOMAIN" "$PLIST"
  launchctl enable "$LAUNCHD_DOMAIN/$LABEL" >/dev/null 2>&1 || true
  launchctl kickstart -k "$LAUNCHD_DOMAIN/$LABEL"
  wait_for_metrics || fail "New local krw-agentd release did not become ready"

  activation_armed=0
  trap - EXIT INT TERM
  printf '%s\n' "Activated local krw-agentd release: $RELEASE_ID ($PROVIDER)"
}

rollback_release() {
  require_command launchctl
  require_command curl
  [ -d "$STATE_DIR" ] || fail "No saved activation state exists for this release"
  restore_previous_activation "$STATE_DIR"
  printf '%s\n' "Rolled back local krw-agentd release: $RELEASE_ID"
}

case "$MODE" in
  stage) stage_release ;;
  activate) activate_release ;;
  rollback) rollback_release ;;
esac
