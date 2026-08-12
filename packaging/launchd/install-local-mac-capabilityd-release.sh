#!/usr/bin/env bash
set -euo pipefail

# Activate the provider-independent canonical capability runtime that travels
# inside a sealed Agent release.  It owns the ontology/Guru/market tool
# surface, while the Rust daemon remains the sole owner of model execution and
# the Supabase queue.

usage() {
  printf '%s\n' 'usage: install-local-mac-capabilityd-release.sh --mode activate|rollback --release-id ID --provider glm|deepseek --operator-root /absolute/krw-agent-prod'
}

fail() { printf '%s\n' "$1" >&2; exit 1; }

MODE=''
RELEASE_ID=''
PROVIDER=''
OPERATOR_ROOT=''
while [[ $# -gt 0 ]]; do
  case "$1" in
    --mode|--release-id|--provider|--operator-root)
      [[ $# -ge 2 ]] || fail "$1 requires a value"
      case "$1" in
        --mode) MODE=$2 ;;
        --release-id) RELEASE_ID=$2 ;;
        --provider) PROVIDER=$2 ;;
        --operator-root) OPERATOR_ROOT=$2 ;;
      esac
      shift 2
      ;;
    --help|-h) usage; exit 0 ;;
    *) usage >&2; exit 2 ;;
  esac
done

case "$MODE" in activate|rollback) ;; *) usage >&2; exit 2 ;; esac
case "$RELEASE_ID" in ''|*[!A-Za-z0-9._-]*) fail 'Invalid release id' ;; esac
case "$PROVIDER" in glm|deepseek) ;; *) fail 'Provider must be glm or deepseek' ;; esac
[[ "$OPERATOR_ROOT" == /* && -d "$OPERATOR_ROOT" && ! -L "$OPERATOR_ROOT" ]] || fail 'Operator root is missing or unsafe'

INSTALL_ROOT=${KRW_AGENT_LOCAL_INSTALL_ROOT:-"$HOME/.local/share/krw-agent"}
[[ "$INSTALL_ROOT" == /* && "$INSTALL_ROOT" != / ]] || fail 'Local install root must be a safe absolute path'
USER_ID=$(id -u)
DOMAIN="gui/$USER_ID"
LABEL=com.krw.capabilityd
PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"
LOG_DIR="$HOME/Library/Logs/krw-agent"
STATE_DIR="$INSTALL_ROOT/deploy-state/capabilityd-$RELEASE_ID"
CURRENT="$INSTALL_ROOT/current"
CAPABILITY_ENV="$OPERATOR_ROOT/runtime/capabilityd.env"
GATEWAY_CONFIG="$OPERATOR_ROOT/config/runtime/ontology-tls.json"

[[ -f "$CAPABILITY_ENV" && ! -L "$CAPABILITY_ENV" ]] || fail 'Capability runtime environment is missing or unsafe'
[[ -f "$GATEWAY_CONFIG" && ! -L "$GATEWAY_CONFIG" ]] || fail 'Ontology gateway config is missing or unsafe'

read_setting() {
  python3 - "$CAPABILITY_ENV" "$1" <<'PY'
import pathlib, sys
path = pathlib.Path(sys.argv[1])
wanted = sys.argv[2]
values = {}
for raw in path.read_text(encoding="utf-8").splitlines():
    if not raw or raw.lstrip().startswith("#"):
        continue
    if "=" not in raw:
        raise SystemExit(1)
    key, value = raw.split("=", 1)
    values[key] = value
value = values.get(wanted, "")
if "\n" in value or "\r" in value:
    raise SystemExit(1)
print(value, end="")
PY
}

CAPABILITY_PORT=$(read_setting KRW_CAPABILITYD_PORT)
CAPABILITY_PORT=${CAPABILITY_PORT:-19432}
case "$CAPABILITY_PORT" in ''|*[!0-9]*) fail 'Capability runtime port is invalid' ;; esac
[ "$CAPABILITY_PORT" -ge 1024 ] && [ "$CAPABILITY_PORT" -le 65535 ] || fail 'Capability runtime port is invalid'

capture_current_bundle() {
  [[ -L "$CURRENT" && -d "$CURRENT" ]] || fail 'Current Agent release is unavailable'
  BUNDLE=$(python3 - "$CURRENT" <<'PY'
import pathlib, sys
print(pathlib.Path(sys.argv[1]).resolve())
PY
)
  expected="$INSTALL_ROOT/releases/$RELEASE_ID/$PROVIDER"
  [[ "$BUNDLE" = "$expected" && -d "$BUNDLE" && ! -L "$BUNDLE" ]] || fail 'Current Agent release does not match the requested sealed bundle'
  RUNTIME="$BUNDLE/capability-runtime"
  IDENTITY="$RUNTIME/identity.json"
  STARTER="$BUNDLE/packaging/launchd/krw-capabilityd-start-local"
  [[ -f "$IDENTITY" && ! -L "$IDENTITY" && -x "$STARTER" && ! -L "$STARTER" && -x "$RUNTIME/.venv/bin/python" ]] \
    || fail 'Sealed canonical capability runtime is incomplete'
}

write_plist() {
  temporary=$(mktemp "${PLIST}.XXXXXX")
  cat > "$temporary" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>$LABEL</string>
  <key>ProgramArguments</key><array>
    <string>/bin/bash</string><string>$INSTALL_ROOT/current/packaging/launchd/krw-capabilityd-start-local</string>
    <string>--env-file</string><string>$CAPABILITY_ENV</string>
  </array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
  <key>ThrottleInterval</key><integer>2</integer>
  <key>WorkingDirectory</key><string>$INSTALL_ROOT</string>
  <key>StandardOutPath</key><string>$LOG_DIR/krw-capabilityd.out.log</string>
  <key>StandardErrorPath</key><string>$LOG_DIR/krw-capabilityd.err.log</string>
</dict></plist>
EOF
  plutil -lint "$temporary" >/dev/null || { rm -f "$temporary"; fail 'Generated capabilityd launchd plist is invalid'; }
  mv -f "$temporary" "$PLIST"
}

verify_identity_and_materialize_gateway() {
  python3 - "$IDENTITY" "$BUNDLE/deployments/deployment-binding.yaml" "$GATEWAY_CONFIG" "$CAPABILITY_PORT" <<'PY'
import json
import os
import pathlib
import re
import sys
import tempfile

identity_path, binding_path, config_path = map(pathlib.Path, sys.argv[1:4])
port = sys.argv[4]
identity = json.loads(identity_path.read_text(encoding="utf-8"))
required = ("build_id", "tool_schema_sha256", "release_manifest_sha256", "protocol_version", "tool_count")
if any(not identity.get(field) for field in required):
    raise SystemExit("sealed capability identity is incomplete")
binding = binding_path.read_text(encoding="utf-8")
blocks = re.split(r"(?=^  - binding_key: )", binding, flags=re.M)
ontology = [block for block in blocks if re.search(r"(?m)^    endpoint_ref: krw-ontology-local$", block)]
if not ontology:
    raise SystemExit("sealed deployment binding has no ontology endpoint")
for block in ontology:
    for field, expected in (
        ("server_build", identity["build_id"]),
        ("server_schema_bundle_hash", identity["tool_schema_sha256"]),
        ("data_release_hash", identity["release_manifest_sha256"]),
    ):
        if not re.search(rf"(?m)^    {field}: {re.escape(str(expected))}$", block):
            raise SystemExit("sealed deployment binding does not pin the canonical capability identity")
config = json.loads(config_path.read_text(encoding="utf-8"))
if not isinstance(config, dict) or not isinstance(config.get("listen"), dict) or not isinstance(config.get("tls"), dict):
    raise SystemExit("operator ontology gateway config is invalid")
config.update({
    "service": "krw-capabilityd",
    "upstream": {"host": "127.0.0.1", "port": int(port), "timeoutMs": 65000},
    "normalizeReadiness": False,
    "checkUpstreamReady": True,
    "protocolVersion": identity["protocol_version"],
    "buildId": identity["build_id"],
    "toolSchemaSha256": identity["tool_schema_sha256"],
    "releaseManifestSha256": identity["release_manifest_sha256"],
    "toolCount": identity["tool_count"],
    "toolSessionReuse": "attested-stateless-v1",
})
fd, temporary = tempfile.mkstemp(prefix=".ontology-tls.", dir=config_path.parent)
try:
    with os.fdopen(fd, "w", encoding="utf-8") as handle:
        json.dump(config, handle, ensure_ascii=False, sort_keys=True, indent=2)
        handle.write("\n")
        handle.flush()
        os.fsync(handle.fileno())
    os.chmod(temporary, 0o600)
    os.replace(temporary, config_path)
finally:
    if os.path.exists(temporary):
        os.unlink(temporary)
PY
}

wait_for_ready() {
  deadline=$(( $(date +%s) + 45 ))
  while [ "$(date +%s)" -le "$deadline" ]; do
    if curl -fsS --connect-timeout 1 --max-time 3 "http://127.0.0.1:$CAPABILITY_PORT/readyz" \
      | python3 - "$IDENTITY" <<'PY'
import json, sys
expected = json.load(open(sys.argv[1], encoding="utf-8"))
actual = json.load(sys.stdin)
for source, target in (
    ("build_id", "build_id"),
    ("tool_schema_sha256", "tool_schema_sha256"),
    ("release_manifest_sha256", "release_manifest_sha256"),
    ("protocol_version", "protocol_version"),
    ("tool_count", "tool_count"),
):
    if actual.get(source) != expected.get(target):
        raise SystemExit(1)
if actual.get("ok") is not True:
    raise SystemExit(1)
PY
    then
      return 0
    fi
    sleep 1
  done
  return 1
}

restore() {
  launchctl bootout "$DOMAIN" "$PLIST" >/dev/null 2>&1 || true
  if [ -f "$STATE_DIR/gateway.previous.json" ]; then
    install -m 0600 "$STATE_DIR/gateway.previous.json" "$GATEWAY_CONFIG"
  fi
  if [ "$(cat "$STATE_DIR/plist.existed" 2>/dev/null || true)" = 1 ] && [ -f "$STATE_DIR/plist.previous" ]; then
    install -m 0644 "$STATE_DIR/plist.previous" "$PLIST"
    launchctl bootstrap "$DOMAIN" "$PLIST"
    launchctl kickstart -k "$DOMAIN/$LABEL"
  else
    rm -f "$PLIST"
  fi
  launchctl kickstart -k "$DOMAIN/com.krw.agent.local-mcp-gateways" >/dev/null 2>&1 || true
}

case "$MODE" in
  activate)
    capture_current_bundle
    mkdir -p "$HOME/Library/LaunchAgents" "$LOG_DIR" "$STATE_DIR"
    chmod 0700 "$LOG_DIR" "$STATE_DIR"
    [ ! -e "$STATE_DIR/gateway.previous.json" ] || fail 'Capability activation state already exists'
    install -m 0600 "$GATEWAY_CONFIG" "$STATE_DIR/gateway.previous.json"
    if [ -f "$PLIST" ]; then
      install -m 0644 "$PLIST" "$STATE_DIR/plist.previous"
      printf '%s\n' 1 > "$STATE_DIR/plist.existed"
    else
      printf '%s\n' 0 > "$STATE_DIR/plist.existed"
    fi
    armed=1
    rollback_on_error() {
      status=$?
      if [ "$armed" = 1 ]; then restore; fi
      exit "$status"
    }
    trap rollback_on_error EXIT INT TERM
    verify_identity_and_materialize_gateway
    launchctl bootout "$DOMAIN" "$PLIST" >/dev/null 2>&1 || true
    write_plist
    launchctl bootstrap "$DOMAIN" "$PLIST"
    launchctl enable "$DOMAIN/$LABEL" >/dev/null 2>&1 || true
    launchctl kickstart -k "$DOMAIN/$LABEL"
    wait_for_ready || fail 'Canonical capability runtime did not become ready'
    armed=0
    trap - EXIT INT TERM
    printf '%s\n' "Activated sealed canonical capability runtime: $RELEASE_ID ($PROVIDER)"
    ;;
  rollback)
    [ -d "$STATE_DIR" ] || fail 'No capability activation state exists for this release'
    restore
    printf '%s\n' "Rolled back canonical capability runtime: $RELEASE_ID"
    ;;
esac
