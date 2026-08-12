#!/usr/bin/env bash
set -euo pipefail

# Build common Rust binaries and AgentImages exactly once, then materialize
# independently verifiable GLM and DeepSeek candidates from that same source
# tree. Provider-specific registry and manifest bytes remain separate.

krw_release_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
krw_output_root=

while [[ $# -gt 0 ]]; do
  case "$1" in
    --output-root)
      [[ $# -ge 2 ]] || { printf '%s\n' '--output-root requires an absolute directory' >&2; exit 2; }
      krw_output_root=$2
      shift 2
      ;;
    --help|-h)
      printf 'usage: %s --output-root /absolute/new/output-directory\n' "$0"
      exit 0
      ;;
    *)
      printf 'unexpected argument: %s\n' "$1" >&2
      exit 2
      ;;
  esac
done

if [[ -z "$krw_output_root" || "$krw_output_root" != /* ]]; then
  printf 'usage: %s --output-root /absolute/new/output-directory\n' "$0" >&2
  exit 2
fi
if [[ -e "$krw_output_root" ]]; then
  printf 'release output already exists: %s\n' "$krw_output_root" >&2
  exit 2
fi

cd "$krw_release_root"
if [[ -n "$(git status --porcelain=v1 --untracked-files=normal)" ]]; then
  printf '%s\n' 'dual provider release requires a clean committed Git tree' >&2
  exit 1
fi

export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
export SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH:-0}

krw_parent=$(dirname -- "$krw_output_root")
mkdir -p -- "$krw_parent"
krw_staging=$(mktemp -d "$krw_parent/.krw-dual-release.XXXXXX")
krw_common="$krw_staging/common"
krw_cleanup=true
trap 'if [[ "$krw_cleanup" == true ]]; then rm -rf -- "$krw_staging"; fi' EXIT

install -d -m 0755 "$krw_common/bin" "$krw_common/images" \
  "$krw_common/migrations" "$krw_common/docs" "$krw_common/host-ts" \
  "$krw_common/packaging"

cargo build --locked --profile production -p krw-agent -p krw-agentd
install -m 0755 target/production/krw-agent "$krw_common/bin/krw-agent"
install -m 0755 target/production/krw-agentd "$krw_common/bin/krw-agentd"

while IFS= read -r krw_agent_package; do
  target/production/krw-agent image build \
    "agents/$krw_agent_package" --out "$krw_common/images/$krw_agent_package"
  target/production/krw-agent image verify "$krw_common/images/$krw_agent_package"
done < <(awk -F '\t' '!/^#/ && NF { print $1 }' agents/fixtures/entrypoints.tsv | LC_ALL=C sort -u)

install -m 0644 migrations/*.sql "$krw_common/migrations/"
install -m 0755 scripts/apply_migrations.sh "$krw_common/apply_migrations.sh"
install -m 0644 README.md TODOS.md "$krw_common/docs/"
install -m 0644 docs/IMPLEMENTATION_STATUS.md docs/POSTGRES_RUNTIME.md \
  docs/PERFORMANCE_RELEASE_GATES.md docs/RELEASE_AUTHORIZATION.md \
  docs/DUAL_PROVIDER_PRODUCTION_RUNBOOK.md "$krw_common/docs/"
install -m 0644 packages/host-ts/package.json packages/host-ts/package-lock.json \
  packages/host-ts/tsconfig.json packages/host-ts/README.md packages/host-ts/INTEGRATION.md \
  "$krw_common/host-ts/"
cp -R packages/host-ts/src packages/host-ts/test "$krw_common/host-ts/"
cp -R packaging/systemd packaging/launchd packaging/local-mcp-gateways "$krw_common/packaging/"
install -m 0644 packaging/README.md packaging/release-trust-registry.example.json \
  "$krw_common/packaging/"
install -m 0755 scripts/verify_standalone_release.py "$krw_common/packaging/"
install -m 0644 scripts/release_provider.py scripts/write_dual_provider_evidence_index.py \
  "$krw_common/packaging/"

krw_commit=$(git rev-parse HEAD)
krw_tree=$(git rev-parse HEAD^{tree})
for krw_provider in glm deepseek; do
  krw_candidate="$krw_staging/$krw_provider"
  cp -R "$krw_common/." "$krw_candidate"
  install -d -m 0755 "$krw_candidate/deployments"
  install -m 0644 deployments/prod/budget-registry.yaml \
    "$krw_candidate/deployments/budget-registry.yaml"
  install -m 0644 "deployments/prod/model-registry.$krw_provider.yaml" \
    "$krw_candidate/deployments/model-registry.yaml"
  install -m 0644 deployments/prod/deployment-binding.krw-ontology.example.yaml \
    "$krw_candidate/deployments/deployment-binding.example.yaml"
  install -m 0644 deployments/prod/endpoint-registry.example.yaml \
    "$krw_candidate/deployments/endpoint-registry.example.yaml"
  python3 scripts/write_release_manifest.py \
    --root "$krw_candidate" --git-commit "$krw_commit" --git-tree "$krw_tree" \
    --provider "$krw_provider"
  python3 scripts/verify_standalone_release.py --root "$krw_candidate" >/dev/null
done

rm -r -- "$krw_common"

python3 - "$krw_staging" "$krw_commit" "$krw_tree" <<'PY'
import json
import pathlib
import sys

root = pathlib.Path(sys.argv[1])
index = {
    "schema_version": 1,
    "git_commit": sys.argv[2],
    "git_tree": sys.argv[3],
    "bundles": {},
}
for provider in ("glm", "deepseek"):
    manifest = json.loads((root / provider / "release-manifest.json").read_text())
    index["bundles"][provider] = {
        "physical_model": manifest["physical_models"][0],
        "manifest_hash": manifest["manifest_hash"],
        "provider_id": manifest["provider_id"],
    }
(root / "dual-release-index.json").write_text(
    json.dumps(index, ensure_ascii=False, sort_keys=True, indent=2) + "\n",
    encoding="utf-8",
)
PY

python3 scripts/test_dual_provider_release.py \
  --staging-root "$krw_staging" --expect-sealed false >/dev/null

# Another invocation can create the output directory while this build is
# compiling. `mv staging existing-directory` would silently nest the complete
# release below that directory, so re-check immediately before the atomic
# publish step and leave the existing candidate untouched.
if [[ -e "$krw_output_root" ]]; then
  printf 'release output appeared during build: %s\n' "$krw_output_root" >&2
  exit 1
fi
mv -- "$krw_staging" "$krw_output_root"
krw_cleanup=false
printf 'dual provider release candidates built: %s\n' "$krw_output_root"
