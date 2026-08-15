#!/usr/bin/env bash
set -euo pipefail

# Build, configure, sign, and finalize both provider bundles as one new
# operator-owned release. The private key remains in the operator directory;
# it is never copied into the release or printed by this script.

krw_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd -P)
krw_output_root=
krw_front_contract=
krw_operator_root=${KRW_AGENT_OPERATOR_ROOT:-"$HOME/krw-agnet-prod"}
krw_runtime_env=${KRW_AGENT_RUNTIME_ENV_FILE:-}
krw_runtime_version=${KRW_AGENT_RUNTIME_VERSION:-0.1.0}
krw_kernel_version=${KRW_AGENT_KERNEL_VERSION:-0.1.0}
krw_authorization_ttl=${KRW_AGENT_RELEASE_AUTH_TTL_SECONDS:-2592000}

usage() {
  cat <<'EOF'
Usage: scripts/build_sealed_dual_provider_release.sh \
  --output-root /absolute/new/release-directory \
  [--front-contract /absolute/agent-v1-deployment-contract.json] \
  [--operator-root /absolute/operator-directory] \
  [--runtime-env /absolute/runtime.env]

Builds a fresh GLM+DeepSeek release, applies the operator-owned endpoint
bindings, signs each exact descriptor, verifies both bundles, and writes the
final dual-release index. It requires a clean committed krw-agnet source tree.
EOF
}

fail() {
  printf '%s\n' "$1" >&2
  exit 1
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --output-root)
      [[ $# -ge 2 ]] || fail "--output-root requires an absolute directory"
      krw_output_root=$2
      shift 2
      ;;
    --front-contract)
      [[ $# -ge 2 ]] || fail "--front-contract requires an absolute file"
      krw_front_contract=$2
      shift 2
      ;;
    --operator-root)
      [[ $# -ge 2 ]] || fail "--operator-root requires an absolute directory"
      krw_operator_root=$2
      shift 2
      ;;
    --runtime-env)
      [[ $# -ge 2 ]] || fail "--runtime-env requires an absolute file"
      krw_runtime_env=$2
      shift 2
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *)
      usage >&2
      exit 2
      ;;
  esac
done

[[ "$krw_output_root" = /* && -n "$krw_output_root" ]] \
  || fail "--output-root must be an absolute directory"
[[ ! -e "$krw_output_root" ]] \
  || fail "release output already exists: $krw_output_root"
if [[ -n "$krw_front_contract" ]]; then
  [[ "$krw_front_contract" = /* && -f "$krw_front_contract" && ! -L "$krw_front_contract" ]] \
    || fail "front contract must be an existing absolute regular file"
fi
[[ "$krw_operator_root" = /* && -d "$krw_operator_root" && ! -L "$krw_operator_root" ]] \
  || fail "operator root must be an existing absolute real directory"
krw_runtime_env=${krw_runtime_env:-"$krw_operator_root/runtime/krw-agent-deploy.env"}
[[ "$krw_runtime_env" = /* && -f "$krw_runtime_env" && ! -L "$krw_runtime_env" ]] \
  || fail "runtime env must be an existing absolute regular file"
case "$krw_authorization_ttl" in
  ''|*[!0-9]*) fail "KRW_AGENT_RELEASE_AUTH_TTL_SECONDS must be a positive integer" ;;
  *) [[ "$krw_authorization_ttl" -gt 0 ]] || fail "KRW_AGENT_RELEASE_AUTH_TTL_SECONDS must be positive" ;;
esac

krw_release_name=$(basename -- "$krw_output_root")
case "$krw_release_name" in
  ''|*[!A-Za-z0-9._-]*) fail "release directory name is invalid" ;;
esac

krw_private_key="$krw_operator_root/signing/release-private.pk8"
[[ -f "$krw_private_key" && ! -L "$krw_private_key" ]] \
  || fail "operator signing key is missing or unsafe: $krw_private_key"

cd "$krw_root"
[[ -z "$(git status --porcelain=v1 --untracked-files=normal)" ]] \
  || fail "sealed dual-provider release requires a clean committed Git tree"

if [[ -n "$krw_front_contract" ]]; then
  python3 "$krw_root/scripts/verify_frontend_deployment_contract.py" \
    --contract "$krw_front_contract"
fi

# Descriptor preparation validates the resolved local MCP endpoints. Keep the
# values in the operator-owned runtime envelope and export them only to this
# short-lived packaging process; neither the release nor its logs contain
# those credentials.
set -a
# shellcheck disable=SC1090
. "$krw_runtime_env"
set +a

krw_now=$(date +%s)
krw_expires_at=$((krw_now + krw_authorization_ttl))
krw_sequence=$krw_now
krw_authorization_root="$krw_operator_root/signing/releases/$krw_release_name"
[[ ! -e "$krw_authorization_root" ]] \
  || fail "operator authorization directory already exists: $krw_authorization_root"

"$krw_root/scripts/build_dual_provider_release.sh" \
  --output-root "$krw_output_root"

mkdir -p -- "$krw_authorization_root"
chmod 700 "$krw_authorization_root"

for krw_provider in glm deepseek; do
  krw_config_root="$krw_operator_root/config/$krw_provider"
  krw_binding="$krw_config_root/deployment-binding.yaml"
  krw_endpoints="$krw_config_root/endpoint-registry.yaml"
  krw_trust_registry="$krw_config_root/release-trust-registry.json"
  krw_authorization="$krw_authorization_root/release-authorization-$krw_provider.json"
  krw_bundle="$krw_output_root/$krw_provider"

  for krw_required in "$krw_binding" "$krw_endpoints" "$krw_trust_registry"; do
    [[ -f "$krw_required" && ! -L "$krw_required" ]] \
      || fail "operator configuration is missing or unsafe: $krw_required"
  done

  krw_key_id=$(python3 - "$krw_trust_registry" "$krw_now" <<'PY'
import json
import sys

path, now_text = sys.argv[1:]
now = int(now_text)
value = json.load(open(path, encoding="utf-8"))
if value.get("schema_version") != 1:
    raise SystemExit("trust registry schema must be 1")
keys = [
    key for key in value.get("keys", [])
    if key.get("revoked") is False
    and isinstance(key.get("key_id"), str)
    and key["key_id"]
    and isinstance(key.get("not_before_unix_seconds"), int)
    and isinstance(key.get("not_after_unix_seconds"), int)
    and key["not_before_unix_seconds"] <= now <= key["not_after_unix_seconds"]
]
if len(keys) != 1:
    raise SystemExit("trust registry must have exactly one active signing key")
print(keys[0]["key_id"])
PY
)

  "$krw_root/scripts/prepare_production_candidate.sh" \
    --provider "$krw_provider" \
    --bundle "$krw_bundle" \
    --binding "$krw_binding" \
    --endpoints "$krw_endpoints"

  "$krw_bundle/bin/krw-agent" release sign \
    --descriptor "$krw_bundle/public-release.json" \
    --private-key "$krw_private_key" \
    --key-id "$krw_key_id" \
    --sequence "$krw_sequence" \
    --issued-at-unix-seconds "$krw_now" \
    --expires-at-unix-seconds "$krw_expires_at" \
    --runtime-version "$krw_runtime_version" \
    --kernel-version "$krw_kernel_version" \
    --out "$krw_authorization" >/dev/null
  chmod 600 "$krw_authorization"

  "$krw_root/scripts/seal_production_candidate.sh" \
    --provider "$krw_provider" \
    --candidate "$krw_bundle" \
    --authorization "$krw_authorization" \
    --trust-registry "$krw_trust_registry"
done

"$krw_root/scripts/finalize_dual_provider_release.sh" \
  --release-root "$krw_output_root"

printf 'sealed dual-provider release ready: %s\n' "$krw_output_root"
