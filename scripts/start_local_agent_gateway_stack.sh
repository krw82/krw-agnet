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
# PostgreSQL is managed externally (init once, stays up). The script only
# starts it if it is not already running.

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
for krw_binary in openssl curl uv /opt/homebrew/bin/initdb /opt/homebrew/bin/pg_ctl /opt/homebrew/bin/psql node npm; do
  if [[ "$krw_binary" == /* ]]; then
    [[ -x "$krw_binary" ]] || { printf 'missing required binary: %s\n' "$krw_binary" >&2; exit 1; }
  else
    command -v "$krw_binary" >/dev/null 2>&1 || { printf 'missing required binary: %s\n' "$krw_binary" >&2; exit 1; }
  fi
done

mkdir -p "$krw_state" "$krw_state/logs" "$krw_state/images" "$krw_state/artifacts"
chmod 700 "$krw_state" "$krw_state/logs" "$krw_state/images" "$krw_state/artifacts"
umask 077

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
export KRW_AGENT_GATEWAY_TOKEN KRW_AGENT_ARTIFACT_KEY_V1

cleanup() {
  set +e
  for krw_pid in "$krw_gateway_pid" "$krw_daemon_pid" "$krw_proxy_pid" "$krw_capability_pid"; do
    [[ -n "$krw_pid" ]] && kill -TERM "$krw_pid" 2>/dev/null
  done
  for krw_pid in "$krw_gateway_pid" "$krw_daemon_pid" "$krw_proxy_pid" "$krw_capability_pid"; do
    [[ -n "$krw_pid" ]] && wait "$krw_pid" 2>/dev/null
  done
  # PostgreSQL stays up across restarts; do not stop it here.
}
trap cleanup EXIT INT TERM

# PostgreSQL: reuse if already running, otherwise init+start.
# Once started the server stays up across script restarts. agentd connects
# with SslMode::Require, so a reused instance MUST accept TLS; a previously
# started server without ssl=on would make agentd fail readiness with an
# opaque ReadinessFailed hash. We probe TLS below and refuse to continue if
# the running server does not accept SSL.
krw_state_abs=$(CDPATH= cd -- "$krw_state" && pwd)
if [[ ! -f "$krw_state_abs/postgres/PG_VERSION" ]]; then
  /opt/homebrew/bin/initdb -A trust -D "$krw_state_abs/postgres" >"$krw_state_abs/logs/initdb.log"
fi
# Ensure CA + server cert/key exist. pg_ctl needs ABSOLUTE cert paths: when
# given a relative path Postgres resolves it against its own CWD, not the
# data directory, so an ssl=on start silently fails and leaves a server
# that agentd cannot talk to over TLS.
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
fi
# Reused-instance guard: agentd hard-requires SslMode::Require. If a server
# from an earlier run is up but not TLS-capable, stop+restart it with ssl=on
# using the absolute cert paths instead of silently inheriting a broken one.
if ! PGSSLMODE=verify-ca PGSSLROOTCERT="$krw_state_abs/ca.pem" \
    /opt/homebrew/bin/psql -X -h 127.0.0.1 -p "$krw_pg_port" -d postgres -tAc "select 1" >/dev/null 2>&1; then
  printf 'local PostgreSQL is up but does not accept TLS; restarting with ssl=on\n' >&2
  /opt/homebrew/bin/pg_ctl -D "$krw_state_abs/postgres" -m fast -w stop >/dev/null 2>&1
  /opt/homebrew/bin/pg_ctl -D "$krw_state_abs/postgres" -l "$krw_state_abs/logs/postgres.log" \
    -o "-F -p $krw_pg_port -h 127.0.0.1 -c ssl=on -c ssl_cert_file='$krw_state_abs/server.pem' -c ssl_key_file='$krw_state_abs/server.key'" \
    -w start >/dev/null
fi

# Production deployment creates database roles outside the migration runner,
# but a fresh local cluster has no such role.  Migration 0008 grants the
# bounded retention procedure to this no-login daemon role, so create exactly
# that inert role before the first migration pass.  The local worker itself
# still connects as the current OS user; this is only bootstrap parity with
# the documented production grant boundary.
PGSSLMODE=verify-ca PGSSLROOTCERT="$krw_state_abs/ca.pem" \
  /opt/homebrew/bin/psql -X -v ON_ERROR_STOP=1 -h 127.0.0.1 -p "$krw_pg_port" -d postgres \
  -c "DO \$\$ BEGIN IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'krw_agent_daemon') THEN CREATE ROLE krw_agent_daemon NOLOGIN; END IF; END \$\$;" \
  >/dev/null

PGSSLMODE=verify-ca PGSSLROOTCERT="$krw_state_abs/ca.pem" \
  "$krw_root/scripts/apply_migrations.sh" -X -h 127.0.0.1 -p "$krw_pg_port" -d postgres \
  >/dev/null

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

python3 - "$krw_root/deployments/local/deployment-binding.krw-ontology.example.yaml" "$krw_state/deployment-binding.yaml" "$krw_state/endpoint-registry.yaml" "$krw_build" "$krw_schema_hash" "$krw_release_hash" <<'PY'
import pathlib, sys
import json
source, output, endpoint_output, build, schema, release = sys.argv[1:]
text = pathlib.Path(source).read_text(encoding="utf-8")
text = text.replace("server_schema_bundle_hash: sha256:" + "0" * 64, "server_schema_bundle_hash: " + schema)
text = text.replace("server_build: fixture", "server_build: " + build)
text = text.replace("data_release_hash: sha256:" + "0" * 64, "data_release_hash: " + release)
pathlib.Path(output).write_text(text, encoding="utf-8")
endpoint = {
    "schema_version": 1,
    "registry_id": "local-loopback-runtime",
    "endpoints": [{
        "endpoint_ref": "krw-ontology-local",
        "url_env": "KRW_ONTOLOGY_MCP_URL",
        "readiness_url_env": "KRW_ONTOLOGY_READY_URL",
        "protocol_version": "2025-06-18",
        "origin": "https://krw-agent.local",
        "credential_version": "local-public-v1",
        "tls_profile": "system-plus-pinned-ca-v1",
        "tls_ca_pem_env": "KRW_ONTOLOGY_CA_PEM",
    }],
}
# YAML 1.2 is a JSON superset; the Rust YAML loader accepts this canonical,
# secret-free single-endpoint document without a second YAML dependency.
pathlib.Path(endpoint_output).write_text(json.dumps(endpoint, separators=(",", ":"), sort_keys=True), encoding="utf-8")
PY

(
  cd "$krw_root/services/krw-ontology-runtime"
  KRW_ONTOLOGY_ENV=prod KRW_ONTOLOGY_RELEASE_ROOT="$krw_release_root" \
  KRW_CAPABILITYD_HOST=127.0.0.1 KRW_CAPABILITYD_PORT="$krw_capability_port" \
  KRW_CAPABILITYD_EXPECTED_BUILD_ID="$krw_build" \
  KRW_CAPABILITYD_EXPECTED_TOOL_SCHEMA_SHA256="$krw_schema_hash" \
  KRW_CAPABILITYD_EXPECTED_RELEASE_MANIFEST_SHA256="$krw_release_hash" \
  exec uv run krw-capabilityd
) >"$krw_state/logs/capabilityd.log" 2>&1 &
krw_capability_pid=$!
for _ in $(seq 1 100); do curl --fail --silent "http://127.0.0.1:$krw_capability_port/healthz" >/dev/null && break; sleep 0.1; done
curl --fail --silent "http://127.0.0.1:$krw_capability_port/healthz" >/dev/null
python3 "$krw_root/scripts/local_tls_reverse_proxy.py" --listen-port "$krw_tls_port" --upstream-port "$krw_capability_port" \
  --certfile "$krw_state/server.pem" --keyfile "$krw_state/server.key" >"$krw_state/logs/mcp-tls-proxy.log" 2>&1 &
krw_proxy_pid=$!
for _ in $(seq 1 100); do curl --fail --silent --cacert "$krw_state/ca.pem" "https://127.0.0.1:$krw_tls_port/healthz" >/dev/null && break; sleep 0.1; done
curl --fail --silent --cacert "$krw_state/ca.pem" "https://127.0.0.1:$krw_tls_port/healthz" >/dev/null

krw_run_tag="$(date +%s)-$$"
krw_image_dir="$krw_state/images/krw-ontology-$krw_run_tag"
krw_descriptor="$krw_state/public-release-$krw_run_tag.json"
krw_release_authorization="$krw_state/release-authorization-$krw_run_tag.json"
"$krw_root/scripts/with_local_env.sh" "$krw_root/target/debug/krw-agent" image build --out "$krw_image_dir" "$krw_root/agents/krw-ontology" >/dev/null
krw_database_user=$(id -un)
export KRW_ONTOLOGY_MCP_URL="https://127.0.0.1:$krw_tls_port/mcp/"
export KRW_ONTOLOGY_READY_URL="https://127.0.0.1:$krw_tls_port/healthz"
export KRW_ONTOLOGY_CA_PEM="$(< "$krw_state/ca.pem")"
"$krw_root/scripts/with_local_env.sh" "$krw_root/target/debug/krw-agentd" \
  --image-dir "$krw_image_dir" --deployment-binding "$krw_state/deployment-binding.yaml" \
  --model-registry "$krw_root/deployments/local/model-registry.yaml" --budget-registry "$krw_root/deployments/local/budget-registry.yaml" \
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
krw_expiry=$((krw_now + 2592000))
"$krw_root/scripts/with_local_env.sh" "$krw_root/target/debug/krw-agent" release sign \
  --descriptor "$krw_descriptor" --private-key "$krw_state/release-private.pk8" --key-id local-dev-v1 --sequence 1 \
  --issued-at-unix-seconds "$krw_now" --expires-at-unix-seconds "$krw_expiry" --runtime-version 0.1.0 --kernel-version 0.1.0 \
  --out "$krw_release_authorization" >/dev/null

export KRW_AGENT_DATABASE_URL="postgresql://127.0.0.1:$krw_pg_port/postgres?user=$krw_database_user"
export KRW_AGENT_DATABASE_CA_PEM="$(< "$krw_state/ca.pem")"
export RUST_LOG="${RUST_LOG:-info}"
# Forward provider/MCP debug flags to agentd when set in the caller's env so
# DeepSeek SSE payloads and MCP arguments land in agentd.log for diagnosis.
export KRW_DEBUG_PROVIDER="${KRW_DEBUG_PROVIDER:-}"
export KRW_DEBUG_MCP_ARGS="${KRW_DEBUG_MCP_ARGS:-}"
# Run the pre-built agentd binary directly (not via cargo run) so the log
# stays clean and the process is a simple child of this script.
krw_agentd_bin="$krw_root/target/debug/krw-agentd"
"$krw_root/scripts/with_local_env.sh" "$krw_agentd_bin" \
    --image-dir "$krw_image_dir" --deployment-binding "$krw_state/deployment-binding.yaml" \
    --model-registry "$krw_root/deployments/local/model-registry.yaml" --budget-registry "$krw_root/deployments/local/budget-registry.yaml" \
    --endpoint-registry "$krw_state/endpoint-registry.yaml" --release-authorization "$krw_release_authorization" \
    --release-trust-registry "$krw_state/release-trust-registry.json" --runtime-version 0.1.0 --worker-id local-agentd \
    --database-url-env KRW_AGENT_DATABASE_URL --database-ca-pem-env KRW_AGENT_DATABASE_CA_PEM \
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
export KRW_AGENT_GATEWAY_HOST=127.0.0.1 KRW_AGENT_GATEWAY_PORT="$krw_gateway_port"
export KRW_AGENT_GATEWAY_TENANT_ID=local_tenant KRW_AGENT_GATEWAY_PRINCIPAL_ID=local_principal
export KRW_AGENT_GATEWAY_DATABASE_URL="$KRW_AGENT_DATABASE_URL"
export KRW_AGENT_GATEWAY_DATABASE_CA_PEM_FILE="$krw_state/ca.pem"
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
