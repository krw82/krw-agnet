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
MATERIALIZED_RUNTIME="$STATE_DIR/runtime"

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
  RUNTIME_ARCHIVE="$RUNTIME/capability-runtime.venv.tar.gz"
  STARTER="$BUNDLE/packaging/launchd/krw-capabilityd-start-local"
  [[ -f "$IDENTITY" && ! -L "$IDENTITY" && -f "$RUNTIME_ARCHIVE" && ! -L "$RUNTIME_ARCHIVE" && -x "$STARTER" && ! -L "$STARTER" ]] \
    || fail 'Sealed canonical capability runtime is incomplete'
}

capability_runtime_is_active() {
  [[ -f "$IDENTITY" && -f "$GATEWAY_CONFIG" ]] || return 1
  curl -fsS --connect-timeout 1 --max-time 3 "http://127.0.0.1:$CAPABILITY_PORT/healthz" \
    | python3 -c '
import json, sys
expected = json.load(open(sys.argv[1], encoding="utf-8"))
actual = json.load(sys.stdin)
for field in ("ok", "build_id", "tool_schema_sha256", "release_manifest_sha256", "protocol_version", "tool_count"):
    if actual.get(field) != expected.get(field):
        raise SystemExit(1)
if actual.get("ok") is not True:
    raise SystemExit(1)
' "$IDENTITY" >/dev/null 2>&1 || return 1
  python3 - "$GATEWAY_CONFIG" "$CAPABILITY_PORT" <<'PY'
import json, sys
config = json.load(open(sys.argv[1], encoding="utf-8"))
if config.get("service") != "krw-capabilityd":
    raise SystemExit(1)
upstream = config.get("upstream")
if not isinstance(upstream, dict) or upstream.get("host") != "127.0.0.1" or upstream.get("port") != int(sys.argv[2]):
    raise SystemExit(1)
PY
}

validate_runtime_archive() {
  python3 - "$RUNTIME_ARCHIVE" <<'PY'
import pathlib
import posixpath
import tarfile
import sys

archive_path = pathlib.Path(sys.argv[1])
if archive_path.stat().st_size <= 0 or archive_path.stat().st_size > 4 * 1024**3:
    raise SystemExit("capability runtime archive is outside the allowed size bound")

members = []
with tarfile.open(archive_path, mode="r:gz") as archive:
    for member in archive.getmembers():
        name = member.name
        path = pathlib.PurePosixPath(name)
        if not name or path.is_absolute() or any(part in ("", ".", "..") for part in path.parts):
            raise SystemExit("capability runtime archive has an unsafe path")
        if path.parts[0] not in {".venv", ".python"}:
            raise SystemExit("capability runtime archive has an unexpected root")
        if member.isdev() or member.isfifo() or member.ischr() or member.isblk():
            raise SystemExit("capability runtime archive has an unsupported entry")
        if member.issym() or member.islnk():
            target = member.linkname
            if not target or pathlib.PurePosixPath(target).is_absolute():
                raise SystemExit("capability runtime archive has an unsafe link")
            resolved = pathlib.PurePosixPath(posixpath.normpath(str(path.parent / target)))
            if not resolved.parts or resolved.parts[0] not in {".venv", ".python"} or any(part == ".." for part in resolved.parts):
                raise SystemExit("capability runtime archive link escapes the virtual environment")
        elif not member.isdir() and not member.isfile():
            raise SystemExit("capability runtime archive has an unsupported entry")
        members.append(member)

if not members or len(members) > 100000:
    raise SystemExit("capability runtime archive entry count is invalid")
PY
}

safe_remove_runtime_staging() {
  case "$1" in
    "$STATE_DIR"/.runtime-staging.*) [ -d "$1" ] && rm -rf -- "$1" ;;
    *) fail 'Refusing to remove an unexpected capability runtime staging directory' ;;
  esac
}

safe_remove_materialized_runtime() {
  case "$1" in
    "$STATE_DIR"/runtime) [ -d "$1" ] && rm -rf -- "$1" ;;
    *) fail 'Refusing to remove an unexpected materialized capability runtime' ;;
  esac
}

rewrite_runtime_home() {
  python3 - "$MATERIALIZED_RUNTIME/.venv/pyvenv.cfg" "$MATERIALIZED_RUNTIME/.python/bin" <<'PY'
import os
import pathlib
import sys
import tempfile

path = pathlib.Path(sys.argv[1])
home = sys.argv[2]
lines = path.read_text(encoding="utf-8").splitlines()
replaced = 0
rewritten = []
for line in lines:
    if line.startswith("home = "):
        rewritten.append(f"home = {home}")
        replaced += 1
    else:
        rewritten.append(line)
if replaced != 1:
    raise SystemExit("materialized capability runtime has an invalid pyvenv configuration")
fd, temporary = tempfile.mkstemp(prefix=".pyvenv.", dir=path.parent)
try:
    with os.fdopen(fd, "w", encoding="utf-8") as handle:
        handle.write("\n".join(rewritten) + "\n")
        handle.flush()
        os.fsync(handle.fileno())
    os.chmod(temporary, 0o644)
    os.replace(temporary, path)
finally:
    if os.path.exists(temporary):
        os.unlink(temporary)
PY
}

materialize_runtime() {
  [ ! -e "$MATERIALIZED_RUNTIME" ] || fail 'Capability runtime state already exists for this release'
  validate_runtime_archive
  runtime_staging=$(mktemp -d "$STATE_DIR/.runtime-staging.XXXXXX")
  if ! tar -xzf "$RUNTIME_ARCHIVE" -C "$runtime_staging"; then
    safe_remove_runtime_staging "$runtime_staging"
    fail 'Capability runtime archive extraction failed'
  fi
  if [[ ! -d "$runtime_staging/.venv" || -L "$runtime_staging/.venv" || ! -d "$runtime_staging/.python" || -L "$runtime_staging/.python" || ! -x "$runtime_staging/.venv/bin/python" ]]; then
    safe_remove_runtime_staging "$runtime_staging"
    fail 'Capability runtime archive did not materialize a usable Python environment'
  fi
  mv -- "$runtime_staging" "$MATERIALIZED_RUNTIME"
  if ! rewrite_runtime_home || ! "$MATERIALIZED_RUNTIME/.venv/bin/python" -c 'import krw_capability_runtime' >/dev/null; then
    safe_remove_materialized_runtime "$MATERIALIZED_RUNTIME"
    fail 'Capability runtime archive cannot import the canonical capability service'
  fi
}

xml_escape_path() {
  case "$1" in *'&'*|*'<'*|*'>'*|*$'\n'*|*$'\r'*) return 1 ;; esac
  printf '%s' "$1"
}

write_plist() {
  for value in "$CAPABILITY_ENV" "$LOG_DIR" "$INSTALL_ROOT/current" "$MATERIALIZED_RUNTIME"; do
    xml_escape_path "$value" >/dev/null || fail 'Launchd path contains an unsupported XML character'
  done
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
  <key>EnvironmentVariables</key><dict>
    <key>KRW_AGENT_CURRENT_DIR</key><string>$INSTALL_ROOT/current</string>
    <key>KRW_AGENT_CAPABILITY_RUNTIME_DIR</key><string>$MATERIALIZED_RUNTIME</string>
  </dict>
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
    if curl -fsS --connect-timeout 1 --max-time 3 "http://127.0.0.1:$CAPABILITY_PORT/healthz" \
      | python3 -c '
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
' "$IDENTITY"
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
    # Capabilityd is provider-independent and long-lived.  Reusing a healthy
    # listener with the exact sealed identity keeps ordinary agent releases
    # from needlessly restarting the ontology/Guru runtime.  A mismatch still
    # fails closed and follows the full activation path below.
    if capability_runtime_is_active; then
      printf '%s\n' "Reusing healthy canonical capability runtime: $RELEASE_ID ($PROVIDER)"
      exit 0
    fi
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
    materialize_runtime
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
