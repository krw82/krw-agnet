#!/usr/bin/env bash
set -euo pipefail

krw_release_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
krw_release_output=${1:-}

if [[ -z "$krw_release_output" || "$krw_release_output" != /* ]]; then
  printf 'usage: %s /absolute/new/output-directory\n' "$0" >&2
  exit 2
fi
if [[ -e "$krw_release_output" ]]; then
  printf 'release output already exists: %s\n' "$krw_release_output" >&2
  exit 2
fi

export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
export SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH:-0}

cd "$krw_release_root"
if ! git rev-parse --verify HEAD >/dev/null 2>&1; then
  printf 'release packaging requires a committed Git baseline\n' >&2
  exit 1
fi
if [[ -n "$(git status --porcelain=v1 --untracked-files=normal)" ]]; then
  printf 'release packaging requires a clean tracked source tree\n' >&2
  exit 1
fi

krw_release_parent=$(dirname -- "$krw_release_output")
mkdir -p -- "$krw_release_parent"
krw_release_staging=$(mktemp -d "$krw_release_parent/.krw-agent-release.XXXXXX")
krw_release_committed=false

cleanup_krw_release_staging() {
  if [[ "$krw_release_committed" != true ]]; then
    case "$krw_release_staging" in
      "$krw_release_parent"/.krw-agent-release.*)
        rm -r -- "$krw_release_staging"
        ;;
    esac
  fi
}
trap cleanup_krw_release_staging EXIT

cargo build --locked --profile production -p krw-agent -p krw-agentd

install -d -m 0755 \
  "$krw_release_staging/bin" \
  "$krw_release_staging/images" \
  "$krw_release_staging/migrations" \
  "$krw_release_staging/deployments" \
  "$krw_release_staging/host-ts" \
  "$krw_release_staging/docs" \
  "$krw_release_staging/packaging"
install -m 0755 target/production/krw-agent "$krw_release_staging/bin/krw-agent"
install -m 0755 target/production/krw-agentd "$krw_release_staging/bin/krw-agentd"

while IFS= read -r krw_agent_package; do
  target/production/krw-agent image build \
    "agents/$krw_agent_package" \
    --out "$krw_release_staging/images/$krw_agent_package"
  target/production/krw-agent image verify \
    "$krw_release_staging/images/$krw_agent_package"
done < <(awk -F '\t' '!/^#/ && NF { print $1 }' agents/fixtures/entrypoints.tsv | LC_ALL=C sort -u)

install -m 0644 migrations/*.sql "$krw_release_staging/migrations/"
install -m 0644 deployments/local/*.yaml "$krw_release_staging/deployments/"
install -m 0644 README.md TODOS.md "$krw_release_staging/docs/"
install -m 0644 docs/IMPLEMENTATION_STATUS.md docs/POSTGRES_RUNTIME.md \
  docs/PERFORMANCE_RELEASE_GATES.md docs/RELEASE_AUTHORIZATION.md "$krw_release_staging/docs/"
install -m 0644 packages/host-ts/package.json packages/host-ts/package-lock.json \
  packages/host-ts/tsconfig.json packages/host-ts/README.md packages/host-ts/INTEGRATION.md \
  "$krw_release_staging/host-ts/"
cp -R packages/host-ts/src packages/host-ts/test "$krw_release_staging/host-ts/"
cp -R packaging/systemd packaging/launchd "$krw_release_staging/packaging/"
install -m 0644 packaging/README.md packaging/release-trust-registry.example.json \
  "$krw_release_staging/packaging/"
install -m 0755 scripts/verify_standalone_release.py "$krw_release_staging/packaging/"

krw_release_commit=$(git rev-parse HEAD)
krw_release_tree=$(git rev-parse HEAD^{tree})
python3 scripts/write_release_manifest.py \
  --root "$krw_release_staging" \
  --git-commit "$krw_release_commit" \
  --git-tree "$krw_release_tree" \
  --model glm-5.2
python3 scripts/verify_standalone_release.py --root "$krw_release_staging"

chmod -R a-w "$krw_release_staging/images" "$krw_release_staging/migrations"
mv -- "$krw_release_staging" "$krw_release_output"
krw_release_committed=true
printf 'standalone release bundle built: %s\n' "$krw_release_output"
