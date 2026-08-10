#!/usr/bin/env bash
set -euo pipefail

# Runs one isolated, opt-in acceptance of the real critical path:
# DeepSeek Flash -> TLS MCP -> immutable ontology release -> TLS PostgreSQL
# -> atomic final/outbox.  It never starts or changes the existing MCP/web app.
#
# The generated directory is intentionally retained for post-failure inspection.
# It contains only ephemeral test data and a short-lived local test certificate;
# no DeepSeek key is written there.

krw_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
krw_tmp=$(mktemp -d /tmp/krw-agent-live-e2e.XXXXXX)
krw_nonce=$(( $$ % 1000 ))
krw_pg_port=$(( 55000 + krw_nonce ))
krw_capability_port=$(( 19000 + krw_nonce ))
krw_tls_port=$(( 20000 + krw_nonce ))
krw_live_run_id=${KRW_LIVE_E2E_RUN_ID:-live-e2e-aapl-v1}
krw_live_question=${KRW_LIVE_E2E_QUESTION:-'AAPL의 매출 추이를 최근 10-K 공시 근거로 간단히 설명해줘'}
krw_live_ticker=${KRW_LIVE_E2E_TICKER:-AAPL}
krw_capability_pid=''
krw_proxy_pid=''
krw_pg_started=false

cleanup() {
  set +e
  if [[ -n "$krw_proxy_pid" ]] && kill -0 "$krw_proxy_pid" 2>/dev/null; then
    kill -TERM "$krw_proxy_pid" 2>/dev/null
    wait "$krw_proxy_pid" 2>/dev/null
  fi
  if [[ -n "$krw_capability_pid" ]] && kill -0 "$krw_capability_pid" 2>/dev/null; then
    kill -TERM "$krw_capability_pid" 2>/dev/null
    wait "$krw_capability_pid" 2>/dev/null
  fi
  if [[ "$krw_pg_started" == true ]]; then
    /opt/homebrew/bin/pg_ctl -D "$krw_tmp/postgres" -m immediate stop >/dev/null 2>&1
  fi
  printf 'live E2E temporary evidence retained at: %s\n' "$krw_tmp" >&2
}
trap cleanup EXIT

[[ "$krw_live_run_id" =~ ^[A-Za-z0-9_-]{1,128}$ ]] || {
  printf 'KRW_LIVE_E2E_RUN_ID must be a safe bounded identifier\n' >&2
  exit 2
}
[[ "$krw_live_ticker" =~ ^[A-Z0-9][A-Z0-9.-]{0,31}$ ]] || {
  printf 'KRW_LIVE_E2E_TICKER must be a canonical uppercase ticker\n' >&2
  exit 2
}
[[ -n "$krw_live_question" ]] || {
  printf 'KRW_LIVE_E2E_QUESTION must be non-empty\n' >&2
  exit 2
}

for krw_binary in openssl curl uv /opt/homebrew/bin/initdb /opt/homebrew/bin/pg_ctl /opt/homebrew/bin/psql; do
  if [[ "$krw_binary" == /* ]]; then
    [[ -x "$krw_binary" ]] || { printf 'missing required binary: %s\n' "$krw_binary" >&2; exit 1; }
  else
    command -v "$krw_binary" >/dev/null 2>&1 || { printf 'missing required binary: %s\n' "$krw_binary" >&2; exit 1; }
  fi
done

# The sidecar build ID intentionally commits to every local runtime source
# file.  Do not hand-maintain it here: first derive the identity from the
# exact immutable release and then pin that value for both sidecar admission
# and Rust's deployment binding.  The tool schema remains an explicit ABI
# pin.  The data-release hash is supplied by the caller, rather than copied
# into this script: accepting whatever `current` happens to point at would
# defeat the immutable-release admission this test is meant to prove.
readonly krw_expected_tool_schema_sha256=sha256:66517f6128c616225ef6d961344dc63440d73b30d121baf0485b3d33f720679b
krw_ontology_release_pointer=${KRW_LIVE_ONTOLOGY_RELEASE_ROOT:-~/krw-ontology-data/releases/prod/current}
krw_expected_release_manifest_sha256=${KRW_LIVE_EXPECTED_RELEASE_MANIFEST_SHA256:-}
[[ "$krw_ontology_release_pointer" == /* ]] || {
  printf 'KRW_LIVE_ONTOLOGY_RELEASE_ROOT must be an absolute release pointer\n' >&2
  exit 2
}
[[ -d "$krw_ontology_release_pointer" ]] || {
  printf 'ontology release pointer is not a directory: %s\n' "$krw_ontology_release_pointer" >&2
  exit 2
}
# Pass the `env/current` pointer only to the capability daemon's admission
# boundary. `prepare_mcp_runtime` verifies that pointer did not rotate while
# resolving it, then rewrites its own runtime paths to the physical immutable
# directory. The expected identity below prevents a rotation between this
# probe and the actual sidecar startup from serving a different release.
read -r krw_capability_build krw_capability_tool_schema_sha256 krw_capability_release_manifest_sha256 < <(
  (
    cd "$krw_root/services/krw-ontology-runtime"
    KRW_ONTOLOGY_ENV=prod \
    KRW_ONTOLOGY_RELEASE_ROOT="$krw_ontology_release_pointer" \
    uv run krw-capabilityd --print-identity
  ) | python3 -c '
import json
import sys

identity = json.load(sys.stdin)
fields = ("build_id", "tool_schema_sha256", "release_manifest_sha256")
if set(identity) != {
    "schema_version", "ok", "fingerprint_match", "service", "transport",
    "protocol_version", "build_id", "tool_schema_sha256",
    "release_manifest_sha256", "tool_count",
} or identity.get("ok") is not True:
    raise SystemExit("invalid capability identity")
values = [identity.get(field) for field in fields]
if not all(isinstance(value, str) and value for value in values):
    raise SystemExit("incomplete capability identity")
print(" ".join(values))
'
)
[[ -n "$krw_capability_build" ]] || { printf 'capability build identity missing\n' >&2; exit 1; }
[[ "$krw_capability_tool_schema_sha256" == "$krw_expected_tool_schema_sha256" ]] || {
  printf 'unexpected capability tool schema pin\n' >&2
  exit 1
}
[[ "$krw_expected_release_manifest_sha256" =~ ^sha256:[0-9a-f]{64}$ ]] || {
  printf 'set KRW_LIVE_EXPECTED_RELEASE_MANIFEST_SHA256 to the approved immutable manifest hash; observed: %s\n' \
    "$krw_capability_release_manifest_sha256" >&2
  exit 2
}
[[ "$krw_capability_release_manifest_sha256" == "$krw_expected_release_manifest_sha256" ]] || {
  printf 'unexpected capability release manifest pin (expected %s, observed %s)\n' \
    "$krw_expected_release_manifest_sha256" "$krw_capability_release_manifest_sha256" >&2
  exit 1
}

openssl req -x509 -newkey rsa:2048 -nodes -sha256 -days 1 \
  -config "$krw_root/scripts/live_e2e_openssl.cnf" \
  -extensions certificate_authority_extensions \
  -keyout "$krw_tmp/ca.key" -out "$krw_tmp/ca.pem" >/dev/null 2>&1
openssl req -new -newkey rsa:2048 -nodes -sha256 \
  -config "$krw_root/scripts/live_e2e_openssl.cnf" \
  -reqexts request_extensions \
  -keyout "$krw_tmp/server.key" -out "$krw_tmp/server.csr" >/dev/null 2>&1
openssl x509 -req -sha256 -days 1 \
  -in "$krw_tmp/server.csr" \
  -CA "$krw_tmp/ca.pem" -CAkey "$krw_tmp/ca.key" -CAcreateserial \
  -extfile "$krw_root/scripts/live_e2e_openssl.cnf" \
  -extensions server_certificate_extensions \
  -out "$krw_tmp/server.pem" >/dev/null 2>&1
chmod 600 "$krw_tmp/server.key"

/opt/homebrew/bin/initdb -A trust -D "$krw_tmp/postgres" >"$krw_tmp/initdb.log"
/opt/homebrew/bin/pg_ctl \
  -D "$krw_tmp/postgres" \
  -l "$krw_tmp/postgres.log" \
  -o "-F -p $krw_pg_port -h 127.0.0.1 -c ssl=on -c ssl_cert_file='$krw_tmp/server.pem' -c ssl_key_file='$krw_tmp/server.key'" \
  -w start >/dev/null
krw_pg_started=true

for krw_migration in \
  migrations/0001_agent_v1.sql \
  migrations/0002_action_finalization.sql \
  migrations/0003_bounded_child.sql \
  migrations/0004_session_memory.sql \
  migrations/0005_session_memory_snapshot.sql \
  migrations/0006_read_final_output.sql
do
  PGSSLMODE=disable /opt/homebrew/bin/psql -X -v ON_ERROR_STOP=1 \
    -h 127.0.0.1 -p "$krw_pg_port" -d postgres -f "$krw_root/$krw_migration" >/dev/null
done

(
  cd "$krw_root/services/krw-ontology-runtime"
  KRW_ONTOLOGY_ENV=prod \
  KRW_ONTOLOGY_RELEASE_ROOT="$krw_ontology_release_pointer" \
  KRW_CAPABILITYD_HOST=127.0.0.1 \
  KRW_CAPABILITYD_PORT="$krw_capability_port" \
  KRW_CAPABILITYD_EXPECTED_BUILD_ID="$krw_capability_build" \
  KRW_CAPABILITYD_EXPECTED_TOOL_SCHEMA_SHA256="$krw_capability_tool_schema_sha256" \
  KRW_CAPABILITYD_EXPECTED_RELEASE_MANIFEST_SHA256="$krw_capability_release_manifest_sha256" \
  exec "$krw_root/scripts/with_local_env.sh" --market-sidecar uv run krw-capabilityd
) >"$krw_tmp/capabilityd.log" 2>&1 &
krw_capability_pid=$!

for _ in $(seq 1 100); do
  if curl --fail --silent "http://127.0.0.1:$krw_capability_port/healthz" >/dev/null; then
    break
  fi
  sleep 0.1
done
curl --fail --silent "http://127.0.0.1:$krw_capability_port/healthz" >/dev/null

python3 "$krw_root/scripts/local_tls_reverse_proxy.py" \
  --listen-port "$krw_tls_port" \
  --upstream-port "$krw_capability_port" \
  --certfile "$krw_tmp/server.pem" \
  --keyfile "$krw_tmp/server.key" \
  >"$krw_tmp/tls-proxy.log" 2>&1 &
krw_proxy_pid=$!

for _ in $(seq 1 100); do
  if curl --fail --silent --cacert "$krw_tmp/server.pem" "https://127.0.0.1:$krw_tls_port/healthz" >/dev/null; then
    break
  fi
  sleep 0.1
done
curl --fail --silent --cacert "$krw_tmp/server.pem" "https://127.0.0.1:$krw_tls_port/healthz" >/dev/null

krw_database_user=$(id -un)
KRW_LIVE_E2E_ACCEPTANCE=1 \
KRW_LIVE_E2E_RUN_ID="$krw_live_run_id" \
KRW_LIVE_E2E_QUESTION="$krw_live_question" \
KRW_LIVE_E2E_TICKER="$krw_live_ticker" \
KRW_LIVE_E2E_ARTIFACT_ROOT="$krw_tmp/artifacts" \
KRW_LIVE_E2E_DATABASE_URL="postgresql://127.0.0.1:$krw_pg_port/postgres?user=$krw_database_user" \
KRW_LIVE_E2E_DATABASE_CA_PEM="$(< "$krw_tmp/ca.pem")" \
KRW_ONTOLOGY_MCP_URL="https://127.0.0.1:$krw_tls_port/mcp/" \
KRW_ONTOLOGY_READY_URL="https://127.0.0.1:$krw_tls_port/healthz" \
KRW_ONTOLOGY_CA_PEM="$(< "$krw_tmp/ca.pem")" \
KRW_LIVE_MCP_SERVER_BUILD="$krw_capability_build" \
KRW_LIVE_MCP_TOOL_SCHEMA_SHA256="$krw_capability_tool_schema_sha256" \
KRW_LIVE_MCP_RELEASE_MANIFEST_SHA256="$krw_capability_release_manifest_sha256" \
"$krw_root/scripts/with_local_env.sh" \
  cargo test -p krw-agent-runtime-persistence --test live_end_to_end_acceptance -- --nocapture

krw_atomic_shape=$(PGSSLMODE=disable /opt/homebrew/bin/psql -X -A -t -v ON_ERROR_STOP=1 \
  -h 127.0.0.1 -p "$krw_pg_port" -d postgres -c "
    SELECT r.state || '|' ||
      (SELECT count(*)::text FROM agent_store.answer_bundles b WHERE b.run_id=r.run_id) || '|' ||
      (SELECT count(*)::text FROM agent_store.settlements s WHERE s.run_id=r.run_id) || '|' ||
      (SELECT count(*)::text FROM agent_store.outbox o WHERE o.run_id=r.run_id AND o.event_kind='answer.committed') || '|' ||
      (SELECT count(*)::text FROM agent_store.outbox o WHERE o.run_id=r.run_id AND o.event_kind='answer.billing') || '|' ||
      (SELECT count(*)::text FROM agent_store.outbox o WHERE o.run_id=r.run_id AND o.event_kind='answer.presentation')
    FROM agent_store.runs r
    WHERE r.run_id='$krw_live_run_id';" | tr -d '[:space:]')
if [[ "$krw_atomic_shape" != 'final|1|1|1|1|1' ]]; then
  printf 'unexpected atomic-final row shape: %s\n' "$krw_atomic_shape" >&2
  exit 1
fi
krw_final_bundle_shape=$(PGSSLMODE=disable /opt/homebrew/bin/psql -X -A -t -v ON_ERROR_STOP=1 \
  -h 127.0.0.1 -p "$krw_pg_port" -d postgres -c "
    SELECT b.final_output_hash || '|' ||
      COALESCE(b.answer_bundle->'output_contract'->>'id','') || '|' ||
      jsonb_typeof(b.answer_bundle->'output') || '|' ||
      CASE WHEN b.answer_bundle->'answer_ir' = 'null'::jsonb THEN 'null' ELSE 'present' END || '|' ||
      CASE
        WHEN b.answer_bundle->>'evidence_ledger_hash' ~ '^sha256:[0-9a-f]{64}$'
         AND jsonb_typeof(b.answer_bundle->'evidence_ids') = 'array'
         AND jsonb_array_length(b.answer_bundle->'evidence_ids') > 0
        THEN 'bound'
        ELSE 'invalid'
      END
    FROM agent_store.answer_bundles b
    WHERE b.run_id='$krw_live_run_id';" | tr -d '[:space:]')
if [[ ! "$krw_final_bundle_shape" =~ ^sha256:[0-9a-f]{64}\|final-markdown/v1\|string\|null\|bound$ ]]; then
  printf 'unexpected direct-Markdown final bundle shape: %s\n' "$krw_final_bundle_shape" >&2
  exit 1
fi
printf 'live E2E acceptance: ok (run_id=%s final|answer_bundle|settlement|outbox=%s)\n' \
  "$krw_live_run_id" "$krw_atomic_shape"
