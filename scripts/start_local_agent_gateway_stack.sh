#!/usr/bin/env bash
set -euo pipefail

# Standalone loopback development stack for `krw-agent run`.
#
# This intentionally does not mount any web-app route. It starts one local
# PostgreSQL instance, one immutable ontology capability sidecar behind TLS,
# one signed Rust daemon, and the small host-owned HTTP Gateway. All durable
# local state stays below `.local/agent-gateway`; source data and the existing
# production MCP/web app are never modified.

krw_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
krw_state=${KRW_AGENT_LOCAL_STATE_DIR:-"$krw_root/.local/agent-gateway"}
krw_release_root=${KRW_AGENT_LOCAL_ONTOLOGY_RELEASE_ROOT:-~/krw-ontology-data/releases/prod/current}
krw_database_mode=${KRW_AGENT_DATABASE_MODE:-local}
krw_postgres_lifecycle=${KRW_AGENT_POSTGRES_LIFECYCLE:-persistent}
krw_supabase_project_dir=${KRW_AGENT_SUPABASE_PROJECT_DIR:-}
krw_provider=${KRW_AGENT_PROVIDER:-glm}
krw_agent_packages=(krw-ontology krw-guru-advisor)
krw_deployment_binding_source="$krw_root/deployments/local/deployment-binding.krw-ontology.example.yaml"
krw_model_registry="$krw_root/deployments/local/model-registry.$krw_provider.yaml"
# Source ontology root for build-time metric prose codegen. The Rust build
# reads `ontology/schema/metric_dictionary.yaml` from here so metric
# natural-language phrases stay in sync with the ontology without a parallel
# hardcoded mapping.
export KRW_ONTOLOGY_ROOT=${KRW_ONTOLOGY_ROOT:-~/krw-ontology}
krw_gateway_port=${KRW_AGENT_GATEWAY_PORT:-4318}
krw_pg_port=${KRW_AGENT_LOCAL_POSTGRES_PORT:-55432}
krw_capability_port=${KRW_AGENT_LOCAL_CAPABILITY_PORT:-19432}
krw_tls_port=${KRW_AGENT_LOCAL_MCP_TLS_PORT:-20432}
krw_gateway_pid=''
krw_daemon_pid=''
krw_proxy_pid=''
krw_capability_pid=''
krw_pg_started=false
krw_postgres_marker="$krw_state/postgres-lifecycle"
krw_database_url=''
krw_database_ca_file=''
krw_database_tls_mode=require
# PostgreSQL is managed externally (init once, stays up). The script only
# starts it if it is not already running.

case "$krw_database_mode" in
  local|supabase) ;;
  *)
    printf 'KRW_AGENT_DATABASE_MODE must be local or supabase\n' >&2
    exit 2
    ;;
esac
case "$krw_postgres_lifecycle" in
  persistent|ephemeral) ;;
  *) printf 'KRW_AGENT_POSTGRES_LIFECYCLE must be persistent or ephemeral\n' >&2; exit 2 ;;
esac
case "$krw_provider" in
  glm)
    krw_provider_model_id=glm-5.3-flash
    ;;
  deepseek)
    krw_provider_model_id=deepseek-v4-flash
    ;;
  *) printf 'KRW_AGENT_PROVIDER must be glm or deepseek\n' >&2; exit 2 ;;
esac
[[ -f "$krw_model_registry" ]] || {
  printf 'provider model registry is missing: %s\n' "$krw_model_registry" >&2
  exit 2
}

for krw_value in "$krw_gateway_port" "$krw_pg_port" "$krw_capability_port" "$krw_tls_port"; do
  [[ "$krw_value" =~ ^[0-9]+$ ]] && (( krw_value >= 1 && krw_value <= 65535 )) || {
    printf 'all local stack ports must be integers in 1..65535\n' >&2
    exit 2
  }
done
[[ "$krw_release_root" == /* && -d "$krw_release_root" ]] || {
  printf 'KRW_AGENT_LOCAL_ONTOLOGY_RELEASE_ROOT must be an existing absolute directory\n' >&2
  exit 2
}
krw_required_binaries=(openssl curl uv /opt/homebrew/bin/psql node npm)
if [[ "$krw_database_mode" == local ]]; then
  krw_required_binaries+=(/opt/homebrew/bin/initdb /opt/homebrew/bin/pg_ctl)
else
  krw_required_binaries+=(supabase)
fi
for krw_binary in "${krw_required_binaries[@]}"; do
  if [[ "$krw_binary" == /* ]]; then
    [[ -x "$krw_binary" ]] || { printf 'missing required binary: %s\n' "$krw_binary" >&2; exit 1; }
  else
    command -v "$krw_binary" >/dev/null 2>&1 || { printf 'missing required binary: %s\n' "$krw_binary" >&2; exit 1; }
  fi
done

mkdir -p "$krw_state" "$krw_state/logs" "$krw_state/images" "$krw_state/artifacts"
chmod 700 "$krw_state" "$krw_state/logs" "$krw_state/images" "$krw_state/artifacts"
umask 077
printf '%s\n' "$krw_postgres_lifecycle" >"$krw_postgres_marker"
chmod 600 "$krw_postgres_marker"

# Persist the exact port map alongside this reusable state directory.  The
# `dev-stack.sh reload` path reads this as data before it starts the next
# supervisor, so a stack that intentionally uses offset ports cannot restart
# against the default ports by accident.
krw_runtime_ports="$krw_state/runtime-ports.env"
krw_runtime_ports_tmp="$krw_runtime_ports.$$.tmp"
{
  printf 'KRW_AGENT_GATEWAY_PORT=%s\n' "$krw_gateway_port"
  printf 'KRW_AGENT_LOCAL_POSTGRES_PORT=%s\n' "$krw_pg_port"
  printf 'KRW_AGENT_LOCAL_CAPABILITY_PORT=%s\n' "$krw_capability_port"
  printf 'KRW_AGENT_LOCAL_MCP_TLS_PORT=%s\n' "$krw_tls_port"
} >"$krw_runtime_ports_tmp"
chmod 600 "$krw_runtime_ports_tmp"
mv -f "$krw_runtime_ports_tmp" "$krw_runtime_ports"

krw_secrets="$krw_state/secrets.env"
if [[ ! -f "$krw_secrets" ]]; then
  {
    printf 'KRW_AGENT_GATEWAY_TOKEN=%s\n' "$(openssl rand -hex 32)"
    printf 'KRW_AGENT_ARTIFACT_KEY_V1=%s\n' "$(openssl rand -hex 32)"
  } >"$krw_secrets"
  chmod 600 "$krw_secrets"
fi
# shellcheck disable=SC1090
source "$krw_secrets"
# External MCP gateway wiring (feed and filings rungs) plus the web news
# lookup. Values live in the operator's gitignored secrets file; the pinned
# CA bundle for the external gateways is deployment material read from the
# state directory. Names only are referenced here.
if [[ -f "$krw_state/mcp-gateways-ca.pem" ]]; then
  export KRW_AGENT_MCP_CA_PEM="$(< "$krw_state/mcp-gateways-ca.pem")"
fi
export KRW_AGENT_GATEWAY_TOKEN KRW_AGENT_ARTIFACT_KEY_V1 \
  KRW_FEED_MCP_URL KRW_FEED_MCP_READY_URL KRW_FEED_MCP_TOKEN \
  KRW_FILINGS_MCP_URL KRW_FILINGS_MCP_READY_URL KRW_FILINGS_MCP_TOKEN \
  KRW_AGENT_MCP_CA_PEM KRW_WEB_NEWS_API_KEY KRW_WEB_NEWS_API_BASE

cleanup() {
  set +e
  for krw_pid in "$krw_gateway_pid" "$krw_daemon_pid" "$krw_proxy_pid" "$krw_capability_pid"; do
    [[ -n "$krw_pid" ]] && kill -TERM "$krw_pid" 2>/dev/null
  done
  for krw_pid in "$krw_gateway_pid" "$krw_daemon_pid" "$krw_proxy_pid" "$krw_capability_pid"; do
    [[ -n "$krw_pid" ]] && wait "$krw_pid" 2>/dev/null
  done
  if [[ "$krw_postgres_lifecycle" == ephemeral && "$krw_database_mode" == local && "$krw_pg_started" == true ]]; then
    /opt/homebrew/bin/pg_ctl -D "$krw_state_abs/postgres" -m fast -w stop >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT INT TERM

# Database: either reuse the script-managed TLS PostgreSQL instance or use an
# already-running Supabase CLI project. The two modes share the same Rust
# Postgres ABI and migration runner; only startup/transport setup differs.
krw_state_abs=$(CDPATH= cd -- "$krw_state" && pwd)
# The capability sidecar is always served through the local TLS proxy, even
# when PostgreSQL is supplied by Supabase.  Keep its CA/certificate lifecycle
# independent from the database mode so the shared-DB development stack has
# the same pinned MCP transport as the local-Postgres mode.
if [[ ! -f "$krw_state_abs/ca.pem" || ! -f "$krw_state_abs/server.pem" || ! -f "$krw_state_abs/server.key" ]]; then
  openssl req -x509 -newkey rsa:2048 -nodes -sha256 -days 30 \
    -config "$krw_root/scripts/live_e2e_openssl.cnf" -extensions certificate_authority_extensions \
    -keyout "$krw_state_abs/ca.key" -out "$krw_state_abs/ca.pem" >/dev/null 2>&1
  openssl req -new -newkey rsa:2048 -nodes -sha256 \
    -config "$krw_root/scripts/live_e2e_openssl.cnf" -reqexts request_extensions \
    -keyout "$krw_state_abs/server.key" -out "$krw_state_abs/server.csr" >/dev/null 2>&1
  openssl x509 -req -sha256 -days 30 -in "$krw_state_abs/server.csr" \
    -CA "$krw_state_abs/ca.pem" -CAkey "$krw_state_abs/ca.key" -CAcreateserial \
    -extfile "$krw_root/scripts/live_e2e_openssl.cnf" -extensions server_certificate_extensions \
    -out "$krw_state_abs/server.pem" >/dev/null 2>&1
  chmod 600 "$krw_state_abs/ca.key" "$krw_state_abs/server.key"
fi
if [[ "$krw_database_mode" == local ]]; then
  if [[ ! -f "$krw_state_abs/postgres/PG_VERSION" ]]; then
    /opt/homebrew/bin/initdb -A trust -D "$krw_state_abs/postgres" >"$krw_state_abs/logs/initdb.log"
  fi
  # pg_ctl needs absolute certificate paths; PostgreSQL otherwise resolves
  # relative paths against its own working directory.
  if [[ ! -f "$krw_state_abs/ca.pem" || ! -f "$krw_state_abs/server.pem" || ! -f "$krw_state_abs/server.key" ]]; then
    openssl req -x509 -newkey rsa:2048 -nodes -sha256 -days 30 \
      -config "$krw_root/scripts/live_e2e_openssl.cnf" -extensions certificate_authority_extensions \
      -keyout "$krw_state_abs/ca.key" -out "$krw_state_abs/ca.pem" >/dev/null 2>&1
    openssl req -new -newkey rsa:2048 -nodes -sha256 \
      -config "$krw_root/scripts/live_e2e_openssl.cnf" -reqexts request_extensions \
      -keyout "$krw_state_abs/server.key" -out "$krw_state_abs/server.csr" >/dev/null 2>&1
    openssl x509 -req -sha256 -days 30 -in "$krw_state_abs/server.csr" \
      -CA "$krw_state_abs/ca.pem" -CAkey "$krw_state_abs/ca.key" -CAcreateserial \
      -extfile "$krw_root/scripts/live_e2e_openssl.cnf" -extensions server_certificate_extensions \
      -out "$krw_state_abs/server.pem" >/dev/null 2>&1
    chmod 600 "$krw_state_abs/ca.key" "$krw_state_abs/server.key"
  fi
  if ! /opt/homebrew/bin/pg_ctl -D "$krw_state_abs/postgres" status >/dev/null 2>&1; then
    /opt/homebrew/bin/pg_ctl -D "$krw_state_abs/postgres" -l "$krw_state_abs/logs/postgres.log" \
      -o "-F -p $krw_pg_port -h 127.0.0.1 -c ssl=on -c ssl_cert_file='$krw_state_abs/server.pem' -c ssl_key_file='$krw_state_abs/server.key'" \
      -w start >/dev/null
    krw_pg_started=true
  fi
  if ! PGSSLMODE=verify-ca PGSSLROOTCERT="$krw_state_abs/ca.pem" \
      /opt/homebrew/bin/psql -X -h 127.0.0.1 -p "$krw_pg_port" -d postgres -tAc "select 1" >/dev/null 2>&1; then
    printf 'local PostgreSQL is up but does not accept TLS; restarting with ssl=on\n' >&2
    /opt/homebrew/bin/pg_ctl -D "$krw_state_abs/postgres" -m fast -w stop >/dev/null 2>&1
    /opt/homebrew/bin/pg_ctl -D "$krw_state_abs/postgres" -l "$krw_state_abs/logs/postgres.log" \
      -o "-F -p $krw_pg_port -h 127.0.0.1 -c ssl=on -c ssl_cert_file='$krw_state_abs/server.pem' -c ssl_key_file='$krw_state_abs/server.key'" \
      -w start >/dev/null
    krw_pg_started=true
  fi

  # Bootstrap the inert retention role before applying the agent migrations.
  PGSSLMODE=verify-ca PGSSLROOTCERT="$krw_state_abs/ca.pem" \
    /opt/homebrew/bin/psql -X -v ON_ERROR_STOP=1 -h 127.0.0.1 -p "$krw_pg_port" -d postgres \
    -c "DO \$\$ BEGIN IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'krw_agent_daemon') THEN CREATE ROLE krw_agent_daemon NOLOGIN; END IF; END \$\$;" \
    >/dev/null
  PGSSLMODE=verify-ca PGSSLROOTCERT="$krw_state_abs/ca.pem" \
    "$krw_root/scripts/apply_migrations.sh" -X -h 127.0.0.1 -p "$krw_pg_port" -d postgres \
    >/dev/null
  krw_database_url="postgresql://127.0.0.1:$krw_pg_port/postgres?user=$(id -un)"
  krw_database_ca_file="$krw_state_abs/ca.pem"
else
  [[ -n "$krw_supabase_project_dir" && "$krw_supabase_project_dir" == /* && -d "$krw_supabase_project_dir" ]] || {
    printf 'KRW_AGENT_SUPABASE_PROJECT_DIR must point to an existing absolute Supabase project\n' >&2
    exit 2
  }
  if [[ "${KRW_AGENT_SUPABASE_AUTOSTART:-0}" == 1 ]]; then
    supabase start --workdir "$krw_supabase_project_dir" >/dev/null 2>&1
  fi
  krw_database_url="${KRW_AGENT_DATABASE_URL:-}"
  if [[ -z "$krw_database_url" ]]; then
    krw_database_url="$(supabase status --workdir "$krw_supabase_project_dir" -o json 2>/dev/null | python3 -c 'import json,sys; print(json.load(sys.stdin).get("DB_URL", ""))')"
  fi
  [[ "$krw_database_url" == postgresql://* || "$krw_database_url" == postgres://* ]] || {
    printf 'Supabase DB_URL was not found; start Supabase or set KRW_AGENT_DATABASE_URL\n' >&2
    exit 1
  }
  # Supabase CLI's local Postgres port is plain TCP by default. This is a
  # deliberate local-only mode; hosted Supabase should use database mode
  # `local` plus a TLS URL, or be supplied through the standard daemon path.
  krw_database_tls_mode=disable
  PGSSLMODE=disable /opt/homebrew/bin/psql -X -v ON_ERROR_STOP=1 "$krw_database_url" \
    -c "DO \$\$ BEGIN IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'krw_agent_daemon') THEN CREATE ROLE krw_agent_daemon NOLOGIN; END IF; END \$\$;" \
    >/dev/null
  PGSSLMODE=disable "$krw_root/scripts/apply_migrations.sh" "$krw_database_url" >/dev/null
fi

read -r krw_build krw_schema_hash krw_release_hash < <(
  (
    cd "$krw_root/services/krw-ontology-runtime"
    KRW_ONTOLOGY_ENV=prod KRW_ONTOLOGY_RELEASE_ROOT="$krw_release_root" uv run krw-capabilityd --print-identity
  ) | python3 -c '
import json, sys
value = json.load(sys.stdin)
if value.get("ok") is not True: raise SystemExit("capability identity is not admitted")
print(value["build_id"], value["tool_schema_sha256"], value["release_manifest_sha256"])
'
)

# The image, descriptor, and release authorization are immutable inputs to
# agentd. Keying them by source/configuration content means a stop/start cycle
# only recreates short-lived processes; it does not compile every skill or
# sign an identical release again.
krw_cache_root="$krw_state/cache"
mkdir -p "$krw_cache_root"
chmod 700 "$krw_cache_root"
krw_prepare_fingerprint=$(python3 - \
  "$krw_root/agents/krw-ontology" \
  "$krw_root/agents/krw-guru-advisor" \
  "$krw_deployment_binding_source" \
  "$krw_model_registry" \
  "$krw_root/deployments/local/budget-registry.yaml" \
  "$krw_root/target/debug/krw-agent" \
  "$krw_root/target/debug/krw-agentd" \
  "$krw_build" "$krw_schema_hash" "$krw_release_hash" <<'PY'
import hashlib
import pathlib
import sys

paths = [pathlib.Path(value) for value in sys.argv[1:8]]
identity = sys.argv[8:]
digest = hashlib.sha256()

for value in identity:
    digest.update(b"identity\0")
    digest.update(value.encode("utf-8"))
    digest.update(b"\0")

files = []
for root in paths:
    if root.is_dir():
        files.extend(path for path in root.rglob("*") if path.is_file())
    elif root.is_file():
        files.append(root)
for path in sorted(files, key=lambda item: item.as_posix()):
    digest.update(b"path\0")
    digest.update(path.as_posix().encode("utf-8"))
    digest.update(b"\0")
    digest.update(hashlib.sha256(path.read_bytes()).digest())
    digest.update(b"\0")
print(digest.hexdigest())
PY
)
krw_cache_dir="$krw_cache_root/$krw_prepare_fingerprint"
mkdir -p "$krw_cache_dir"
chmod 700 "$krw_cache_dir"

# Optional release-manifest hash of the external feed/filings MCP gateways,
# supplied by name through the operator's environment (never a value here).
krw_gateways_release=${KRW_MCP_GATEWAYS_RELEASE_MANIFEST_SHA256:-}
python3 - "$krw_deployment_binding_source" "$krw_state/deployment-binding.yaml" "$krw_state/endpoint-registry.yaml" "$krw_build" "$krw_schema_hash" "$krw_release_hash" "$krw_gateways_release" <<'PY'
import pathlib, re, sys
import json
source, output, endpoint_output, build, schema, release, gateways_release = sys.argv[1:8]
text = pathlib.Path(source).read_text(encoding="utf-8")
# The feed and filings endpoints are served by the external MCP gateways,
# whose readiness documents pin their own front-contract release manifest.
# Their bindings must therefore not inherit this stack's local release hash;
# substitute the operator-provided gateway manifest hash (an environment
# NAME resolved by the caller, value never in this script) when present.
if gateways_release:
    external_refs = {"krw-feed-local", "krw-filings-local"}
    rewritten = []
    external_block = False
    for line in text.splitlines(keepends=True):
        matched = re.search(r"^\s*endpoint_ref:\s*(\S+)\s*$", line)
        if matched:
            external_block = matched.group(1) in external_refs
        if external_block and re.match(r"^\s*data_release_hash:\s*sha256:0{64}\s*$", line):
            indent = line[: len(line) - len(line.lstrip())]
            rewritten.append(f"{indent}data_release_hash: {gateways_release}\n")
            continue
        rewritten.append(line)
    text = "".join(rewritten)
text = text.replace("server_schema_bundle_hash: sha256:" + "0" * 64, "server_schema_bundle_hash: " + schema)
text = text.replace("server_build: fixture", "server_build: " + build)
text = text.replace("data_release_hash: sha256:" + "0" * 64, "data_release_hash: " + release)
pathlib.Path(output).write_text(text, encoding="utf-8")
endpoint = {
    "schema_version": 1,
    "registry_id": "local-loopback-runtime",
    "endpoints": [
        {
            "endpoint_ref": "krw-ontology-local",
            "url_env": "KRW_ONTOLOGY_MCP_URL",
            "readiness_url_env": "KRW_ONTOLOGY_READY_URL",
            "protocol_version": "2025-06-18",
            "origin": "https://krw-agent.local",
            "credential_version": "local-public-v1",
            "tls_profile": "system-plus-pinned-ca-v1",
            "tls_ca_pem_env": "KRW_ONTOLOGY_CA_PEM",
        },
        {
            "endpoint_ref": "krw-feed-local",
            "url_env": "KRW_FEED_MCP_URL",
            "readiness_url_env": "KRW_FEED_MCP_READY_URL",
            "protocol_version": "2025-06-18",
            "origin": "https://127.0.0.1",
            "credential_version": "feed-service-v1",
            "tls_profile": "system-plus-pinned-ca-v1",
            "tls_ca_pem_env": "KRW_AGENT_MCP_CA_PEM",
        },
        {
            "endpoint_ref": "krw-filings-local",
            "url_env": "KRW_FILINGS_MCP_URL",
            "readiness_url_env": "KRW_FILINGS_MCP_READY_URL",
            "protocol_version": "2025-06-18",
            "origin": "https://127.0.0.1",
            "credential_version": "filings-service-v1",
            "tls_profile": "system-plus-pinned-ca-v1",
            "tls_ca_pem_env": "KRW_AGENT_MCP_CA_PEM",
        },
    ],
}
# YAML 1.2 is a JSON superset; the Rust YAML loader accepts this canonical,
# secret-free document without a second YAML dependency. The feed and filings
# descriptors resolve against operator-provided environment names (URLs,
# readiness probes, bearer tokens, and the shared pinned CA bundle for the
# external MCP gateways); only the names appear here, never values.
pathlib.Path(endpoint_output).write_text(json.dumps(endpoint, separators=(",", ":"), sort_keys=True), encoding="utf-8")
PY

(
  cd "$krw_root/services/krw-ontology-runtime"
  KRW_ONTOLOGY_ENV=prod KRW_ONTOLOGY_RELEASE_ROOT="$krw_release_root" \
  KRW_CAPABILITYD_HOST=127.0.0.1 KRW_CAPABILITYD_PORT="$krw_capability_port" \
  KRW_CAPABILITYD_EXPECTED_BUILD_ID="$krw_build" \
  KRW_CAPABILITYD_EXPECTED_TOOL_SCHEMA_SHA256="$krw_schema_hash" \
  KRW_CAPABILITYD_EXPECTED_RELEASE_MANIFEST_SHA256="$krw_release_hash" \
  exec "$krw_root/scripts/with_local_env.sh" --market-sidecar uv run krw-capabilityd
) >"$krw_state/logs/capabilityd.log" 2>&1 &
krw_capability_pid=$!
for _ in $(seq 1 100); do curl --fail --silent "http://127.0.0.1:$krw_capability_port/healthz" >/dev/null && break; sleep 0.1; done
curl --fail --silent "http://127.0.0.1:$krw_capability_port/healthz" >/dev/null
python3 "$krw_root/scripts/local_tls_reverse_proxy.py" --listen-port "$krw_tls_port" --upstream-port "$krw_capability_port" \
  --certfile "$krw_state/server.pem" --keyfile "$krw_state/server.key" >"$krw_state/logs/mcp-tls-proxy.log" 2>&1 &
krw_proxy_pid=$!
for _ in $(seq 1 100); do curl --fail --silent --cacert "$krw_state/ca.pem" "https://127.0.0.1:$krw_tls_port/healthz" >/dev/null && break; sleep 0.1; done
curl --fail --silent --cacert "$krw_state/ca.pem" "https://127.0.0.1:$krw_tls_port/healthz" >/dev/null

krw_agent_bin="$krw_root/target/debug/krw-agent"
krw_agentd_bin="$krw_root/target/debug/krw-agentd"
krw_image_root="$krw_cache_dir/images"
mkdir -p "$krw_image_root"
krw_image_dirs=()
krw_image_rebuilt=false
for krw_agent_package in "${krw_agent_packages[@]}"; do
  krw_image_dir="$krw_image_root/$krw_agent_package"
  if [[ -d "$krw_image_dir" && -f "$krw_image_dir/manifest.json" ]] &&
      "$krw_agent_bin" image verify "$krw_image_dir" >/dev/null 2>&1; then
    krw_image_dirs+=("$krw_image_dir")
    continue
  fi
  if [[ -e "$krw_image_dir" ]]; then
    krw_quarantine="$krw_image_root/${krw_agent_package}.invalid-$(date +%s)-$$"
    mv "$krw_image_dir" "$krw_quarantine"
  fi
  "$krw_root/scripts/with_local_env.sh" "$krw_agent_bin" image build \
    --out "$krw_image_dir" "$krw_root/agents/$krw_agent_package" >/dev/null
  krw_image_dirs+=("$krw_image_dir")
  krw_image_rebuilt=true
done
if [[ "$krw_image_rebuilt" == true ]]; then
  printf 'prepared agent images (ontology + Guru): %s\n' "$krw_prepare_fingerprint"
else
  printf 'reusing prepared agent images (ontology + Guru): %s\n' "$krw_prepare_fingerprint"
fi
krw_database_user=$(id -un)
export KRW_ONTOLOGY_MCP_URL="https://127.0.0.1:$krw_tls_port/mcp/"
export KRW_ONTOLOGY_READY_URL="https://127.0.0.1:$krw_tls_port/healthz"
export KRW_ONTOLOGY_CA_PEM="$(< "$krw_state/ca.pem")"
krw_descriptor="$krw_cache_dir/public-release.json"
krw_release_authorization="$krw_cache_dir/release-authorization.json"
krw_image_args=()
for krw_image_dir in "${krw_image_dirs[@]}"; do
  krw_image_args+=(--image-dir "$krw_image_dir")
done
"$krw_root/scripts/with_local_env.sh" "$krw_agentd_bin" \
  "${krw_image_args[@]}" --deployment-binding "$krw_state/deployment-binding.yaml" \
  --model-registry "$krw_model_registry" --provider "$krw_provider" --budget-registry "$krw_root/deployments/local/budget-registry.yaml" \
  --endpoint-registry "$krw_state/endpoint-registry.yaml" --check \
  --public-release-descriptor-output "$krw_descriptor" >/dev/null

if [[ ! -f "$krw_state/release-private.pk8" ]]; then
  "$krw_root/scripts/with_local_env.sh" "$krw_root/target/debug/krw-agent" release keygen --private-key-out "$krw_state/release-private.pk8" >"$krw_state/release-public-key.txt"
fi
krw_public_key=$("$krw_root/scripts/with_local_env.sh" "$krw_root/target/debug/krw-agent" release public-key --private-key "$krw_state/release-private.pk8")
python3 - "$krw_state/release-trust-registry.json" "$krw_public_key" <<'PY'
import json, sys
path, public = sys.argv[1:]
value = {"keys":[{"ed25519_public_key_hex":public,"key_id":"local-dev-v1","not_after_unix_seconds":2000000000,"not_before_unix_seconds":1700000000,"revoked":False}],"minimum_sequence":1,"registry_id":"local-dev","schema_version":1}
open(path, "w", encoding="utf-8").write(json.dumps(value, separators=(",", ":"), sort_keys=True))
PY
krw_now=$(date +%s)
if [[ -f "$krw_release_authorization" ]] && \
    "$krw_root/scripts/with_local_env.sh" "$krw_agent_bin" release verify \
      --descriptor "$krw_descriptor" --authorization "$krw_release_authorization" \
      --trust-registry "$krw_state/release-trust-registry.json" \
      --runtime-version 0.1.0 --kernel-version 0.1.0 --now-unix-seconds "$krw_now" \
      >/dev/null 2>&1; then
  printf 'reusing unexpired release authorization: %s\n' "$krw_prepare_fingerprint"
else
  krw_expiry=$((krw_now + 2592000))
  krw_new_authorization="$krw_cache_dir/release-authorization.$$.json"
  "$krw_root/scripts/with_local_env.sh" "$krw_agent_bin" release sign \
    --descriptor "$krw_descriptor" --private-key "$krw_state/release-private.pk8" --key-id local-dev-v1 --sequence 1 \
    --issued-at-unix-seconds "$krw_now" --expires-at-unix-seconds "$krw_expiry" --runtime-version 0.1.0 --kernel-version 0.1.0 \
    --model-id "$krw_provider_model_id" \
    --out "$krw_new_authorization" >/dev/null
  mv -f "$krw_new_authorization" "$krw_release_authorization"
  printf 'signed prepared release authorization: %s\n' "$krw_prepare_fingerprint"
fi

export KRW_AGENT_DATABASE_URL="$krw_database_url"
krw_agentd_database_args=(--database-url-env KRW_AGENT_DATABASE_URL --database-tls-mode "$krw_database_tls_mode")
if [[ -n "$krw_database_ca_file" ]]; then
  export KRW_AGENT_DATABASE_CA_PEM="$(< "$krw_database_ca_file")"
  krw_agentd_database_args+=(--database-ca-pem-env KRW_AGENT_DATABASE_CA_PEM)
else
  unset KRW_AGENT_DATABASE_CA_PEM
fi
export RUST_LOG="${RUST_LOG:-info}"
# Forward provider/MCP diagnostic flags only when the operator explicitly sets
# them. MCP diagnostics are metadata-only (name, byte count, hash), never raw
# arguments or results; an empty inherited variable remains disabled.
export KRW_DEBUG_PROVIDER="${KRW_DEBUG_PROVIDER:-}"
export KRW_DEBUG_MCP_ARGS="${KRW_DEBUG_MCP_ARGS:-}"
# Run the pre-built agentd binary directly (not via cargo run) so the log
# stays clean and the process is a simple child of this script.
"$krw_root/scripts/with_local_env.sh" "$krw_agentd_bin" \
    "${krw_image_args[@]}" --deployment-binding "$krw_state/deployment-binding.yaml" \
    --model-registry "$krw_model_registry" --provider "$krw_provider" --budget-registry "$krw_root/deployments/local/budget-registry.yaml" \
    --endpoint-registry "$krw_state/endpoint-registry.yaml" --release-authorization "$krw_release_authorization" \
    --release-trust-registry "$krw_state/release-trust-registry.json" --runtime-version 0.1.0 --worker-id local-agentd \
    "${krw_agentd_database_args[@]}" \
    --database-max-connections 32 \
    --artifact-root "$krw_state/artifacts" --artifact-active-key-env KRW_AGENT_ARTIFACT_KEY_V1 \
    >"$krw_state/logs/agentd.log" 2>&1 &
krw_daemon_pid=$!

krw_descriptor_hash="sha256:$(shasum -a 256 "$krw_descriptor" | awk '{print $1}')"
krw_release_set_hash=$(python3 - "$krw_descriptor" <<'PY'
import json, sys
print(json.load(open(sys.argv[1], encoding="utf-8"))["release_set_hash"])
PY
)
# Keep the front-end's immutable release pin in a separate, generated file.
# A new local stack prepares a new image and descriptor, so a checked-in pin
# would otherwise leave the UI enqueueing runs that the new daemon cannot
# claim.  The file is mode 0600: it contains the local DB URL for development
# in addition to the allowlisted release settings, and is never packaged.
krw_frontend_env="$krw_state_abs/frontend.env"
python3 - "$krw_frontend_env" "$krw_descriptor" "$krw_descriptor_hash" "$krw_release_set_hash" "$krw_database_url" <<'PY'
import os
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
values = {
    "KRW_AGENT_BACKEND_MODE": "rust",
    "KRW_AGENT_PROVIDER": os.environ.get("KRW_AGENT_PROVIDER", "glm"),
    "KRW_AGENT_ADMISSION_MODE": "open",
    "KRW_RUNTIME_ENVIRONMENT": "dev",
    "KRW_AGENT_TENANT_ID": "local_tenant",
    "KRW_AGENT_RELEASE_DESCRIPTOR_PATH": sys.argv[2],
    "KRW_AGENT_RELEASE_ARTIFACT_HASH": sys.argv[3],
    "KRW_AGENT_RELEASE_SET_HASH": sys.argv[4],
    "AGENT_V1_DATABASE_URL": sys.argv[5],
}
if any("\n" in value or "\r" in value or "\0" in value for value in values.values()):
    raise SystemExit("invalid_frontend_runtime_env")
temporary = path.with_name(f".{path.name}.{os.getpid()}.tmp")
temporary.write_text(
    "\n".join(f"{key}={value}" for key, value in values.items()) + "\n",
    encoding="utf-8",
)
os.chmod(temporary, 0o600)
os.replace(temporary, path)
PY
export KRW_AGENT_GATEWAY_HOST=127.0.0.1 KRW_AGENT_GATEWAY_PORT="$krw_gateway_port"
export KRW_AGENT_GATEWAY_TENANT_ID=local_tenant KRW_AGENT_GATEWAY_PRINCIPAL_ID=local_principal
export KRW_AGENT_GATEWAY_DATABASE_URL="$KRW_AGENT_DATABASE_URL"
if [[ -n "$krw_database_ca_file" ]]; then
  export KRW_AGENT_GATEWAY_DATABASE_CA_PEM_FILE="$krw_database_ca_file"
else
  unset KRW_AGENT_GATEWAY_DATABASE_CA_PEM_FILE
fi
export KRW_AGENT_RELEASE_DESCRIPTOR_PATH="$krw_descriptor"
export KRW_AGENT_RELEASE_DESCRIPTOR_HASH="$krw_descriptor_hash"
export KRW_AGENT_RELEASE_SET_HASH="$krw_release_set_hash"
(
  cd "$krw_root/packages/host-ts"
  exec npm run gateway:local
) >"$krw_state/logs/gateway.log" 2>&1 &
krw_gateway_pid=$!
for _ in $(seq 1 100); do curl --fail --silent "http://127.0.0.1:$krw_gateway_port/healthz" >/dev/null && break; sleep 0.1; done
curl --fail --silent "http://127.0.0.1:$krw_gateway_port/healthz" >/dev/null

printf '\nGateway ready: http://127.0.0.1:%s/v1/agent\n' "$krw_gateway_port"
printf 'In another terminal:\n'
printf '  source %q; export KRW_AGENT_GATEWAY_TOKEN\n' "$krw_secrets"
printf '  cd %q\n' "$krw_root"
printf '  ./scripts/with_local_env.sh cargo run -q -p krw-agent -- run --gateway-url http://127.0.0.1:%s/v1/agent --ticker AAPL --question "..." --wait\n' "$krw_gateway_port"
printf 'Logs: %s/logs\n\n' "$krw_state"
printf 'Stack is running. Press Ctrl-C to stop all services.\n'

# The Gateway can still answer /healthz while the actual worker has exited.
# Supervising the worker prevents a stale Gateway from accepting a long quality
# batch that no process can ever claim. The EXIT trap below tears down the
# remaining local children without touching the persistent PostgreSQL state.
if wait "$krw_daemon_pid"; then
  printf 'agentd exited; stopping the local stack\n' >&2
  exit 0
else
  krw_daemon_status=$?
  printf 'agentd exited with status %s; stopping the local stack\n' "$krw_daemon_status" >&2
  exit "$krw_daemon_status"
fi
