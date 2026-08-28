#!/usr/bin/env bash
set -euo pipefail

# Resolve one unsigned provider bundle into a descriptor-producing production
# candidate. The command intentionally requires the operator's real endpoint
# and identity files; it never invents production fingerprints.

krw_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$krw_root"
krw_provider=
krw_bundle=
krw_binding=
krw_endpoints=
krw_descriptor=
krw_runtime_version=${KRW_AGENT_RUNTIME_VERSION:-0.1.0}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --provider) [[ $# -ge 2 ]] || { echo '--provider requires glm or deepseek' >&2; exit 2; }; krw_provider=$2; shift 2 ;;
    --bundle) [[ $# -ge 2 ]] || { echo '--bundle requires a directory' >&2; exit 2; }; krw_bundle=$2; shift 2 ;;
    --binding) [[ $# -ge 2 ]] || { echo '--binding requires a YAML file' >&2; exit 2; }; krw_binding=$2; shift 2 ;;
    --endpoints) [[ $# -ge 2 ]] || { echo '--endpoints requires a YAML file' >&2; exit 2; }; krw_endpoints=$2; shift 2 ;;
    --descriptor-output) [[ $# -ge 2 ]] || { echo '--descriptor-output requires a file' >&2; exit 2; }; krw_descriptor=$2; shift 2 ;;
    --runtime-version) [[ $# -ge 2 ]] || { echo '--runtime-version requires a value' >&2; exit 2; }; krw_runtime_version=$2; shift 2 ;;
    --help|-h)
      printf 'usage: %s --provider glm|deepseek --bundle DIR --binding FILE --endpoints FILE [--descriptor-output FILE] [--runtime-version VERSION]\n' "$0"
      exit 0
      ;;
    *) echo "unexpected argument: $1" >&2; exit 2 ;;
  esac
done

case "$krw_provider" in glm|deepseek) ;; *) echo 'provider must be glm or deepseek' >&2; exit 2 ;; esac
[[ "$krw_bundle" = /* && -d "$krw_bundle" && ! -L "$krw_bundle" ]] || { echo 'bundle must be an absolute real directory' >&2; exit 2; }
[[ "$krw_binding" = /* && -f "$krw_binding" && ! -L "$krw_binding" ]] || { echo 'binding must be an absolute regular file' >&2; exit 2; }
[[ "$krw_endpoints" = /* && -f "$krw_endpoints" && ! -L "$krw_endpoints" ]] || { echo 'endpoints must be an absolute regular file' >&2; exit 2; }
[[ -n "$krw_runtime_version" && "$krw_runtime_version" != *$'\n'* ]] || { echo 'runtime version is invalid' >&2; exit 2; }

krw_descriptor=${krw_descriptor:-$krw_bundle/public-release.json}
[[ "$krw_descriptor" = /* ]] || { echo 'descriptor output must be absolute' >&2; exit 2; }
krw_candidate_descriptor="$krw_bundle/public-release.json"
krw_deployments="$krw_bundle/deployments"
install -d -m 0755 "$krw_deployments"
install -m 0644 "$krw_binding" "$krw_deployments/deployment-binding.yaml"
install -m 0644 "$krw_endpoints" "$krw_deployments/endpoint-registry.yaml"

python3 - "$krw_root/scripts" "$krw_provider" "$krw_bundle" <<'PY'
import json
import pathlib
import re
import sys

sys.path.insert(0, sys.argv[1])
from release_provider import model_for_provider

provider, root_name = sys.argv[2], sys.argv[3]
root = pathlib.Path(root_name)
expected_model = model_for_provider(provider)
registry = (root / "deployments" / "model-registry.yaml").read_text(encoding="utf-8")
if f"model_id: {expected_model}" not in registry:
    raise SystemExit("provider registry does not contain the closed physical model")
if "model_id: glm-5.3-flash" in registry and provider != "glm":
    raise SystemExit("DeepSeek candidate contains the GLM registry")
if "model_id: deepseek-v4-flash" in registry and provider != "deepseek":
    raise SystemExit("GLM candidate contains the DeepSeek registry")
for relative in ("deployments/deployment-binding.yaml", "deployments/endpoint-registry.yaml"):
    text = (root / relative).read_text(encoding="utf-8")
    forbidden = (
        "REPLACE_WITH_",
        "server_build: fixture",
        "sha256:0000000000000000000000000000000000000000000000000000000000000000",
    )
    if any(token in text for token in forbidden):
        raise SystemExit(f"unresolved production placeholder in {relative}")
    if relative.endswith("endpoint-registry.yaml") and "origin: https://" not in text:
        raise SystemExit("production endpoints must use HTTPS origins")

# The ontology capability identity is produced by the sealed runtime itself.
# Materialize it into every ontology binding while preparing the candidate so
# operators never hand-edit three coupled hashes or accidentally bind the
# previous sidecar build.  Guru bindings ride the same capabilityd endpoint
# (that runtime derives the trusted light company context from the ontology
# release), so they receive the identical identity.  Feed/filings bindings
# remain independent services and are intentionally left unchanged.
identity_path = root / "capability-runtime" / "identity.json"
identity = json.loads(identity_path.read_text(encoding="utf-8"))
required = ("build_id", "tool_schema_sha256", "release_manifest_sha256")
if any(not isinstance(identity.get(field), str) or not identity[field] for field in required):
    raise SystemExit("sealed capability identity is incomplete")
binding_path = root / "deployments" / "deployment-binding.yaml"
binding_text = binding_path.read_text(encoding="utf-8")
blocks = re.split(r"(?=^  - binding_key: )", binding_text, flags=re.M)
updated = []
ontology_count = 0
for block in blocks:
    if re.search(r"(?m)^    endpoint_ref: krw-ontology-local$", block):
        for field, value in (
            ("server_build", identity["build_id"]),
            ("server_schema_bundle_hash", identity["tool_schema_sha256"]),
            ("data_release_hash", identity["release_manifest_sha256"]),
        ):
            block, count = re.subn(rf"(?m)^    {field}: .*?$", f"    {field}: {value}", block)
            if count != 1:
                raise SystemExit(f"ontology binding is missing exactly one {field}")
        ontology_count += 1
    updated.append(block)
if ontology_count == 0:
    raise SystemExit("production binding has no ontology endpoint")
binding_path.write_text("".join(updated), encoding="utf-8")
PY

krw_bin="$krw_bundle/bin/krw-agentd"
[[ -x "$krw_bin" ]] || { echo 'candidate does not contain executable krw-agentd' >&2; exit 1; }
krw_image_args=()
for krw_image in krw-ontology krw-ontology-en krw-feed krw-source-filing krw-guru-advisor krw-router krw-display krw-notebook; do
  krw_image_args+=(--image-dir "$krw_bundle/images/$krw_image")
  [[ -d "$krw_bundle/images/$krw_image" ]] || { echo "missing image: $krw_image" >&2; exit 1; }
done

krw_descriptor_parent=$(dirname -- "$krw_descriptor")
mkdir -p -- "$krw_descriptor_parent"
"$krw_bin" \
  "${krw_image_args[@]}" \
  --deployment-binding "$krw_deployments/deployment-binding.yaml" \
  --model-registry "$krw_deployments/model-registry.yaml" \
  --provider "$krw_provider" \
  --budget-registry "$krw_deployments/budget-registry.yaml" \
  --endpoint-registry "$krw_deployments/endpoint-registry.yaml" \
  --runtime-version "$krw_runtime_version" \
  --public-release-descriptor-output "$krw_descriptor" \
  --check

if [[ "$krw_descriptor" != "$krw_candidate_descriptor" ]]; then
  install -m 0644 "$krw_descriptor" "$krw_candidate_descriptor"
fi
chmod 0644 "$krw_candidate_descriptor"
printf 'production candidate prepared for %s: %s\n' "$krw_provider" "$krw_bundle"
