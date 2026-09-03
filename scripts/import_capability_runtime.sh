#!/usr/bin/env bash
# Reproducibly seed and maintain the canonical online runtime from an
# immutable upstream Git object. This is an import/lock utility, never a
# runtime dependency.
#
# Modes:
#   import_capability_runtime.sh [REPO]            initial wholesale import
#                                                  (refuses an existing tree)
#   import_capability_runtime.sh --update [REPO]   overlay-refresh every
#                                                  SOURCE_PATHS file from the
#                                                  pinned commit; never touches
#                                                  runtime-native files
#   import_capability_runtime.sh --record [REPO]   (re)write UPSTREAM_LOCK.json
#                                                  with per-file sha256 of the
#                                                  CURRENT tree plus the pin
#                                                  reference; changes no source
#   import_capability_runtime.sh --verify [REPO]   recompute hashes and diff
#                                                  against the lock; exit
#                                                  nonzero on drift
#
# Mixed-provenance rule: files listed in SOURCE_PATHS are upstream-owned
# (their content is the pinned commit modulo the mechanical package rename
# plus reviewed canonical edits recorded as divergence in the lock); every
# other file under src/krw_capability_runtime (market/, transport/,
# observation/tools.py, __main__.py, ...) is runtime-native and is never
# written by this script.
set -euo pipefail

MODE="initial"
if [[ "${1:-}" == --* ]]; then
  MODE="${1#--}"
  shift
fi
case "$MODE" in
  initial|update|record|verify) ;;
  *)
    echo "unknown mode: --$MODE (expected --update, --record, or --verify)" >&2
    exit 2
    ;;
esac

readonly SOURCE_REPOSITORY="${1:-${HOME}/krw-ontology-v2/krw-ontology}"
readonly SOURCE_COMMIT="0cdc12200f6478bf71e373c6ab820137d1dff676"
readonly SCRIPT_DIRECTORY="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
readonly TARGET_ROOT="$(cd -- "$SCRIPT_DIRECTORY/../services/krw-ontology-runtime" && pwd)"
readonly PACKAGE_ROOT="$TARGET_ROOT/src/krw_capability_runtime"
readonly LOCK_PATH="$TARGET_ROOT/UPSTREAM_LOCK.json"
readonly STAGING_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/krw-capability-import.XXXXXX")"

cleanup() {
  rm -rf -- "$STAGING_ROOT"
}
trap cleanup EXIT

if [[ ! -d "$SOURCE_REPOSITORY/.git" ]]; then
  echo "source repository is not a Git checkout: $SOURCE_REPOSITORY" >&2
  exit 2
fi
git -C "$SOURCE_REPOSITORY" cat-file -e "${SOURCE_COMMIT}^{commit}"

# The curated upstream-owned inventory. Keep in lockstep with
# UPSTREAM_LOCK.json module_paths (record/verify enforce this).
readonly SOURCE_PATHS=(
  src/krw_ontology/__init__.py
  src/krw_ontology/agent_index/__init__.py
  src/krw_ontology/agent_index/builder.py
  src/krw_ontology/agent_index/cache_seal.py
  src/krw_ontology/agent_index/chart_series.py
  src/krw_ontology/agent_index/cross_company_links.py
  src/krw_ontology/agent_index/discovery.py
  src/krw_ontology/agent_index/metric_dictionary.py
  src/krw_ontology/agent_index/research_budget.py
  src/krw_ontology/agent_index/research_contexts.py
  src/krw_ontology/agent_index/research_kernel.py
  src/krw_ontology/agent_index/research_router.py
  src/krw_ontology/agent_index/research_types.py
  src/krw_ontology/agent_index/retrieval_text.py
  src/krw_ontology/agent_index/retriever.py
  src/krw_ontology/agent_index/router.py
  src/krw_ontology/agent_index/router_cache.py
  src/krw_ontology/agent_index/router_coherence.py
  src/krw_ontology/agent_index/router_sidecar.py
  src/krw_ontology/agent_index/semantic_identity.py
  src/krw_ontology/agent_index/source_artifact_sqlite.py
  src/krw_ontology/agent_index/spine_builder.py
  src/krw_ontology/agent_index/spine_preflight.py
  src/krw_ontology/agent_index/spine_router.py
  src/krw_ontology/agent_index/spine_schema.py
  src/krw_ontology/agent_index/spine_verify.py
  src/krw_ontology/agent_index/store.py
  src/krw_ontology/config/__init__.py
  src/krw_ontology/config/constants.py
  src/krw_ontology/config/paths.py
  src/krw_ontology/guru/__init__.py
  src/krw_ontology/guru/company_bridge.py
  src/krw_ontology/guru/company_context.py
  src/krw_ontology/guru/context_taxonomy.json
  src/krw_ontology/guru/context_taxonomy.py
  src/krw_ontology/guru/index.py
  src/krw_ontology/guru/lens_selector.py
  src/krw_ontology/guru/mcp_tools.py
  src/krw_ontology/guru/models.py
  src/krw_ontology/guru/planner.py
  src/krw_ontology/guru/renderer.py
  src/krw_ontology/guru/sources.py
  src/krw_ontology/guru/workspace.py
  src/krw_ontology/mcp_server/__init__.py
  src/krw_ontology/mcp_server/contracts.py
  src/krw_ontology/mcp_server/evidence_pack.py
  src/krw_ontology/mcp_server/runtime.py
  src/krw_ontology/mcp_server/tools.py
  # Observation data layer: ports + store + seed only. The provider adapters
  # (providers/*) are build-time collection plumbing — the serving runtime
  # never touches a network — and builder.py materializes the store at build
  # time, so neither belongs in the capability runtime.
  src/krw_ontology/observation/__init__.py
  src/krw_ontology/observation/ports.py
  src/krw_ontology/observation/seed.py
  src/krw_ontology/observation/store.py
  src/krw_ontology/release.py
  src/krw_ontology/schema/__init__.py
  src/krw_ontology/schema/objects.py
  src/krw_ontology/utils/__init__.py
  src/krw_ontology/utils/io.py
  src/krw_ontology/validators/__init__.py
  src/krw_ontology/validators/metric_validator.py
  # Observation seed resource consumed by observation/seed.py at runtime.
  ontology/observation/series_seed.yaml
  ontology/registry.yaml
  ontology/schema/claim_types.yaml
  ontology/schema/language_signals.yaml
  ontology/schema/metric_dictionary.yaml
  ontology/schema/quote_types.yaml
  ontology/schema/relations.yaml
  ontology/schema/risk_categories.yaml
  ontology/sector_packs/energy_lng.yaml
  ontology/sector_packs/generic.yaml
  ontology/taxonomy/factors.yaml
  ontology/taxonomy/sector_expectations.yaml
)

stage_upstream() {
  # Materialize the pinned upstream tree read-only (git archive never reads
  # or writes the source working tree) and apply the mechanical package
  # rename to the staged *.py files so staged bytes are directly comparable
  # with (and installable as) runtime files.
  local missing=0
  local path
  for path in "${SOURCE_PATHS[@]}"; do
    if ! git -C "$SOURCE_REPOSITORY" cat-file -e "${SOURCE_COMMIT}:${path}" 2>/dev/null; then
      echo "pinned commit ${SOURCE_COMMIT} is missing curated path: ${path}" >&2
      missing=1
    fi
  done
  if [[ "$missing" -ne 0 ]]; then
    exit 2
  fi
  mkdir -p "$STAGING_ROOT/upstream"
  git -C "$SOURCE_REPOSITORY" archive --format=tar "$SOURCE_COMMIT" "${SOURCE_PATHS[@]}" \
    | tar -x -C "$STAGING_ROOT/upstream"
  find "$STAGING_ROOT/upstream/src" -type f -name '*.py' \
    -exec perl -pi -e 's/\bkrw_ontology\b/krw_capability_runtime/g' {} +
}

write_paths_list() {
  printf '%s\n' "${SOURCE_PATHS[@]}" > "$STAGING_ROOT/paths.txt"
}

require_existing_runtime() {
  if [[ ! -d "$PACKAGE_ROOT" ]]; then
    echo "canonical runtime source does not exist yet; run the initial import first" >&2
    exit 3
  fi
}

refuse_dirty_tree() {
  # Overlay updates must land on a committed state so the lock can attribute
  # every content change to exactly one reviewed step.
  local repo_root dirty
  if ! repo_root=$(git -C "$TARGET_ROOT" rev-parse --show-toplevel 2>/dev/null); then
    echo "target tree is not inside a Git repository: $TARGET_ROOT" >&2
    exit 2
  fi
  if ! dirty=$(git -C "$repo_root" status --porcelain -- "$TARGET_ROOT" "$SCRIPT_DIRECTORY"); then
    echo "unable to inspect the Git status of $TARGET_ROOT" >&2
    exit 2
  fi
  if [[ -n "$dirty" ]]; then
    echo "refusing to update a dirty tree; commit or stash first:" >&2
    printf '%s\n' "$dirty" >&2
    exit 4
  fi
}

run_lock_tool() {
  # Python performs the JSON bookkeeping (per-file sha256 records, drift
  # diff); bash owns staging and file installation.
  KRW_LOCK_PATH="$LOCK_PATH" \
  KRW_TARGET_ROOT="$TARGET_ROOT" \
  KRW_UPSTREAM_STAGE="$STAGING_ROOT/upstream" \
  KRW_PATHS_LIST="$STAGING_ROOT/paths.txt" \
  KRW_SOURCE_REPOSITORY="$SOURCE_REPOSITORY" \
  KRW_SOURCE_COMMIT="$SOURCE_COMMIT" \
  KRW_LOCK_MODE="$1" \
    python3 - <<'PY'
import hashlib
import json
import os
import sys
from datetime import datetime, timezone
from pathlib import Path

lock_path = Path(os.environ["KRW_LOCK_PATH"])
target_root = Path(os.environ["KRW_TARGET_ROOT"])
stage = Path(os.environ["KRW_UPSTREAM_STAGE"])
paths = [line.strip() for line in Path(os.environ["KRW_PATHS_LIST"]).read_text().splitlines() if line.strip()]
source_repository = os.environ["KRW_SOURCE_REPOSITORY"]
source_commit = os.environ["KRW_SOURCE_COMMIT"]
mode = os.environ["KRW_LOCK_MODE"]


def mappings():
    for path in paths:
        if path.startswith("src/krw_ontology/"):
            staged = Path("src/krw_ontology") / path[len("src/krw_ontology/"):]
            runtime = Path("src/krw_capability_runtime") / path[len("src/krw_ontology/"):]
        elif path.startswith("ontology/"):
            staged = Path(path)
            runtime = Path("src/krw_capability_runtime/resources") / path
        else:
            raise SystemExit(f"curated path is outside the known trees: {path}")
        yield path, staged, runtime


def sha256_of(path: Path) -> str | None:
    if not path.is_file():
        return None
    return "sha256:" + hashlib.sha256(path.read_bytes()).hexdigest()


def load_lock() -> dict:
    if lock_path.is_file():
        with lock_path.open() as handle:
            return json.load(handle)
    return {}


def preserve_hand_maintained(lock: dict) -> dict:
    previous = load_lock()
    for key in ("excluded_paths", "tool_disposition", "forbidden_runtime_dependencies"):
        if key in previous:
            lock[key] = previous[key]
    return lock


records = []
diverged = []
for path, staged_rel, runtime_rel in mappings():
    upstream_sha = sha256_of(stage / staged_rel)
    runtime_sha = sha256_of(target_root / runtime_rel)
    record = {
        "path": path,
        "runtime_path": runtime_rel.as_posix(),
        "upstream_sha256": upstream_sha,
        "runtime_sha256": runtime_sha,
    }
    if upstream_sha is None or runtime_sha is None:
        record["diverged"] = True
        diverged.append(path)
    else:
        record["diverged"] = upstream_sha != runtime_sha
        if record["diverged"]:
            diverged.append(path)
    records.append(record)

if mode == "record":
    lock = preserve_hand_maintained(
        {
            "format_version": 2,
            "generated_at": datetime.now(timezone.utc)
            .isoformat(timespec="seconds")
            .replace("+00:00", "Z"),
            "source": {
                "repository": source_repository,
                "commit": source_commit,
                "import_mode": "git_object_only",
                "working_tree_is_never_an_input": True,
            },
            "module_paths": paths,
            # Per-file provenance: upstream_sha256 is the pinned commit
            # (modulo the mechanical package rename); runtime_sha256 is the
            # bytes actually in this tree when the lock was recorded.
            # diverged=true files carry reviewed canonical runtime edits or
            # predate the pin; the lock records reality, never aspiration.
            "files": records,
        }
    )
    with lock_path.open("w") as handle:
        json.dump(lock, handle, indent=2, ensure_ascii=False)
        handle.write("\n")
    matching = len(records) - len(diverged)
    print(f"lock recorded: {len(records)} upstream-owned files, {matching} at pin, {len(diverged)} diverged")
    for path in diverged:
        print(f"  diverged: {path}")
    sys.exit(0)

if mode == "verify":
    if not lock_path.is_file():
        print("verify: UPSTREAM_LOCK.json is missing; run --record first")
        sys.exit(1)
    lock = load_lock()
    failures = []
    if lock.get("source", {}).get("commit") != source_commit:
        failures.append(f"lock pins commit {lock.get('source', {}).get('commit')!r}, script pins {source_commit!r}")
    if lock.get("module_paths") != paths:
        failures.append("lock module_paths differ from the script SOURCE_PATHS; run --record")
    locked = {record["path"]: record for record in lock.get("files", [])}
    drift = []
    for path, _staged_rel, runtime_rel in mappings():
        record = locked.get(path)
        if record is None:
            failures.append(f"lock is missing a record for {path}; run --record")
            continue
        if sha256_of(target_root / runtime_rel) != record.get("runtime_sha256"):
            drift.append(path)
    for record in lock.get("files", []):
        if record["path"] not in set(paths):
            failures.append(f"lock records a path outside SOURCE_PATHS: {record['path']}")
    recorded_diverged = sorted(
        record["path"] for record in lock.get("files", []) if record.get("diverged")
    )
    print(f"verify: {len(records)} upstream-owned files checked against the lock")
    print(
        f"verify: {len(recorded_diverged)} files are recorded as intentionally "
        "diverged from the pin (canonical runtime edits or pre-pin content)"
    )
    for path in recorded_diverged:
        print(f"  recorded diverged: {path}")
    if drift:
        print(f"verify: DRIFT in {len(drift)} file(s) — tree no longer matches the lock:")
        for path in drift:
            print(f"  drift: {path}")
    for failure in failures:
        print(f"verify: {failure}")
    if drift or failures:
        sys.exit(1)
    print("verify: clean (tree matches the recorded lock)")
    sys.exit(0)
PY
}

case "$MODE" in
  initial)
    if [[ -e "$PACKAGE_ROOT" ]]; then
      echo "canonical runtime source already exists; refusing to overwrite it" >&2
      exit 3
    fi
    stage_upstream
    mkdir -p "$TARGET_ROOT/src"
    mv "$STAGING_ROOT/upstream/src/krw_ontology" "$PACKAGE_ROOT"
    mkdir -p "$PACKAGE_ROOT/resources"
    mv "$STAGING_ROOT/upstream/ontology" "$PACKAGE_ROOT/resources/ontology"
    write_paths_list
    run_lock_tool record
    echo "Imported canonical capability runtime from $SOURCE_COMMIT"
    ;;
  update)
    require_existing_runtime
    refuse_dirty_tree
    stage_upstream
    write_paths_list
    refreshed=0
    for path in "${SOURCE_PATHS[@]}"; do
      staged="$STAGING_ROOT/upstream/${path}"
      if [[ "$path" == src/krw_ontology/* ]]; then
        runtime="$PACKAGE_ROOT/${path#src/krw_ontology/}"
      else
        runtime="$PACKAGE_ROOT/resources/${path}"
      fi
      mkdir -p "$(dirname -- "$runtime")"
      install -m 0644 "$staged" "$runtime"
      refreshed=$((refreshed + 1))
    done
    run_lock_tool record
    echo "Overlay-refreshed $refreshed upstream-owned files from $SOURCE_COMMIT"
    echo "Runtime-native files (market/, transport/, observation/tools.py, ...) were not touched."
    echo "Re-apply reviewed canonical runtime edits on top, then commit and run --record."
    ;;
  record)
    require_existing_runtime
    stage_upstream
    write_paths_list
    run_lock_tool record
    ;;
  verify)
    require_existing_runtime
    stage_upstream
    write_paths_list
    run_lock_tool verify
    ;;
esac
