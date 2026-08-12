#!/usr/bin/env bash
set -euo pipefail

# Add the independently signed authorization/trust files, verify the exact
# provider release, and only then write the final manifest and front runtime
# pins. The signing private key is never read by this script.

krw_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$krw_root"
krw_provider=
krw_bundle=
krw_authorization=
krw_trust_registry=
krw_runtime_version=${KRW_AGENT_RUNTIME_VERSION:-0.1.0}
krw_kernel_version=${KRW_AGENT_KERNEL_VERSION:-0.1.0}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --provider) [[ $# -ge 2 ]] || { echo '--provider requires glm or deepseek' >&2; exit 2; }; krw_provider=$2; shift 2 ;;
    --candidate|--bundle) [[ $# -ge 2 ]] || { echo '--candidate requires a directory' >&2; exit 2; }; krw_bundle=$2; shift 2 ;;
    --authorization) [[ $# -ge 2 ]] || { echo '--authorization requires a file' >&2; exit 2; }; krw_authorization=$2; shift 2 ;;
    --trust-registry) [[ $# -ge 2 ]] || { echo '--trust-registry requires a file' >&2; exit 2; }; krw_trust_registry=$2; shift 2 ;;
    --runtime-version) [[ $# -ge 2 ]] || { echo '--runtime-version requires a value' >&2; exit 2; }; krw_runtime_version=$2; shift 2 ;;
    --kernel-version) [[ $# -ge 2 ]] || { echo '--kernel-version requires a value' >&2; exit 2; }; krw_kernel_version=$2; shift 2 ;;
    --help|-h)
      printf 'usage: %s --provider glm|deepseek --candidate DIR --authorization FILE --trust-registry FILE [--runtime-version VERSION] [--kernel-version VERSION]\n' "$0"
      exit 0
      ;;
    *) echo "unexpected argument: $1" >&2; exit 2 ;;
  esac
done

case "$krw_provider" in glm|deepseek) ;; *) echo 'provider must be glm or deepseek' >&2; exit 2 ;; esac
[[ "$krw_bundle" = /* && -d "$krw_bundle" && ! -L "$krw_bundle" ]] || { echo 'candidate must be an absolute real directory' >&2; exit 2; }
[[ "$krw_authorization" = /* && -f "$krw_authorization" && ! -L "$krw_authorization" ]] || { echo 'authorization must be an absolute regular file' >&2; exit 2; }
[[ "$krw_trust_registry" = /* && -f "$krw_trust_registry" && ! -L "$krw_trust_registry" ]] || { echo 'trust registry must be an absolute regular file' >&2; exit 2; }

krw_deployments="$krw_bundle/deployments"
krw_bin="$krw_bundle/bin/krw-agent"
krw_daemon="$krw_bundle/bin/krw-agentd"
[[ -x "$krw_bin" && -x "$krw_daemon" ]] || { echo 'candidate binaries are missing' >&2; exit 1; }
[[ -f "$krw_bundle/public-release.json" ]] || { echo 'public-release.json is missing; prepare the candidate first' >&2; exit 1; }

install -m 0644 "$krw_authorization" "$krw_bundle/release-authorization.json"
install -m 0644 "$krw_trust_registry" "$krw_bundle/release-trust-registry.json"

"$krw_bin" release verify \
  --descriptor "$krw_bundle/public-release.json" \
  --authorization "$krw_bundle/release-authorization.json" \
  --trust-registry "$krw_bundle/release-trust-registry.json" \
  --runtime-version "$krw_runtime_version" \
  --kernel-version "$krw_kernel_version"

python3 - "$krw_root/scripts" "$krw_provider" "$krw_bundle" <<'PY'
import pathlib
import sys

sys.path.insert(0, sys.argv[1])
from release_provider import model_for_provider

provider, root_name = sys.argv[2], sys.argv[3]
root = pathlib.Path(root_name)
expected_model = model_for_provider(provider)
registry = (root / "deployments" / "model-registry.yaml").read_text(encoding="utf-8")
if f"model_id: {expected_model}" not in registry:
    raise SystemExit("provider registry/model mismatch")
for relative in ("deployments/deployment-binding.yaml", "deployments/endpoint-registry.yaml"):
    text = (root / relative).read_text(encoding="utf-8")
    if any(token in text for token in (
        "REPLACE_WITH_",
        "server_build: fixture",
        "sha256:0000000000000000000000000000000000000000000000000000000000000000",
    )):
        raise SystemExit(f"unresolved production placeholder in {relative}")
PY

krw_image_args=()
for krw_image in krw-ontology krw-ontology-en krw-feed krw-source-filing krw-guru-advisor krw-router krw-display krw-notebook; do
  krw_image_args+=(--image-dir "$krw_bundle/images/$krw_image")
  [[ -d "$krw_bundle/images/$krw_image" ]] || { echo "missing image: $krw_image" >&2; exit 1; }
done
"$krw_daemon" \
  "${krw_image_args[@]}" \
  --deployment-binding "$krw_deployments/deployment-binding.yaml" \
  --model-registry "$krw_deployments/model-registry.yaml" \
  --provider "$krw_provider" \
  --budget-registry "$krw_deployments/budget-registry.yaml" \
  --endpoint-registry "$krw_deployments/endpoint-registry.yaml" \
  --release-authorization "$krw_bundle/release-authorization.json" \
  --release-trust-registry "$krw_bundle/release-trust-registry.json" \
  --runtime-version "$krw_runtime_version" \
  --check

read -r krw_commit krw_tree < <(python3 - "$krw_bundle/release-manifest.json" <<'PY'
import json
import sys
manifest = json.loads(open(sys.argv[1], encoding="utf-8").read())
print(manifest["git_commit"], manifest["git_tree"])
PY
)
read -r krw_descriptor_hash krw_release_set_hash < <(python3 - "$krw_bundle/public-release.json" <<'PY'
import hashlib
import json
import sys
path = sys.argv[1]
raw = open(path, "rb").read()
descriptor = json.loads(raw.decode("utf-8"))
print("sha256:" + hashlib.sha256(raw).hexdigest(), descriptor["release_set_hash"])
PY
)
umask 022
printf '%s\n' \
  "KRW_AGENT_PROVIDER=$krw_provider" \
  'KRW_AGENT_RELEASE_DESCRIPTOR_PATH=/run/krw-agent/public-release.json' \
  "KRW_AGENT_RELEASE_ARTIFACT_HASH=$krw_descriptor_hash" \
  "KRW_AGENT_RELEASE_SET_HASH=$krw_release_set_hash" \
  "KRW_RUNTIME_ENVIRONMENT=prod" > "$krw_bundle/frontend-runtime.env"
chmod 0644 "$krw_bundle/frontend-runtime.env"
python3 "$krw_root/scripts/write_release_manifest.py" \
  --root "$krw_bundle" \
  --git-commit "$krw_commit" \
  --git-tree "$krw_tree" \
  --provider "$krw_provider"
python3 "$krw_root/scripts/verify_standalone_release.py" --root "$krw_bundle" >/dev/null
printf 'sealed production candidate for %s: %s\n' "$krw_provider" "$krw_bundle"
