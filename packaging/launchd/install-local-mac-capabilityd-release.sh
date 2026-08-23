#!/usr/bin/env bash
set -euo pipefail

# Activate the provider-independent canonical capability runtime that travels
# inside a sealed Agent release.  It owns the ontology/Guru/market tool
# surface, while the Rust daemon remains the sole owner of model execution and
# the Supabase queue.

usage() {
  printf '%s\n' 'usage: install-local-mac-capabilityd-release.sh --mode activate --release-id ID --provider glm|deepseek --operator-root /absolute/krw-agent-prod'
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

case "$MODE" in activate) ;; *) usage >&2; exit 2 ;; esac
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
  ENDPOINT_REGISTRY="$BUNDLE/deployments/endpoint-registry.yaml"
  DEPLOYMENT_BINDING="$BUNDLE/deployments/deployment-binding.yaml"
  [[ -f "$IDENTITY" && ! -L "$IDENTITY" && -f "$RUNTIME_ARCHIVE" && ! -L "$RUNTIME_ARCHIVE" && -x "$STARTER" && ! -L "$STARTER" \
    && -f "$ENDPOINT_REGISTRY" && ! -L "$ENDPOINT_REGISTRY" \
    && -f "$DEPLOYMENT_BINDING" && ! -L "$DEPLOYMENT_BINDING" ]] \
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
  python3 - "$GATEWAY_CONFIG" "$ENDPOINT_REGISTRY" "$DEPLOYMENT_BINDING" "$CAPABILITY_PORT" <<'PY'
import json, sys
config = json.load(open(sys.argv[1], encoding="utf-8"))
registry = open(sys.argv[2], encoding="utf-8").read()
binding = open(sys.argv[3], encoding="utf-8").read()

import re
from urllib.parse import urlparse

def exact_origin(value):
    parsed = urlparse(value)
    if (
        parsed.scheme != "https"
        or not parsed.netloc
        or parsed.path not in ("", "/")
        or parsed.params
        or parsed.query
        or parsed.fragment
        or parsed.username
        or parsed.password
    ):
        raise SystemExit(1)
    return value.rstrip("/")

registry_blocks = re.split(r"(?=^  - endpoint_ref: )", registry, flags=re.M)
ontology_registry = [block for block in registry_blocks if re.search(r"(?m)^  - endpoint_ref: krw-ontology-local$", block)]
if len(ontology_registry) != 1:
    raise SystemExit(1)
origin_match = re.search(r"(?m)^    origin: (.+)$", ontology_registry[0])
if not origin_match:
    raise SystemExit(1)
origin = exact_origin(origin_match.group(1).strip())

binding_blocks = re.split(r"(?=^  - binding_key: )", binding, flags=re.M)
ontology_bindings = [block for block in binding_blocks if re.search(r"(?m)^    endpoint_ref: krw-ontology-local$", block)]
reuse = {
    match.group(1)
    for block in ontology_bindings
    for match in [re.search(r"(?m)^    tool_session_reuse: ([A-Za-z0-9._-]+)$", block)]
    if match
}
if not ontology_bindings or len(reuse) != 1 or next(iter(reuse)) not in {"run-scoped", "attested-stateless-v1"}:
    raise SystemExit(1)
session_reuse = next(iter(reuse))

if config.get("service") != "krw-capabilityd":
    raise SystemExit(1)
if config.get("allowedOrigins") != [origin] or config.get("toolSessionReuse") != session_reuse:
    raise SystemExit(1)
if config.get("checkUpstreamReady") is not True or config.get("normalizeReadiness") is not True:
    raise SystemExit(1)
upstream = config.get("upstream")
if not isinstance(upstream, dict) or upstream.get("host") != "127.0.0.1" or upstream.get("port") != int(sys.argv[4]):
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
  <key>KeepAlive</key><true/>
  <key>ThrottleInterval</key><integer>10</integer>
  <key>EnvironmentVariables</key><dict>
    <key>KRW_AGENT_CURRENT_DIR</key><string>$INSTALL_ROOT/current</string>
    <key>KRW_AGENT_CAPABILITY_RUNTIME_DIR</key><string>$MATERIALIZED_RUNTIME</string>
    <key>KRW_CHART_SERIES_ENABLED</key><string>0</string>
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
  python3 - "$IDENTITY" "$BUNDLE/deployments/endpoint-registry.yaml" "$BUNDLE/deployments/deployment-binding.yaml" "$GATEWAY_CONFIG" "$CAPABILITY_PORT" <<'PY'
import json
import os
import pathlib
import re
import sys
import tempfile
from urllib.parse import urlparse

identity_path, registry_path, binding_path, config_path = map(pathlib.Path, sys.argv[1:5])
port = sys.argv[5]
identity = json.loads(identity_path.read_text(encoding="utf-8"))
required = ("build_id", "tool_schema_sha256", "release_manifest_sha256", "protocol_version", "tool_count")
if any(not identity.get(field) for field in required):
    raise SystemExit("sealed capability identity is incomplete")
registry = registry_path.read_text(encoding="utf-8")
registry_blocks = re.split(r"(?=^  - endpoint_ref: )", registry, flags=re.M)
ontology_registry = [block for block in registry_blocks if re.search(r"(?m)^  - endpoint_ref: krw-ontology-local$", block)]
if len(ontology_registry) != 1:
    raise SystemExit("sealed endpoint registry has no unique ontology endpoint")
origin_match = re.search(r"(?m)^    origin: (.+)$", ontology_registry[0])
if not origin_match:
    raise SystemExit("sealed endpoint registry has no ontology Origin")
origin = origin_match.group(1).strip().rstrip("/")
parsed_origin = urlparse(origin)
if (
    parsed_origin.scheme != "https"
    or not parsed_origin.netloc
    or parsed_origin.path not in ("", "/")
    or parsed_origin.params
    or parsed_origin.query
    or parsed_origin.fragment
    or parsed_origin.username
    or parsed_origin.password
):
    raise SystemExit("sealed endpoint registry Origin is not a bare HTTPS origin")
binding = binding_path.read_text(encoding="utf-8")
blocks = re.split(r"(?=^  - binding_key: )", binding, flags=re.M)
ontology = [block for block in blocks if re.search(r"(?m)^    endpoint_ref: krw-ontology-local$", block)]
if not ontology:
    raise SystemExit("sealed deployment binding has no ontology endpoint")
reuse = set()
for block in ontology:
    reuse_match = re.search(r"(?m)^    tool_session_reuse: ([A-Za-z0-9._-]+)$", block)
    if not reuse_match:
        raise SystemExit("sealed deployment binding has an ontology capability without session policy")
    reuse.add(reuse_match.group(1))
    for field, expected in (
        ("server_build", identity["build_id"]),
        ("server_schema_bundle_hash", identity["tool_schema_sha256"]),
        ("data_release_hash", identity["release_manifest_sha256"]),
    ):
        if not re.search(rf"(?m)^    {field}: {re.escape(str(expected))}$", block):
            raise SystemExit("sealed deployment binding does not pin the canonical capability identity")
if len(reuse) != 1 or next(iter(reuse)) not in {"run-scoped", "attested-stateless-v1"}:
    raise SystemExit("sealed deployment binding has inconsistent ontology session policy")
config = json.loads(config_path.read_text(encoding="utf-8"))
if not isinstance(config, dict) or not isinstance(config.get("listen"), dict) or not isinstance(config.get("tls"), dict):
    raise SystemExit("operator ontology gateway config is invalid")
config.update({
    "service": "krw-capabilityd",
    "upstream": {"host": "127.0.0.1", "port": int(port), "timeoutMs": 65000},
    # Starlette mounts the streamable MCP app at /mcp/; normalize the
    # loopback hop once so the TLS boundary never forwards a 307 redirect.
    "upstreamMcpPath": "/mcp/",
    # The canonical capability runtime exposes /healthz.  The TLS gateway
    # contract exposes /readyz to the Rust/front verifiers, so always enable
    # the small readiness translation here instead of depending on a mutable
    # operator config default.
    "normalizeReadiness": True,
    "upstreamReadinessPath": "/healthz",
    "checkUpstreamReady": True,
    "protocolVersion": identity["protocol_version"],
    "buildId": identity["build_id"],
    "toolSchemaSha256": identity["tool_schema_sha256"],
    "releaseManifestSha256": identity["release_manifest_sha256"],
    "toolCount": identity["tool_count"],
    # The Rust MCP client sends this exact Origin value from the sealed local
    # endpoint registry. Keep the gateway allowlist explicit and materialized
    # into every activated release; an absent Origin must remain rejected.
    "allowedOrigins": [origin],
    "toolSessionReuse": next(iter(reuse)),
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
    response=$(curl -fsS --connect-timeout 1 --max-time 3 "http://127.0.0.1:$CAPABILITY_PORT/healthz" 2>/dev/null || true)
    if [ -n "$response" ] && python3 -c '
import json, sys
expected = json.load(open(sys.argv[1], encoding="utf-8"))
actual = json.loads(sys.stdin.read())
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
' "$IDENTITY" <<<"$response"
    then
      return 0
    fi
    sleep 1
  done
  return 1
}

write_activation_status() {
  status=$1
  exit_code=$2
  temporary=$(mktemp "$STATE_DIR/.activation-status.XXXXXX")
  printf '{"schema_version":1,"component":"krw-capabilityd","release_id":"%s","provider":"%s","status":"%s","exit_code":%s}\n' \
    "$RELEASE_ID" "$PROVIDER" "$status" "$exit_code" > "$temporary"
  chmod 0600 "$temporary"
  mv -f "$temporary" "$STATE_DIR/activation-status.json"
}

ACTIVATION_FAILURE_ARMED=0
record_activation_failure() {
  status=$?
  trap - EXIT INT TERM
  if [ "$ACTIVATION_FAILURE_ARMED" = 1 ]; then
    ACTIVATION_FAILURE_ARMED=0
    write_activation_status failed "$status" || true
  fi
  exit "$status"
}

case "$MODE" in
  activate)
    capture_current_bundle
    # Capabilityd is provider-independent and long-lived.  Reusing a healthy
    # listener with the exact sealed identity keeps ordinary agent releases
    # from needlessly restarting the ontology/Guru runtime.  A mismatch still
    # fails closed and follows the full activation path below.
    capability_reuse=0
    if capability_runtime_is_active; then capability_reuse=1; fi
    mkdir -p "$HOME/Library/LaunchAgents" "$LOG_DIR"
    chmod 0700 "$LOG_DIR"
    [ ! -e "$STATE_DIR" ] || fail 'Capability activation state already exists'
    mkdir -p "$STATE_DIR"
    chmod 0700 "$STATE_DIR"
    write_activation_status activating 0
    ACTIVATION_FAILURE_ARMED=1
    trap record_activation_failure EXIT INT TERM
    if [ "$capability_reuse" = 1 ]; then
      verify_identity_and_materialize_gateway
      write_activation_status active 0
      ACTIVATION_FAILURE_ARMED=0
      trap - EXIT INT TERM
      printf '%s\n' "Reusing healthy canonical capability runtime: $RELEASE_ID ($PROVIDER)"
      exit 0
    fi
    materialize_runtime
    verify_identity_and_materialize_gateway
    launchctl bootout "$DOMAIN" "$PLIST" >/dev/null 2>&1 || true
    write_plist
    launchctl bootstrap "$DOMAIN" "$PLIST"
    launchctl enable "$DOMAIN/$LABEL" >/dev/null 2>&1 || true
    launchctl kickstart -k "$DOMAIN/$LABEL"
    wait_for_ready || fail 'Canonical capability runtime did not become ready'
    write_activation_status active 0
    ACTIVATION_FAILURE_ARMED=0
    trap - EXIT INT TERM
    printf '%s\n' "Activated sealed canonical capability runtime: $RELEASE_ID ($PROVIDER)"
    ;;
esac
