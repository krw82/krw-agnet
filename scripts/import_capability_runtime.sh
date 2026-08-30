#!/usr/bin/env bash
# Reproducibly seed the canonical online runtime from an immutable upstream
# Git object. This is an initial import utility, never a runtime dependency.
set -euo pipefail

readonly SOURCE_REPOSITORY="${1:-~/krw-ontology-v2/krw-ontology}"
readonly SOURCE_COMMIT="0cdc12200f6478bf71e373c6ab820137d1dff676"
readonly TARGET_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../services/krw-ontology-runtime" && pwd)"
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

if [[ -e "$TARGET_ROOT/src/krw_capability_runtime" ]]; then
  echo "canonical runtime source already exists; refusing to overwrite it" >&2
  exit 3
fi

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

git -C "$SOURCE_REPOSITORY" archive --format=tar "$SOURCE_COMMIT" "${SOURCE_PATHS[@]}" \
  | tar -x -C "$STAGING_ROOT"

mkdir -p "$TARGET_ROOT/src" "$TARGET_ROOT/resources"
mv "$STAGING_ROOT/src/krw_ontology" "$TARGET_ROOT/src/krw_capability_runtime"
mv "$STAGING_ROOT/ontology" "$TARGET_ROOT/resources/ontology"

# This is a mechanical package-identity rewrite. Domain semantics are edited
# afterwards only through reviewed, canonical runtime code.
find "$TARGET_ROOT/src/krw_capability_runtime" -type f -name '*.py' \
  -exec perl -pi -e 's/\bkrw_ontology\b/krw_capability_runtime/g' {} +

echo "Imported canonical capability runtime from $SOURCE_COMMIT"
