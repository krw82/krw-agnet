# Presentation Pipeline Cutover Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move chart data out of the model-visible `ResearchState` into a private MCP `_meta` channel, compile final chart artifacts deterministically in a new Rust crate, and attach them to `AnswerBundle` — the frontend's existing Nivo renderer and DB tables stay, dead `compose_visualization` remnants go.

**Architecture:** The Python ontology runtime already builds a verified metric pack from a chart-series SQLite sidecar but injects it into `ResearchState` (model context) behind a keyword gate, and drops it first under response-size pressure. Instead: emit a metadata-rich `PresentationSeriesPackV1` under `_meta["com.krwontology/presentationSeries"]` of the `tools/call` result; `capability-runtime` captures it into a per-run ledger (never model-visible); at commit, `run-engine` feeds the ledger to the new `krw-presentation` compiler which decides chart-worthiness deterministically (≥3 same-basis periods → trend; breakdown without negatives → composition; etc.) and emits `KrwVisualizationArtifact` JSON (frontend contract v3); `AnswerBundle` carries `visualizations`; the gateway exposes them; the frontend persists them via its already-built (but unwired) `persistCreatedVisualizations`.

**Tech Stack:** Python 3.11 (mcp 1.x SDK, pydantic v2) in `services/krw-ontology-runtime`; Rust workspace (serde, serde_jcs, sha2 via existing `ContentHash`); TypeScript/Next.js frontend (`krw-ontology-front`, no renderer changes).

## Global Constraints

- The `_meta` vendor key is exactly `com.krwontology/presentationSeries` (reverse-DNS, MCP 2026 `_meta` convention).
- `PresentationSeriesPackV1` is bounded: ≤8 series, ≤12 points/series, ≤64 KiB canonical JSON on the Rust side.
- Chart failure NEVER fails the run: compile errors produce zero artifacts plus a `presentation_omitted` diagnostic; the text answer still commits.
- Model context must not contain chart pack data after cutover: `ResearchState` loses `metric_series_pack` everywhere (Python model, Rust `ResearchStateV2`, `krw-contracts` exact-keys, adapter sidecar-evidence mapping).
- No extra MCP call, no HTML from MCP, no LLM involvement in chart-type decisions.
- Artifact JSON must satisfy the frontend `KrwVisualizationArtifact` contract verbatim: `artifact_format: "krw-visualization"`, `schema_version: 3`, view/dataset/point field names exactly as in `krw-ontology-front/src/lib/visualizations/contracts.ts`.
- Frontend renderer (`KrwVisualizationCard.tsx`), `chat_message_visualizations` table, and read path are untouched.

## Verified current state (2026-08-15)

- Python: `spine_router.py:3068` keyword-gates attachment; `chart_series.py:346` builds the pack (metadata-rich) but SQLite candidate query has no `ORDER BY` before `LIMIT`; `contracts.py:1194` puts it into `ResearchState`; `contracts.py:1206-1210` drops it first under size pressure; `contracts.py:1311` emits `content`+`structuredContent` (no `_meta`).
- Rust: `capability-runtime/src/lib.rs:1542` already tolerates a top-level `_meta` key in the tools/call envelope but discards it (`extract_json_tool_payload` returns only the structured payload); `krw-contracts` `RESEARCH_STATE_V2` exact-keys requires `metric_series_pack`; `krw-ontology-adapter` maps it to sidecar evidence records (`map_metric_series_pack_records`, struct field at line 64).
- `AnswerBundle` (`run-engine/src/lib.rs:1990`) has no visualizations field, `schema_version: 3`, single construction site at `run-engine/src/lib.rs:4152`.
- Gateway serves `final_output { markdown, final_output_hash }` (`bins/krw-agent/src/gateway.rs:333-337`).
- Frontend: `persistCreatedVisualizations` (`db.ts:60`) exists, tested, and has NO production caller; the chat history route reads `chat_message_visualizations`; `create-visualization-tool.ts:15` throws `visualization_composition_owned_by_krw_agent` (tool already decommissioned).

---

### Task 1: Python emits `PresentationSeriesPackV1` in `_meta`

**Files:**
- Modify: `services/krw-ontology-runtime/src/krw_capability_runtime/agent_index/spine_router.py` (attach fn ~3020-3112)
- Modify: `services/krw-ontology-runtime/src/krw_capability_runtime/agent_index/chart_series.py` (query ~346-440, point fetch helper)
- Modify: `services/krw-ontology-runtime/src/krw_capability_runtime/mcp_server/contracts.py` (ResearchState model, compaction fn ~1226-1287, size-drop branch ~1206, `_collect_evidence_candidates` ~1340-1346)
- Modify: `services/krw-ontology-runtime/src/krw_capability_runtime/transport/mcp/descriptors.py` (`DispatchOutcome` ~74-87, query_context emission site)
- Test: existing pytest files under `services/krw-ontology-runtime/tests/` referencing `metric_series_pack` / `chart_series` (locate with grep)

**Interfaces:**
- Produces: internal payload key `payload["presentation_series_pack"] = {"schema_version": 1, "mode": "chart_series_sidecar", "series": [...]}` where each series is the metadata-rich dict already built in `query_chart_series_pack` (series_key, label, ticker, metric_name, canonical_metric, unit, scope{kind,key,label}, basis, duration, source_class, statement_family, period_type, points[{period, value, formatted_value, object_id}]).
- Produces: `DispatchOutcome.meta: JsonObject | None`; `as_mcp_result()` passes it as `CallToolResult(meta=...)` (mcp SDK serializes as `_meta`). Wire value: `{"com.krwontology/presentationSeries": pack}`.
- Removes: `ResearchState.metric_series_pack` field; `_compact_visualization_metric_series_pack`; the drop-for-size branch; `_should_attach_chart_series` keyword gate; `research_pack["metric_series_pack"]`/`chart_series_pack` copies.

- [ ] **Step 1: Failing tests** — (a) research-state compile test asserting `metric_series_pack` absent from state even when the sidecar matched; (b) query_context dispatch test asserting result `_meta["com.krwontology/presentationSeries"]` present with bounded series and metadata (basis/unit/scope); (c) pack attached even when the question has no metric keywords (gate removed) as long as plan tickers/metrics match sidecar rows.
- [ ] **Step 2: Run pytest — expect the new failures.**
- [ ] **Step 3: Implement** the spine_router attach rewrite (plan-driven: prefer metrics named in the SearchPlan clauses over question keywords for candidate filtering; keyword list stays only as fallback when the plan carries no metrics), the contracts.py removals, `ORDER BY ticker, canonical_metric, scope_kind, scope_key, series_key` before `LIMIT` in the candidate query and deterministic period ordering in `_chart_series_points`, and `DispatchOutcome.meta` plumbing.
- [ ] **Step 4: Full pytest run for the service; all green.**
- [ ] **Step 5: Commit** `feat(runtime): presentation series pack via MCP _meta, plan-driven and deterministic`

### Task 2: Rust contract/adapter clean-cut

**Files:**
- Modify: `crates/krw-contracts/src/lib.rs` (`validate_research_state` exact-keys ~1498-1530)
- Modify: `crates/krw-ontology-adapter/src/lib.rs` (struct field 64, mapping 1407-1419, `map_metric_series_pack_records` 1702+, tests 3721/3775/3886)
- Test: both crates' existing test modules

**Interfaces:**
- Removes: `metric_series_pack` from the `RESEARCH_STATE_V2` required key set and from `ResearchStateV2`; sidecar evidence mapping deleted (calculations keep primary `EvidenceUnit` backing only).

- [ ] **Step 1: Update/replace adapter tests** that set `state.metric_series_pack` — they become "pack never enters evidence" assertions.
- [ ] **Step 2: `cargo test -p krw-contracts -p krw-ontology-adapter` — expect failures, then implement removals, then green.**
- [ ] **Step 3: Commit** `refactor(contracts): drop metric_series_pack from ResearchStateV2`

### Task 3: capability-runtime captures the presentation pack

**Files:**
- Modify: `crates/capability-runtime/src/lib.rs` (`extract_json_tool_payload` 1531-1694, `map_outcome_async` 1401-1457, runtime struct ~728-738, trait `CapabilityRuntime`)
- Test: same file test module

**Interfaces:**
- `extract_json_tool_payload` → `Result<(Value, Option<Value>), DependencyFailure>` returning `(payload, presentation_pack)`; unknown `_meta` keys are ignored; the vendor key's value is shape-checked (object, `schema_version == 1`, bounded series/points) and rejected as `DependencyFailure` only when present-but-malformed (a valid pack absent is normal).
- `PooledMcpCapabilityRuntime` gains `presentation: Arc<Mutex<Vec<Value>>>`; `map_outcome_async` pushes validated packs (post-`extract`) for `EvidenceMapping::ResearchStateV2`.
- `CapabilityRuntime` trait gains `fn presentation_packs(&self) -> Vec<Value> { Vec::new() }`; the pooled runtime returns a clone of the ledger. Other trait impls (test fakes) inherit the default.

- [ ] **Step 1: Failing tests** — envelope with `_meta` vendor key yields pack in ledger and unchanged normalized `CapabilityResult`; malformed pack → `DependencyFailure` with a `presentation_meta` reject code; unknown `_meta` keys ignored.
- [ ] **Step 2: Implement; `cargo test -p capability-runtime` green.**
- [ ] **Step 3: Commit** `feat(capability-runtime): per-run presentation ledger from MCP _meta`

### Task 4: `krw-presentation` crate

**Files:**
- Create: `crates/krw-presentation/Cargo.toml`, `crates/krw-presentation/src/lib.rs` (types + compiler + tests)
- Modify: workspace `Cargo.toml` members

**Interfaces:**
- `pub const PRESENTATION_META_KEY: &str = "com.krwontology/presentationSeries";`
- `pub fn compile(pack: &Value, question: &str) -> Result<Vec<Value>, PresentationOmission>` — input is the raw `_meta` pack JSON, output artifacts are `serde_json::Value` shaped exactly as frontend `KrwVisualizationArtifact`.
- Decision rules (deterministic, no keywords): group by `(canonical_metric, unit, basis)`; mixed-basis group → omit `mixed_basis`; trend view (line, dataset `{kind:"series"}`) when ≥3 points on one series or ≥2 comparable series; cross-ticker comparison at latest period → bar; breakdown scope kinds (segment/product/geography) with no negatives at one period → donut (dataset `{kind:"composition"}`), multiple periods → stacked_bar; derived series `yoy_percent`/`qoq_percent`/`share_of_total`/`cumulative` computed in Rust with `derived_from_evidence_refs` and `source_kinds += "derived"`; single point / single series-single period → omit `insufficient_data`; NaN/inf/non-finite values drop the point.
- `semantic_fingerprint` = sha256 over serde_jcs of the selected-data identity; `artifact_ref = "viz_" + fingerprint[..16]`; `evidence_ref` per point = the sidecar `object_id`; `provenance.source_tool_call_ids` = `[]` (pack arrives outside tool-call identity; evidence refs carry lineage).
- `intent` synthesized: `{goal: <deterministic from metric+scope labels>, measures: [canonical_metric], relationship: "trend"|"comparison"|"composition", scope_mode: "total"|"breakdown", transform_hints: [...]}`.

- [ ] **Step 1: Golden unit tests** for trend (multi-series), donut snapshot, stacked periods, comparison bar, yoy derivation math (exact expected values), omission cases (mixed basis, insufficient data, negative composition). Assert artifacts pass an in-crate JSON-schema-ish field check mirroring the TS contract.
- [ ] **Step 2: Implement; `cargo test -p krw-presentation` green.**
- [ ] **Step 3: Commit** `feat(krw-presentation): deterministic visualization compiler`

### Task 5: run-engine `AnswerBundle.visualizations`

**Files:**
- Modify: `crates/run-engine/src/lib.rs` (struct ~1988-2009, commit site ~4152, any schema assertions)
- Test: same file test module + any AnswerBundle constructors elsewhere (grep `schema_version: 3`)

**Interfaces:**
- `AnswerBundle` gains `#[serde(default)] pub visualizations: Vec<Value>`; `schema_version` → 4.
- At commit: `let visualizations = self.capabilities.presentation_packs().iter().filter_map(|pack| krw_presentation::compile(pack, &input.request.question).ok().map(...)).flatten().collect()` — compile failure logs under the existing debug-env pattern and yields empty vec (`presentation_omitted`), never an engine error.

- [ ] **Step 1: Failing test** — fake runtime returns one pack; committed bundle carries one artifact with the exact fingerprint; pack that fails to compile still commits with empty `visualizations`.
- [ ] **Step 2: Implement; `cargo test -p run-engine` green (fix any schema_version assertions).**
- [ ] **Step 3: Commit** `feat(run-engine): visualizations in AnswerBundle (schema v4)`

### Task 6: gateway exposes visualizations

**Files:**
- Modify: `bins/krw-agent/src/gateway.rs` (FinalOutputResponse ~33, ~282, ~333-337, tests)

- [ ] **Step 1: Failing test** — final run status JSON includes `final_output.visualizations` array passthrough.
- [ ] **Step 2: Implement; `cargo test -p krw-agent` green.**
- [ ] **Step 3: Commit** `feat(gateway): serve run visualizations`

### Task 7: frontend ingestion + dead code removal

**Files:**
- Modify: `krw-ontology-front/src/lib/api/chat/*` (the site that receives the final run result — locate the agent-run client)
- Modify/Delete: `krw-ontology-front/src/lib/visualizations/compiler.ts`, `recipes.ts`, `dataset-registry.ts`, `create-visualization-tool.ts` remnants — delete only if no production importer remains (renderer imports `contracts.ts` types only); keep `contracts.ts`, `db.ts`, `KrwVisualizationCard.tsx`.
- Test: corresponding `.test.ts` updates.

- [ ] **Step 1: Failing test** — ingestion of a final output with `visualizations` calls `persistCreatedVisualizations` with `(sessionId, messageId, runId, artifacts)` (mock the db module).
- [ ] **Step 2: Implement + run frontend vitest for touched files; delete orphaned compiler files and their tests.**
- [ ] **Step 3: Commit** `feat(front): persist agent-run visualizations; drop compose_visualization remnants`

## Final verification

- `cargo test --workspace` (export PATH to rustup toolchain 1.97.1 first), `KRW_ONTOLOGY_ROOT` set.
- `cd services/krw-ontology-runtime && uv run pytest` (or the service's documented runner).
- Frontend: `npm test -- src/lib/visualizations src/lib/api/chat` scoped.
- Optional (time permitting): local stack smoke run per `krw-agnet-build-env` memory.
