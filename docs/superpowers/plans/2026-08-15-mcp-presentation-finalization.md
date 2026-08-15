# MCP Presentation Pipeline Finalization Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the final research answer path optionally produce trustworthy charts from MCP structured data, while guaranteeing that chart generation can never block, retry, defer, bill, or invalidate the text answer.

**Architecture:** The ontology runtime keeps the complete research evidence path and emits only a bounded typed `PresentationSeriesPackV2` in the private MCP `_meta` channel. The Rust run engine is the only authority that decides whether a chart is appropriate and compiles the pack into deterministic schema-v4 artifacts. Supabase and the frontend treat those artifacts as an optional projection; missing or invalid visualization data is equivalent to an empty list. The sealed release contains the chart sidecar and its provenance, so the runtime does not depend on an unsealed or ad-hoc database file.

**Tech Stack:** Rust (`capability-runtime`, `run-engine`, `krw-presentation`), Python/Pydantic + SQLite sidecar, MCP `CallToolResult._meta`, PostgreSQL/Supabase projection, TypeScript/React/Nivo, Vitest/pytest, GLM and DeepSeek release fixtures.

## Global Constraints

- Do not modify `~/krw-ontology` or its ontology schema.
- Do not add an LLM turn, MCP call, tool call, skill decision, or provider-specific branch for chart selection.
- MCP returns JSON data only under `com.krwontology/presentationSeries`; no HTML, JSX, SVG, JavaScript, CSS, renderer instructions, or arbitrary URLs are accepted from MCP.
- Presentation metadata never enters provider-visible content, model history, EvidenceLedger claims, or prompt compaction.
- A presentation defect becomes `visualizations: []`; it must not produce a research failure, retry, defer, charge, rollback, or missing Markdown answer.
- There is one production path. Do not retain a legacy chart compiler, feature flag, compatibility reader, or provider-specific chart format.
- GLM and DeepSeek use the same pack schema, deterministic compiler, release manifest, and frontend artifact contract.
- Private pack bounds: 64 KiB canonical JSON, at most 8 series, at most 12 points per series. Committed artifacts: at most 3.
- Every committed chart point must resolve to evidence/object references from the current run and the current sealed data release.
- A chart is an optional presentation of research; the text answer is the authoritative completion.

## Current baseline

The following work is already present in the working tree and should be preserved rather than reimplemented:

- `services/krw-ontology-runtime/.../chart_series.py` builds a v2 typed sidecar pack with metric, ticker, scope, period, currency, and object provenance.
- `.../mcp_server/tools.py` places the pack in private MCP `_meta` and removes it from the model-visible ResearchState.
- `crates/capability-runtime/src/lib.rs` captures and bounds the private pack without changing the primary capability result.
- `crates/krw-presentation/src/lib.rs` deterministically compiles schema-v4 trend, comparison, and composition artifacts and omits incomparable data.
- `crates/run-engine/src/lib.rs` filters artifacts against current-run evidence and release provenance before committing `AnswerBundle.visualizations`.
- The frontend has an optional visualization projection and a validated Nivo/table renderer; chat remains successful when the optional read or write is unavailable.
- `scripts/verify_chart_series_release.py` validates the sidecar release contract, and `scripts/materialize_chart_series_release.py` creates a new data release without modifying the ontology source tree.

The remaining work is therefore release materialization, preflight wiring, and provider/production verification—not a new chart architecture.

---

### Task 1: Freeze the MCP/private presentation contract

**Files:**
- Inspect/modify only if needed: `services/krw-ontology-runtime/src/krw_capability_runtime/mcp_server/tools.py`
- Inspect/modify only if needed: `crates/capability-runtime/src/lib.rs`
- Inspect/modify only if needed: `crates/run-engine/src/lib.rs`
- Test: `services/krw-ontology-runtime/tests/test_presentation_pack.py`
- Test: `crates/capability-runtime/src/lib.rs` test module
- Test: `crates/run-engine/src/lib.rs` presentation tests

**Interfaces:**
- Producer: `query_context` may attach `{"com.krwontology/presentationSeries": PresentationSeriesPackV2}` to MCP `_meta`.
- Consumer: `extract_presentation_pack(meta) -> Option<Value>` and `krw_presentation::compile(&pack) -> Result<Vec<Value>, _>`.
- Public result: `CapabilityResult.provider_content` and the model transcript contain no presentation pack.

- [ ] **Step 1: Verify the positive isolation fixture**

  Use the existing fixture to assert all of the following in one test:

  ```python
  assert "presentation_series_pack" not in model_visible_payload
  assert meta["com.krwontology/presentationSeries"]["schema_version"] == 2
  assert "<html" not in json.dumps(meta).lower()
  assert "<svg" not in json.dumps(meta).lower()
  ```

- [ ] **Step 2: Verify the failure-inert fixtures**

  Exercise malformed schema version, more than 8 series, more than 12 points, oversized canonical JSON, and markup-shaped string values. Each case must return the normal primary capability result and `presentation: None`/empty visualizations.

- [ ] **Step 3: Verify current-run provenance**

  Feed the Rust engine one pack with a matching release/object reference and one with a stale reference. The first may render; the second must render the same Markdown with `visualizations: []` and no workflow transition to failure.

- [ ] **Step 4: Run the focused gates**

  ```bash
  cd ~/krw-agnet/services/krw-ontology-runtime
  uv run pytest tests/test_presentation_pack.py -q

  cd ~/krw-agnet
  cargo test -p krw-agent-capability-runtime -p krw-presentation -p krw-agent-run-engine --lib presentation
  ```

  Expected: the primary answer path remains successful in every invalid-presentation case.

---

### Task 2: Materialize and seal the v2 chart-series release

**Files:**
- Use: `scripts/materialize_chart_series_release.py`
- Use: `scripts/verify_chart_series_release.py`
- Modify only if a preflight failure identifies a concrete contract mismatch: `services/krw-ontology-runtime/src/krw_capability_runtime/agent_index/chart_series.py`
- Test: `services/krw-ontology-runtime/tests/test_presentation_pack.py`

**Interfaces:**
- Input: existing `$HOME/krw-ontology-data/releases/prod/current` (read-only).
- Output: a new sibling release under `$HOME/krw-ontology-data/releases/prod/<new-release-id>` containing `indexes/chart_series.sqlite` with schema `krw-ontology-chart-series/v2` and builder `chart-series-builder/v3`.
- Promotion: only the explicit `--promote-current` option may change the `prod/current` symlink.

- [ ] **Step 1: Recheck the current release without changing it**

  ```bash
  cd ~/krw-agnet/services/krw-ontology-runtime
  uv run python ../../scripts/verify_chart_series_release.py \
    --release-root "$HOME/krw-ontology-data/releases/prod/current"
  ```

  Expected current result: a clear failure stating that the existing v1 chart sidecar is not a required v2 release. This is a data-release blocker, not a research-question failure.

- [ ] **Step 2: Create a candidate release without promotion**

  ```bash
  cd ~/krw-agnet/services/krw-ontology-runtime
  uv run python ../../scripts/materialize_chart_series_release.py \
    --source-release-root "$HOME/krw-ontology-data/releases/prod/current" \
    --output-root "$HOME/krw-ontology-data/releases/prod/$(date +%Y%m%d_%H%M%S)_presentation_v2"
  ```

  The script must use the existing release clone/reflink policy, rebuild only the chart sidecar, rebind release metadata, preserve company shard seals, and never write to `~/krw-ontology`.

- [ ] **Step 3: Verify the candidate deeply**

  ```bash
  uv run python ../../scripts/verify_chart_series_release.py \
    --release-root "$HOME/krw-ontology-data/releases/prod/<candidate-id>"
  ```

  The report must show matching release ID, chart-sidecar hash, shard-manifest hash, valid SQLite metadata, finite points, canonical period ordering, and no orphan points/series.

- [ ] **Step 4: Inspect before promotion**

  Confirm that the candidate contains the same company shard count as the source, a v2 chart sidecar, and no modified ontology source. Record the candidate ID and sidecar hash in the release evidence file.

- [ ] **Step 5: Promote only the verified candidate**

  ```bash
  uv run python ../../scripts/materialize_chart_series_release.py \
    --source-release-root "$HOME/krw-ontology-data/releases/prod/current" \
    --output-root "$HOME/krw-ontology-data/releases/prod/<candidate-id>" \
    --promote-current
  ```

  Promotion is a data-release operation only. It must not rebuild Rust, start a daemon, or silently change the ontology source.

---

### Task 3: Make release preflight stop before expensive builds

**Files:**
- Modify: `scripts/build_dual_provider_release.sh`
- Modify: `scripts/build_sealed_dual_provider_release.sh`
- Modify: `scripts/check_product_projection_compat.py`
- Test: `scripts/test_dual_provider_release.py`
- Test: `scripts/verify_chart_series_release.py` fixture path

**Interfaces:**
- `build_dual_provider_release.sh` must verify the selected data release before Rust compilation or Python packaging.
- The preflight must fail with an actionable data-release message, not a generic build error.
- The release manifest must include chart sidecar path, sidecar schema/builder versions, sidecar hash, release ID, and source shard-manifest hash.

- [ ] **Step 1: Add/keep the early gate**

  The build script must execute the verifier from the runtime project using `uv`, because direct system Python may not have the runtime dependencies:

  ```bash
  (cd "$KRW_RELEASE_ROOT/services/krw-ontology-runtime" && \
    uv run python "$KRW_RELEASE_ROOT/scripts/verify_chart_series_release.py" \
      --release-root "$KRW_CAPABILITY_RELEASE_ROOT")
  ```

- [ ] **Step 2: Validate a small fixture**

  Build a temporary v2 sidecar fixture from the existing test helper and verify it through the same `uv` command. The fixture must pass; the real v1 release must fail before any Rust compilation.

- [ ] **Step 3: Check projection ABI compatibility**

  ```bash
  python3 scripts/check_product_projection_compat.py \
    --front-root ~/krw-ontology-front
  ```

  Expected: the agent-owned nine-key `read_final_projection` ABI remains authoritative and visualization fields remain optional.

- [ ] **Step 4: Run release-script contract tests**

  ```bash
  bash -n scripts/build_dual_provider_release.sh
  bash -n scripts/build_sealed_dual_provider_release.sh
  PYTHONPATH=scripts python3 scripts/test_dual_provider_release.py
  ```

---

### Task 4: Verify the frontend optional projection and safe rendering

**Files:**
- Inspect/modify only if a test identifies a real mismatch: `~/krw-ontology-front/src/lib/visualizations/contracts.ts`
- Inspect/modify only if a test identifies a real mismatch: `~/krw-ontology-front/src/lib/visualizations/db.ts`
- Inspect/modify only if a test identifies a real mismatch: `~/krw-ontology-front/src/components/KrwVisualizationCard.tsx`
- Inspect/modify only if a test identifies a real mismatch: `~/krw-ontology-front/src/app/api/chat/[sessionId]/route.ts`
- Test: frontend visualization and chat route tests

**Interfaces:**
- Rust public artifact: `artifact_format: "krw-visualization"`, `schema_version: 4`.
- Frontend read: malformed/unknown artifacts are filtered to `[]`.
- Chat response: visualization DB/table errors return normal Markdown/history with HTTP 200.

- [ ] **Step 1: Run frontend type and unit checks**

  ```bash
  cd ~/krw-ontology-front
  npm run typecheck
  npx vitest run
  ```

- [ ] **Step 2: Confirm renderer safety**

  Review the accepted artifact fields and assert that rendering uses structured Nivo/table props only. No `dangerouslySetInnerHTML`, raw HTML, executable script, or MCP-provided renderer URL may be introduced.

- [ ] **Step 3: Confirm optional DB behavior**

  Use the existing route tests to verify that missing table, malformed row, and empty projection all preserve the assistant Markdown and return an empty visualization array.

---

### Task 5: Build and install one sealed dual-provider release

**Files:**
- Use: `scripts/build_dual_provider_release.sh`
- Use: `scripts/build_sealed_dual_provider_release.sh`
- Use: `packaging/launchd/install-local-mac-capabilityd-release.sh`
- Use: `~/krw-ontology-front/scripts/deploy-production-fast.sh`
- Documentation: `docs/DUAL_PROVIDER_PRODUCTION_RUNBOOK.md`

**Interfaces:**
- One sealed release contains the Rust daemon, GLM/DeepSeek provider configuration, capability runtime, and verified chart sidecar binding.
- Launchd installs/starts the release; the frontend deployment does not use the removed legacy worker path.
- No build occurs while either repository has an uncommitted tree when the release script requires a clean source snapshot.

- [ ] **Step 1: Check both source trees**

  ```bash
  git status --short
  git -C ~/krw-ontology-front status --short
  ```

  Resolve or commit intended changes before invoking the production release builder. Do not use `git reset --hard` or delete unrelated user changes.

- [ ] **Step 2: Point the release at the verified data candidate**

  Set the release root variables expected by the existing scripts to the candidate promoted in Task 2. Do not point the builder at the old v1 `prod/current` release.

- [ ] **Step 3: Run the full deployment command**

  ```bash
  cd ~/krw-ontology-front
  npm run prod:deploy:full
  ```

  The command is allowed to proceed only after the sidecar gate, projection ABI gate, source cleanliness gate, provider release gate, and packaging checks pass.

- [ ] **Step 4: Verify the installed release**

  Confirm launchd reports the same daemon build, release ID, chart sidecar hash, and provider contract for both GLM and DeepSeek. A healthy daemon alone is insufficient if its sidecar hash differs from the release manifest.

---

### Task 6: Run provider-neutral quality canaries and final acceptance

**Files:**
- Use: `scripts/run_live_quality_matrix.py`
- Use: `scripts/run_session_followup_quality.py`
- Use: `scripts/run_dual_provider_acceptance.py`
- Evidence: release/canary output under the existing evidence directory

**Interfaces:**
- Same normalized research result and same presentation pack must compile to byte-identical artifact JSON for GLM and DeepSeek.
- A chart omission is acceptable; a missing or incomplete text answer is not.
- The canary covers one chartable question, one non-chartable qualitative question, one follow-up in the same chat room, and one independent room.

- [ ] **Step 1: Run the deterministic dual-provider fixture gate**

  ```bash
  PYTHONPATH=scripts python3 scripts/test_dual_provider_release.py
  ```

- [ ] **Step 2: Run the live GLM matrix**

  Use the existing GLM test configuration and record original question, Markdown, run status, capability trace, evidence count, and visualization artifacts.

- [ ] **Step 3: Run the live DeepSeek matrix**

  Repeat exactly the same question set with DeepSeek. Do not alter the plan, skill, chart rules, or output schema by provider.

- [ ] **Step 4: Check session continuity**

  In one chat room, ask a chartable first question followed by a short follow-up such as “그럼 최근 분기에는?”; in a separate room ask an unrelated company question. Confirm no cross-room evidence or chart reference is reused.

- [ ] **Step 5: Accept or stop**

  Accept only when Markdown is complete, current-run evidence references are valid, no HTML appears in MCP or artifact payloads, chart failures remain optional, and both providers satisfy the same contract. If a canary fails, keep the release unpromoted and record the exact gate; do not weaken the runtime contract to make it pass.

## Completion criteria

1. The production data release verifier passes for the selected release and chart sidecar.
2. `npm run prod:deploy:full` reaches packaging with the new sidecar and no legacy chart flag/path.
3. Rust, Python, and frontend focused/full regression suites pass.
4. GLM and DeepSeek use one identical chart contract and produce deterministic artifacts for identical normalized evidence.
5. Non-chart questions return normal answers with zero visualizations and no extra model turn.
6. Missing, malformed, stale, mixed-period, mixed-currency, or incomparable chart inputs never block the text answer.
7. No file under `~/krw-ontology` or the ontology schema is modified.

## Handoff

This plan is intentionally split into code-contract verification, data-release materialization, deployment, and live canary gates. The first four tasks are safe local work; Tasks 5–6 require the operator’s release environment and provider credentials. The plan must not be considered complete until the sealed release and both-provider canary have passed.
