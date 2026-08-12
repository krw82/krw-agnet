#!/usr/bin/env bash
set -euo pipefail

# Finalize the two provider candidates only after both have passed their own
# authorization and offline bundle checks. This is metadata assembly; it does
# not start a daemon and does not contact either provider.

krw_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$krw_root"
krw_release_root=
while [[ $# -gt 0 ]]; do
  case "$1" in
    --release-root) [[ $# -ge 2 ]] || { echo '--release-root requires a directory' >&2; exit 2; }; krw_release_root=$2; shift 2 ;;
    --help|-h) printf 'usage: %s --release-root /absolute/dual-release-directory\n' "$0"; exit 0 ;;
    *) echo "unexpected argument: $1" >&2; exit 2 ;;
  esac
done
[[ "$krw_release_root" = /* && -d "$krw_release_root" && ! -L "$krw_release_root" ]] || { echo 'release root must be an absolute real directory' >&2; exit 2; }

python3 "$krw_root/scripts/test_dual_provider_release.py" \
  --staging-root "$krw_release_root" --expect-sealed true >/dev/null

python3 - "$krw_release_root" "$krw_root" <<'PY'
import json
import os
import pathlib
import tempfile
import sys

root = pathlib.Path(sys.argv[1])
source_root = pathlib.Path(sys.argv[2])
providers = ("glm", "deepseek")
bundles = {}
sys.path.insert(0, str(source_root / "scripts"))
from release_provider import validate_public_descriptor

for provider in providers:
    bundle = root / provider
    manifest = json.loads((bundle / "release-manifest.json").read_text(encoding="utf-8"))
    descriptor_meta = validate_public_descriptor(
        (bundle / "public-release.json").resolve(),
        provider,
    )
    runtime = (bundle / "frontend-runtime.env").read_text(encoding="utf-8")
    if (
        f"KRW_AGENT_PROVIDER={provider}\n" not in runtime
        or f"KRW_AGENT_RELEASE_ARTIFACT_HASH={descriptor_meta['descriptor_artifact_hash']}\n" not in runtime
        or f"KRW_AGENT_RELEASE_SET_HASH={descriptor_meta['release_set_hash']}\n" not in runtime
    ):
        raise SystemExit(f"{provider} frontend runtime pins do not match its descriptor")
    bundles[provider] = {
        "provider_id": manifest["provider_id"],
        "physical_model": manifest["physical_models"][0],
        "manifest_hash": manifest["manifest_hash"],
        "descriptor_artifact_hash": descriptor_meta["descriptor_artifact_hash"],
        "release_set_hash": descriptor_meta["release_set_hash"],
    }
commits = {
    json.loads((root / provider / "release-manifest.json").read_text(encoding="utf-8"))["git_commit"]
    for provider in providers
}
trees = {
    json.loads((root / provider / "release-manifest.json").read_text(encoding="utf-8"))["git_tree"]
    for provider in providers
}
if len(commits) != 1 or len(trees) != 1:
    raise SystemExit("provider bundles do not share one source commit/tree")
index = {
    "schema_version": 2,
    "git_commit": commits.pop(),
    "git_tree": trees.pop(),
    "bundles": bundles,
}
target = root / "dual-release-index.json"
fd, temporary_name = tempfile.mkstemp(prefix=".dual-release-index.", dir=root)
temporary = pathlib.Path(temporary_name)
try:
    with os.fdopen(fd, "w", encoding="utf-8") as stream:
        json.dump(index, stream, ensure_ascii=False, sort_keys=True, indent=2)
        stream.write("\n")
        stream.flush()
        os.fsync(stream.fileno())
    os.replace(temporary, target)
finally:
    temporary.unlink(missing_ok=True)
PY
printf 'dual provider release finalized: %s\n' "$krw_release_root"
