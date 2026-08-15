# Private MCP Presentation Pipeline Final Hardening Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Deliver the final chart/presentation path so that structured MCP data can produce a trustworthy optional chart, while the text research answer remains independent, provider-neutral, and complete even when presentation data is missing or invalid.

**Architecture:** The ontology sidecar emits only a bounded typed series pack in MCP `_meta`; it never emits HTML and never exposes the pack to the model. Rust is the only chart-kind/transform/artifact compiler and binds every rendered point to evidence observed in the current run. Supabase stores and reads the optional artifact in a separate projection path; any presentation failure is converted to “no visualization” and cannot fail, retry, defer, bill, or roll back the text answer.

**Tech Stack:** Rust (`run-engine`, `capability-runtime`, `krw-presentation`, `evidence`), Python/Pydantic and SQLite sidecar, PostgreSQL/Supabase projections, TypeScript/React/Vitest, GLM and DeepSeek provider fixtures.

## Global Constraints

- Do not modify `~/krw-ontology` or its ontology schema.
- Do not add an LLM turn, MCP call, tool call, skill decision, or provider-specific branch for charts.
- MCP returns JSON data only under `com.krwontology/presentationSeries`; no HTML, JSX, SVG, or executable markup crosses the MCP boundary.
- Presentation data must not enter `provider_content`, model history, EvidenceLedger claims, or prompt compaction.
- A presentation defect must result in an empty visualization list and a normal text answer.
- Keep one production path: remove feature flags and do not retain a legacy chart compiler or compatibility reader.
- GLM and DeepSeek must consume the same pack/artifact contracts and deterministic compiler.
- Keep private pack bounds at 64 KiB, 8 series, 12 points per series, and committed artifact bounds at 3 artifacts.
- Build/verification must happen in `krw-agnet` and `krw-ontology-front` only.

## Current Baseline

Already present in the worktree and retained by this plan:

- Clause-scoped Python sidecar lookup with per-ticker/metric coverage and release metadata.
- Private MCP `_meta` capture with bounded, failure-inert parsing.
- Rust schema-v4 deterministic compiler for trend, comparison, composition, and derived growth views.
- Run checkpoint/replay support for private packs.
- Agent-owned final projection and optional frontend visualization projection.
- Front fallback that keeps Markdown/history when the optional visualization projection is unavailable.

The remaining work is hardening provenance, cross-company selection, public projection safety, and release preflight. No live production deploy is part of the code change; deployment is the final operator step after the gates below pass.

---

### Task 1: Bind private packs to the current release and evidence ledger

**Files:**
- Modify: `crates/run-engine/src/lib.rs` (`ActiveRun::ingest`, `retain_presentation_pack`, `compile_visualizations`)
- Modify: `crates/krw-presentation/src/lib.rs` (only if artifact provenance filtering needs a shared helper)
- Test: `crates/run-engine/src/lib.rs` presentation tests

**Interfaces:**
- Add `presentation_pack_matches_result(pack: &Value, result: &CapabilityResult) -> bool`.
- Add `current_presentation_evidence_refs(ledger: &EvidenceLedger) -> BTreeSet<String>` containing active evidence IDs and each active record’s `source_object_ids`.
- Add `filter_grounded_visualizations(artifacts: Vec<Value>, allowed_refs: &BTreeSet<String>) -> Vec<Value>`.

- [x] **Step 1: Add a failing release-provenance test**

```rust
#[tokio::test]
async fn mismatched_presentation_release_is_omitted_but_text_commits() {
    let result = research_state_result_with_release("release-current");
    let mut pack = trend_presentation_pack();
    pack["release_id"] = serde_json::json!("release-old");
    let outcome = run_with_result_and_pack(result, pack).await.expect("text answer");
    assert!(outcome.answer_bundle.visualizations.is_empty());
    assert!(!outcome.answer_bundle.rendered_markdown.is_empty());
}
```

- [x] **Step 2: Add a failing stale-object test**

```rust
#[tokio::test]
async fn visualization_with_object_not_in_current_ledger_is_omitted() {
    let mut pack = trend_presentation_pack();
    pack["series"][0]["points"][0]["object_id"] = serde_json::json!("object-from-another-run");
    let outcome = run_with_current_evidence_and_pack(pack).await.expect("text answer");
    assert!(outcome.answer_bundle.visualizations.is_empty());
}
```

- [x] **Step 3: Implement release binding without adding a run failure**

At `ActiveRun::ingest`, accept a pack only when the primary `ResearchStateV2` payload contains the same non-empty `release_id`. If the result is a test/control mapping without a release field, retain the pack only when the pack also omits release metadata. A present-but-mismatched release always drops the pack. This is an optional-presentation decision, so it must not return `EngineError`.

- [x] **Step 4: Implement ledger-backed artifact filtering**

Append the capability evidence first, then compile artifacts with the active ledger reference set. A reference is valid when it equals an active `evidence_id` or appears in an active record’s `source_object_ids`. Reject the whole artifact if any primary or derived reference is absent; do not partially edit a chart because partial charts hide provenance defects.

```rust
let allowed_refs = current_presentation_evidence_refs(&state.ledger);
let artifacts = filter_grounded_visualizations(
    self.compile_visualizations(&state.presentation_packs),
    &allowed_refs,
);
```

- [x] **Step 5: Verify positive and negative paths**

Run:

```bash
RUSTC="$HOME/.rustup/toolchains/1.97.1-aarch64-apple-darwin/bin/rustc" \
RUSTDOC="$HOME/.rustup/toolchains/1.97.1-aarch64-apple-darwin/bin/rustdoc" \
  "$HOME/.rustup/toolchains/1.97.1-aarch64-apple-darwin/bin/cargo" \
  test -p krw-agent-run-engine --lib presentation
```

Expected: valid current-run object IDs render; stale IDs and mismatched release IDs produce `visualizations: []` with a committed Markdown answer.

---

### Task 2: Prevent cross-company and mixed-period sidecar starvation

**Files:**
- Modify: `services/krw-ontology-runtime/src/krw_capability_runtime/agent_index/chart_series.py`
- Test: `services/krw-ontology-runtime/tests/test_presentation_pack.py`

**Interfaces:**
- Change the internal deduplication key in `_select_chart_series_rows` to `(ticker, canonical_metric, scope_kind, scope_key)`.
- Keep `coverage_pairs` as the mandatory `(ticker, metric)` reservation set.

- [x] **Step 1: Add a cross-company shared-scope regression test**

Create AAPL and MSFT rows with the same `scope_key="Other"` and the same metric. Assert that both rows survive the bounded selection when the clause requests both tickers.

- [x] **Step 2: Add a mixed-period regression test**

Create FY and CY points for one series and assert the pack either selects one compatible period basis or omits that series; it must never return a mixed FY/CY point list.

- [x] **Step 3: Include ticker in the dedupe key**

Replace:

```python
(canonical_metric, scope_kind, scope_key)
```

with:

```python
(ticker, canonical_metric, scope_kind, scope_key)
```

and leave the per-pair lookup and Rust chart-kind authority unchanged.

- [x] **Step 4: Verify sidecar behavior**

Run:

```bash
cd services/krw-ontology-runtime
uv run pytest tests/test_presentation_pack.py -q
```

Expected: both companies remain available, requested metrics are covered, and no extra global query limit is introduced.

---

### Task 3: Close the private MCP contract without exposing presentation data

**Files:**
- Modify: `crates/capability-runtime/src/lib.rs` only if an additional contract assertion is needed
- Modify: `services/krw-ontology-runtime/src/krw_capability_runtime/mcp_server/tools.py` only for pack metadata/shape assertions
- Test: `crates/capability-runtime/src/lib.rs` and `services/krw-ontology-runtime/tests/test_presentation_pack.py`

**Interfaces:**
- Keep `extract_presentation_pack(...) -> Option<Value>` non-throwing.
- Keep the provider-visible payload exactly the ResearchState JSON without `presentation_series_pack`.

- [x] **Step 1: Add an isolation test**

Call the Python `query_context` path with an eligible financial clause. Assert that:

```python
assert "presentation_series_pack" not in model_payload
assert meta["com.krwontology/presentationSeries"]["schema_version"] == 2
assert "<html" not in json.dumps(meta).lower()
```

- [x] **Step 2: Add a Rust envelope test**

Assert that parsing removes `_meta` from the model payload, retains the primary `structuredContent`, and returns `presentation: None` for malformed/oversized/private-markup-shaped values.

- [x] **Step 3: Keep failures optional**

Any pack schema, metadata, size, markup, or lock failure must be converted to omission. Only the primary MCP envelope/content/structured-content contract remains fail-closed.

- [x] **Step 4: Verify both language boundaries**

Run the focused Python and Rust tests. Expected: the model transcript contains no presentation pack and all malformed presentation cases still complete the text path.

---

### Task 4: Make the public projection and renderer fail-open and safe

**Files:**
- Modify: `migrations/0021_final_projection_contract.sql` only if the authoritative nine-key ABI changes
- Modify: `~/krw-ontology-front/supabase/migrations/20260815130000_agent_v1_final_visualizations.sql`
- Modify: `~/krw-ontology-front/src/lib/visualizations/contracts.ts`
- Modify: `~/krw-ontology-front/src/components/KrwVisualizationCard.tsx`
- Test: `~/krw-ontology-front/src/app/api/chat/[sessionId]/route.test.ts`
- Test: `~/krw-ontology-front/src/lib/visualizations/presentation.test.ts`

**Interfaces:**
- `agent_v1.read_final_projection` remains the authoritative nine-key result.
- Product projection may add `visualizations`, but invalid optional artifacts become `[]`.

- [x] **Step 1: Validate artifact shape at the product boundary**

Accept only `artifact_format="krw-visualization"`, `schema_version=4`, at most three artifacts, known chart types, finite numeric points, and bounded strings. Do not accept an HTML field, arbitrary URL, renderer function, or `dangerouslySetInnerHTML` input from the artifact.

- [x] **Step 2: Keep projection writes independent**

Insert visualization rows in a per-artifact subtransaction. If an insert, decode, or table lookup fails, keep the Markdown/message/run/billing transaction successful and write zero visualization rows.

- [x] **Step 3: Keep chat reads optional**

The chat route must return `visualizations: []` on missing table/column/row errors while returning the normal message payload and HTTP 200.

- [x] **Step 4: Verify frontend fallback**

Run:

```bash
cd ~/krw-ontology-front
npx vitest run src/lib/visualizations/presentation.test.ts \
  src/lib/visualizations/db.test.ts \
  src/components/KrwVisualizationCard.test.tsx \
  'src/app/api/chat/[sessionId]/route.test.ts'
```

Expected: valid artifacts render as chart/table; malformed or absent artifacts leave the answer visible with no chart.

---

### Task 5: Seal release and deployment preflight

**Files:**
- Modify: `scripts/build_sealed_dual_provider_release.sh`
- Modify: `~/krw-ontology-front/scripts/deploy-production-full.sh`
- Modify: `scripts/check_product_projection_compat.py`
- Test: `scripts/test_dual_provider_release.py`
- Test: `~/krw-ontology-front/src/lib/agent-v1/migration-contract.test.ts`

**Interfaces:**
- The sealed release must contain `indexes/chart_series.sqlite`, its manifest entry, release ID, and shard-manifest hash.
- `prod:deploy:full` must pass the front root to the sealed release builder and install the resulting daemon/sidecar as one release.

- [x] **Step 1: Add release preflight assertions**

Fail before packaging when the sidecar is missing, the sidecar release ID differs from the release manifest, the agent projection function is not the owner of the nine-key ABI, or the frontend migration narrows the authoritative result. These are preflight failures, not per-question failures.

- [x] **Step 2: Add dual-provider contract checks**

Run the same structured pack/invalid-pack fixtures for GLM and DeepSeek. Assert identical chart artifact JSON for identical capability results and identical Markdown when the pack is omitted.

- [x] **Step 3: Verify scripts without a production deploy**

Run:

```bash
bash -n scripts/build_sealed_dual_provider_release.sh
python3 scripts/check_product_projection_compat.py --front-root ~/krw-ontology-front
PYTHONPATH=scripts python3 scripts/test_dual_provider_release.py
```

Expected: preflight is `ready`; no feature flag is required; the release builder reports the sidecar and provenance files.

- [ ] **Step 4: Operator deploy gate**

Only after all prior tasks pass, run the operator-owned production command:

```bash
cd ~/krw-ontology-front
npm run prod:deploy:full
```

Confirm the deployed daemon reports the same release ID and sidecar hash, then run one GLM and one DeepSeek private canary. A chart can be absent; the canary is still successful when Markdown is complete and the answer run is `completed`.

---

## Acceptance Criteria

1. A financial time-series/comparison/composition question with current-run evidence produces a schema-v4 artifact with only current-run evidence references.
2. A qualitative/company-description question produces normal Markdown and zero artifacts without an additional decision turn.
3. Missing sidecar, malformed `_meta`, mismatched release, stale object ID, projection-table failure, and renderer validation failure all leave the text answer completed.
4. FY/CY, annual/quarterly, currency, unit, duration, and company/segment scope are never mixed in one artifact.
5. No MCP response contains HTML/JSX/SVG or renderer instructions.
6. GLM and DeepSeek produce the same artifact for the same normalized capability result.
7. `check_product_projection_compat.py` passes and `npm run prod:deploy:full` can package the sealed daemon without a chart feature flag.

## Handoff

Plan saved to `docs/superpowers/plans/2026-08-15-presentation-final-hardening.md`. Implementation should use `superpowers:executing-plans` and stop after each task’s focused test gate; production deployment remains an operator action after the local release preflight passes.

## Execution status (2026-08-15)

- Tasks 1–4 are implemented and verified.
- Rust workspace library tests, including the 101 `run-engine` tests, pass.
- Python sidecar suite passes (`64 passed, 1 warning`).
- Frontend typecheck and the full Vitest suite pass (`328 files, 1,452 tests`).
- Projection compatibility, gateway syntax/tests, and dual-provider release fixtures pass.
- The only remaining step is the operator-owned production release/canary gate; no live deploy was run in this session.
