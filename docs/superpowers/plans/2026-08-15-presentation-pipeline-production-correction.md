# Presentation Pipeline Production Correction Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the private MCP presentation pipeline always available in the sealed release, financially correct, replay-deterministic, and incapable of preventing the text research answer from completing.

**Architecture:** MCP carries a bounded, typed `PresentationSeriesPackV2` through private `_meta`; it never sends HTML and never exposes presentation data to the model. The durable capability observation stores the private pack beside—not inside—the provider-visible payload, and the Rust compiler deterministically selects and compiles comparable financial series into frontend artifacts. Presentation is optional at every boundary: invalid, missing, or unrenderable data becomes an empty visualization list while the research answer continues.

**Tech Stack:** Rust, Python/Pydantic, MCP `CallToolResult._meta`, PostgreSQL/Supabase PL/pgSQL, TypeScript/React, Vitest, pytest.

## Implementation status (2026-08-15)

The implementation is now on the single-path production design described below.
The Python sidecar only selects rows for validated SearchPlan clause pairs; it
does not emit a chart kind.  The Rust compiler is the sole authority for
trend/comparison/composition selection and all derived values.  The private
pack carries release and source-manifest identity, while the model-visible
ResearchState remains unchanged.

- [x] Private MCP `_meta` pack, bounded capture, replay hash, and
      failure-inert answer commit.
- [x] Clause-scoped sidecar lookup with per-ticker/metric coverage and
      release provenance.
- [x] Rust artifact schema v4 with period/currency/scope checks and
      composition reconciliation.
- [x] Agent-owned final projection plus optional product visualization
      projection and frontend fallback.
- [x] Full workspace Rust checks/tests and Python/frontend focused checks.
- [ ] Live production deployment and provider canary; these require the
      operator-owned release credentials and deployment environment.

## Global Constraints

- Do not modify `~/krw-ontology` or its ontology schema.
- Do not add an LLM turn, MCP call, tool, or skill decision for visualization.
- MCP returns structured data only; it must never return HTML for the frontend.
- Presentation metadata must never enter `provider_content`, model history, EvidenceLedger, or prompt compaction.
- A presentation error must never fail, defer, retry, or roll back the text research answer.
- Remove the chart feature flag and legacy chart compiler paths; there is one canonical path only.
- Preserve ticker, canonical metric, unit, currency, basis, duration, period type, fiscal period ordering, scope dimension, and source object IDs.
- Derived YoY/QoQ values are allowed only when the source periods are provably comparable and adjacent for that transform.
- `tools/call` remains at-most-once. Presentation handling must not introduce transport retries.
- Maximum private pack size remains 64 KiB, eight series, and twelve points per series.
- Maximum committed visualizations is three after semantic deduplication and relevance selection.
- GLM and DeepSeek use the same presentation contract and deterministic compiler.

---

## File and Ownership Map

### `krw-agnet`

- `services/krw-ontology-runtime/src/krw_capability_runtime/agent_index/chart_series.py`: builds and queries typed financial series with period and scope semantics.
- `services/krw-ontology-runtime/src/krw_capability_runtime/mcp_server/tools.py`: derives chart eligibility from the normalized SearchPlan and attaches a private pack when eligible.
- `services/krw-ontology-runtime/src/krw_capability_runtime/transport/mcp/descriptors.py`: places the pack only under MCP `_meta`.
- `services/krw-ontology-runtime/tests/test_presentation_pack.py`: sidecar, relevance, metadata, and model-isolation tests.
- `crates/krw-presentation/src/lib.rs`: owns `PresentationSeriesPackV2`, comparability checks, candidate ranking, and artifact compilation.
- `crates/capability-runtime/src/lib.rs`: extracts private `_meta` best-effort and attaches it to the durable internal result without affecting the primary capability result.
- `crates/run-engine/src/lib.rs`: persists/replays private packs, deduplicates candidates, and commits `AnswerBundle` visualizations.
- `migrations/0021_final_projection_contract.sql`: restores the single authoritative final-projection ABI.
- `packages/host-ts/src/client.ts`: continues to enforce that authoritative ABI.
- `packaging/launchd/install-local-mac-capabilityd-release.sh`: materializes the exact Origin allowlist.
- `packaging/launchd/krw-capabilityd-start-local`: contains no visualization feature flag.
- `scripts/check_product_projection_compat.py`: verifies the frontend SQL cannot redefine or narrow the Agent projection contract.

### `krw-ontology-front`

- `supabase/migrations/20260815130000_agent_v1_final_visualizations.sql`: owns only the public product projection, not `agent_v1.read_final_projection`.
- `scripts/init-mac-worker-env.sh`: stops emitting the removed chart feature flag.
- `src/lib/visualizations/contracts.ts`: consumes visualization artifact schema v4 with financial metadata.
- `src/components/KrwVisualizationCard.tsx`: renders only validated artifacts and falls back to the accessible table.
- `src/app/api/chat/[sessionId]/route.test.ts`: verifies malformed visualization rows are omitted without failing chat history.
- `src/lib/agent-v1/migration-contract.test.ts`: pins the optional-visualization transaction behavior and authoritative ABI ownership.

---

### Task 1: Remove the Dead Feature Flag and Seal Origin Configuration

**Files:**
- Modify: `services/krw-ontology-runtime/src/krw_capability_runtime/mcp_server/tools.py:2021-2040`
- Modify: `services/krw-ontology-runtime/src/krw_capability_runtime/agent_index/spine_router.py:42,3004-3006`
- Modify: `services/krw-ontology-runtime/tests/test_presentation_pack.py:180-210`
- Modify: `packaging/launchd/install-local-mac-capabilityd-release.sh:99-122,293-319`
- Modify: `~/krw-ontology-front/scripts/init-mac-worker-env.sh:293-295`

**Interfaces:**
- Produces: `query_context` automatically checks the sealed `indexes/chart_series.sqlite`; no runtime enable flag exists.
- Produces: every activated gateway config contains `allowedOrigins: ["https://krw-agent.local"]`.

- [ ] **Step 1: Replace the flag-dependent test with availability tests**

```python
def test_search_plan_attaches_pack_when_sealed_sidecar_exists(tmp_path, monkeypatch):
    indexes = tmp_path / "indexes"
    indexes.mkdir()
    _create_sidecar(indexes / "chart_series.sqlite")
    monkeypatch.setattr(ontology_tools, "_runtime_root", lambda: tmp_path)
    payload = {}
    ontology_tools._attach_chart_series_sidecar_to_search_plan_payload(
        raw_payload=payload,
        question="최근 매출 흐름",
        requested_tickers=["AAPL"],
        metric_names=["revenue"],
    )
    assert payload["presentation_series_pack"]["schema_version"] == 2


def test_missing_sidecar_omits_pack_without_error(tmp_path, monkeypatch):
    monkeypatch.setattr(ontology_tools, "_runtime_root", lambda: tmp_path)
    payload = {}
    ontology_tools._attach_chart_series_sidecar_to_search_plan_payload(
        raw_payload=payload,
        question="최근 매출 흐름",
        requested_tickers=["AAPL"],
        metric_names=["revenue"],
    )
    assert "presentation_series_pack" not in payload
```

- [ ] **Step 2: Confirm the new tests fail because the flag defaults to false**

Run: `cd services/krw-ontology-runtime && uv run pytest tests/test_presentation_pack.py -q`

Expected: the existing sidecar test fails unless it monkeypatches `_chart_series_runtime_enabled`.

- [ ] **Step 3: Remove `_CHART_SERIES_ENABLED_ENV`, `_chart_series_runtime_enabled`, its import, and its early return**

The only activation condition becomes:

```python
chart_series_path = _runtime_root() / CHART_SERIES_RELATIVE_PATH
if not chart_series_path.is_file():
    diagnostics["chart_series"] = {
        "available": False,
        "source": "chart_series_sidecar",
    }
    return
```

Delete `KRW_CHART_SERIES_ENABLED=0` from the frontend environment generator. Do not add a replacement flag.

- [ ] **Step 4: Keep and commit the exact Origin materialization already present in the installer**

The installer must both validate and write:

```json
{"allowedOrigins":["https://krw-agent.local"]}
```

- [ ] **Step 5: Run focused verification**

Run:

```bash
cd services/krw-ontology-runtime
uv run pytest tests/test_presentation_pack.py -q
cd ../../
node --test packaging/local-mcp-gateways/mcp_tls_proxy.test.mjs
bash -n packaging/launchd/install-local-mac-capabilityd-release.sh
bash -n packaging/launchd/krw-capabilityd-start-local
```

Expected: all pass, and `rg "KRW_CHART_SERIES_ENABLED" services packaging ../krw-ontology-front/scripts/init-mac-worker-env.sh` returns no matches.

- [ ] **Step 6: Commit the activation boundary**

```bash
git add services/krw-ontology-runtime packaging/launchd
git commit -m "fix: activate sealed presentation sidecar"
git -C ../krw-ontology-front add scripts/init-mac-worker-env.sh
git -C ../krw-ontology-front commit -m "fix: remove retired chart feature flag"
```

---

### Task 2: Make Presentation Failure-Inert at Every Runtime Boundary

**Files:**
- Modify: `crates/capability-runtime/src/lib.rs:1406-1478,1560-1790,2312-2395`
- Modify: `crates/run-engine/src/lib.rs:4375-4404,18082-18135`

**Interfaces:**
- Produces: `extract_presentation_pack(meta) -> Option<Value>` at this isolation stage; Task 4 replaces `Value` with `PresentationSeriesPackV2`. Malformed input returns `None`, never `DependencyFailure`.
- Produces: primary `CapabilityResult` success is independent of presentation parsing and ledger locking.

- [ ] **Step 1: Rewrite the malformed and oversized tests to assert primary success**

```rust
#[tokio::test]
async fn malformed_presentation_meta_is_omitted_without_failing_capability() {
    let (image, resolved) = fixture_image_and_runtime();
    let catalog = CapabilityCatalog::compile(&image, resolved).expect("catalog");
    let (plan, state) = fixture_plan_and_state();
    let mut bad_version = presentation_pack();
    bad_version["schema_version"] = serde_json::json!(999);
    let transport = FakeTransport::new([envelope_with_meta(
        &state,
        serde_json::json!({"com.krwontology/presentationSeries": bad_version}),
    )]);
    let runtime = PooledMcpCapabilityRuntime::for_run(
        Arc::clone(&catalog), transport, scope(),
    ).expect("runtime");
    let result = runtime
        .invoke(&invocation(&catalog, "ontology.query_context", plan))
        .await
        .expect("primary result");
    assert!(!result.evidence.is_empty());
    assert!(runtime.presentation_packs().is_empty());
}

#[test]
fn oversized_presentation_meta_is_omitted() {
    let payload = serde_json::json!({"ok": true});
    let mut oversized = presentation_pack();
    oversized["series"] = serde_json::json!(
        (0..9).map(|index| serde_json::json!({
            "series_key": index,
            "points": []
        })).collect::<Vec<_>>()
    );
    let extracted = extract_json_tool_payload(
        envelope_with_meta(
            &payload,
            serde_json::json!({"com.krwontology/presentationSeries": oversized}),
        ),
        false,
    ).expect("primary MCP payload remains valid");
    assert_eq!(extracted.payload, payload);
    assert!(extracted.presentation.is_none());
}
```

- [ ] **Step 2: Verify the tests fail under the current `transpose()?` implementation**

Run: `cargo test -p krw-agent-capability-runtime presentation -- --nocapture`

Expected: malformed and oversized packs return `presentation_meta` errors.

- [ ] **Step 3: Make private parsing non-throwing**

Change extraction to remove `_meta` first, then independently parse the private key:

```rust
let presentation = object
    .remove("_meta")
    .and_then(|meta| meta.as_object().cloned())
    .and_then(|meta| extract_presentation_pack(&meta));
```

`extract_presentation_pack` must return `Option<Value>`. It validates all limits but maps every private-format failure to `None`. Task 4 replaces the raw value with the strict v2 type. Primary `content`, `structuredContent`, and `isError` validation remains fail-closed.

- [ ] **Step 4: Make presentation storage lock failure-inert**

Replace a poisoned presentation lock error with omission:

```rust
if let (Some(pack), Ok(mut ledger)) = (presentation, self.presentation.lock()) {
    ledger.push(pack);
}
```

Do not return a dependency failure for private presentation storage.

- [ ] **Step 5: Verify runtime and final-answer behavior**

Run:

```bash
cargo test -p krw-agent-capability-runtime presentation
cargo test -p krw-agent-run-engine presentation
```

Expected: malformed, oversized, empty, and unchartable packs all produce a normal text answer with `visualizations: []`.

- [ ] **Step 6: Commit failure isolation**

```bash
git add crates/capability-runtime crates/run-engine
git commit -m "fix: isolate presentation failures from research"
```

---

### Task 3: Restore One Authoritative Final-Projection ABI

**Files:**
- Create: `migrations/0021_final_projection_contract.sql`
- Modify: `migrations/README.md`
- Modify: `~/krw-ontology-front/supabase/migrations/20260815130000_agent_v1_final_visualizations.sql:10-68,297-330`
- Modify: `scripts/check_product_projection_compat.py`
- Modify: `~/krw-ontology-front/src/lib/agent-v1/migration-contract.test.ts`

**Interfaces:**
- Produces: `agent_v1.read_final_projection(jsonb)` with exactly `run_id`, `answer_bundle_hash`, `final_output_hash`, `markdown`, `visualizations`, `usage`, `evidence_ledger_hash`, `memory_revision`, and `memory_frontier_hash`.
- Consumes: frontend public projection calls that function but never redefines it.

- [ ] **Step 1: Add a compatibility test that rejects frontend ownership of the Agent function**

```ts
it("does not redefine the agent-owned final projection", () => {
  const sql = readFileSync(VISUALIZATION_MIGRATION, "utf8");
  expect(sql).not.toMatch(/create\s+or\s+replace\s+function\s+agent_v1\.read_final_projection/i);
  expect(sql).toContain("agent_v1.read_final_projection");
});
```

Add a Python compatibility assertion that the Agent migration returns all nine exact keys and that the frontend product function only consumes them.

- [ ] **Step 2: Verify the contract tests fail against the current duplicate function**

Run:

```bash
python3 scripts/check_product_projection_compat.py --front-root ../krw-ontology-front
cd ../krw-ontology-front
npx vitest run src/lib/agent-v1/migration-contract.test.ts
```

Expected: duplicate ownership and missing projection fields are reported.

- [ ] **Step 3: Add migration 0021 as the only authoritative Agent repair**

Start from the full nine-field return shape in `0020_final_projection_visualizations.sql`. Change only visualization handling so malformed visualization data becomes `[]`:

```sql
v_visualizations := coalesce(v_bundle.answer_bundle->'visualizations', '[]'::jsonb);
if jsonb_typeof(v_visualizations) <> 'array'
   or jsonb_array_length(v_visualizations) > 3
   or exists (
     select 1 from jsonb_array_elements(v_visualizations) element
     where jsonb_typeof(element) <> 'object'
   ) then
  v_visualizations := '[]'::jsonb;
end if;
```

Retain all ownership, final-state, hash, Markdown, usage, evidence, and memory checks.

- [ ] **Step 4: Remove the `agent_v1.read_final_projection` definition from the frontend migration**

The frontend migration must begin with the public product function and call the Agent-owned function. It must not copy Agent SQL.

- [ ] **Step 5: Make individual visualization insertion optional inside the product transaction**

Use a PL/pgSQL subtransaction per artifact:

```sql
begin
  v_artifact_ref := v_artifact->>'artifact_ref';
  if v_artifact_ref is null or length(v_artifact_ref) not between 1 and 128 then
    v_visualization_omitted := v_visualization_omitted + 1;
  else
    insert into public.chat_message_visualizations (...)
    values (...)
    on conflict (message_id, artifact_ref) do nothing;
    v_visualization_inserted := v_visualization_inserted + 1;
  end if;
exception when others then
  v_visualization_omitted := v_visualization_omitted + 1;
end;
```

The `done` event reports inserted and omitted counts. It must use inserted count, not input array length.

- [ ] **Step 6: Run migration integrity and frontend contract tests**

Run:

```bash
cargo test -p krw-agent-persistence --test migration_apply
python3 scripts/check_product_projection_compat.py --front-root ../krw-ontology-front
cd ../krw-ontology-front
npx vitest run src/lib/agent-v1/migration-contract.test.ts src/app/api/chat/\[sessionId\]/route.test.ts
```

Expected: exact nine-field ABI passes; an invalid chart is omitted while the assistant message is completed.

- [ ] **Step 7: Commit both sides independently**

```bash
git add migrations scripts/check_product_projection_compat.py
git commit -m "fix: restore authoritative final projection ABI"
git -C ../krw-ontology-front add supabase/migrations src/lib/agent-v1/migration-contract.test.ts
git -C ../krw-ontology-front commit -m "fix: make chart projection failure-inert"
```

---

### Task 4: Introduce `PresentationSeriesPackV2` With Financial Semantics

**Files:**
- Modify: `services/krw-ontology-runtime/src/krw_capability_runtime/agent_index/chart_series.py:180-228,346-464,717-740`
- Modify: `services/krw-ontology-runtime/src/krw_capability_runtime/mcp_server/tools.py:1900-2085`
- Modify: `services/krw-ontology-runtime/tests/test_presentation_pack.py`
- Modify: `crates/krw-presentation/src/lib.rs:1-193`

**Interfaces:**
- Produces: `PresentationSeriesPackV2` with `search_plan_hash`, `data_release_hash`, `requested_tickers`, `chart_clauses`, and typed series.
- Produces: no pack when the SearchPlan contains no chart-eligible goal.

- [ ] **Step 1: Add Python fixture expectations for the complete v2 pack**

The exact point shape is:

```json
{
  "period": "CY2026Q1",
  "period_sort_key": 202601,
  "fiscal_year": 2026,
  "fiscal_quarter": 1,
  "start_date": "2026-01-01",
  "end_date": "2026-03-31",
  "document_type": "10-Q",
  "document_period": "CY2026Q1",
  "value": 100.0,
  "formatted_value": "$100.0M",
  "object_id": "metric-object-1"
}
```

The exact series identity includes:

```json
{
  "ticker": "AAPL",
  "canonical_metric": "revenue",
  "unit": "USD",
  "currency": "USD",
  "basis": "gaap",
  "duration": "quarter",
  "period_type": "quarterly",
  "scope": {
    "dimension": "company",
    "kind": "company_total",
    "key": "AAPL:total",
    "label": "Apple",
    "composition_eligible": false
  }
}
```

- [ ] **Step 2: Add relevance tests**

```python
def test_qualitative_plan_does_not_emit_presentation_pack():
    plan = SearchPlan.model_validate({
        "question": "What kind of company is Apple?",
        "intent": "company_research",
        "tickers": ["AAPL"],
        "clauses": [{
            "clause_id": "business_profile",
            "retrieval_query": "AAPL business segments",
            "required_concepts": ["business segments"],
        }],
    })
    assert chart_clauses_for_plan(plan) == []


def test_temporal_metric_clause_emits_only_its_metric():
    plan = SearchPlan.model_validate({
        "question": "Show the recent AAPL revenue trend",
        "intent": "company_research",
        "tickers": ["AAPL"],
        "clauses": [{
            "clause_id": "revenue_trend",
            "retrieval_query": "AAPL revenue trend",
            "metrics": ["revenue"],
            "calculation_window": "period_over_period",
        }],
    })
    clauses = chart_clauses_for_plan(plan)
    assert [(item.clause_id, item.intent, item.metric_names) for item in clauses] == [
        ("revenue_trend", "trend", ["revenue"]),
    ]
```

- [ ] **Step 3: Verify current code loses fields and emits broad candidates**

Run: `cd services/krw-ontology-runtime && uv run pytest tests/test_presentation_pack.py -q`

Expected: v2 metadata and no-chart profile tests fail.

- [ ] **Step 4: Build the pack from SearchPlan goals, not question keywords**

Use this closed mapping over existing validated `QueryClause` fields without adding a model field:

```text
metrics + calculation_window -> trend
metrics + two or more effective clause/plan tickers -> comparison
metrics + metric_scope=dimensioned + one metric dimension -> composition candidate
metrics without any of the three shapes above -> no chart
clauses without metrics -> no chart
```

Remove `_metric_candidates_for_question`, `_scope_candidates_for_question`, and broad “query every metric” fallback from the presentation path. The research retrieval path remains unchanged.

- [ ] **Step 5: Preserve every financial ordering and source field from SQLite**

Extend `_chart_series_points` to copy `period_sort_key`, fiscal year/quarter, document fields, and dates. Add `currency` and `scope_dimension` columns to the sidecar schema/build projection. Bump the sidecar and pack schema versions once; do not retain a v1 reader.

- [ ] **Step 6: Parse into strict Rust structs**

Define serde types in `krw-presentation` with `deny_unknown_fields` and bounded constructors:

```rust
pub struct PresentationSeriesPackV2 {
    pub schema_version: u16,
    pub search_plan_hash: String,
    pub data_release_hash: String,
    pub requested_tickers: Vec<String>,
    pub chart_clauses: Vec<ChartClause>,
    pub series: Vec<PresentationSeries>,
}
```

`ChartClause` contains `clause_id`, `required`, `intent`, `metric_names`, and optional `scope_dimension`. The bounded constructor validates both hash strings as lowercase `sha256:<64 hex>` values and validates that every series maps to one chart clause. At finalization, the Run Engine uses its existing `research_intent_receipt.clause_goal_ids` mapping to add kernel goal IDs to the artifact; the MCP server never invents goal IDs.

- [ ] **Step 7: Run Python and Rust schema tests**

Run:

```bash
cd services/krw-ontology-runtime
uv run pytest tests/test_presentation_pack.py -q
cd ../../
cargo test -p krw-presentation pack_v2
```

- [ ] **Step 8: Commit the typed private contract**

```bash
git add services/krw-ontology-runtime crates/krw-presentation
git commit -m "feat: add typed financial presentation pack v2"
```

---

### Task 5: Replace Heuristic Charting With a Financially Correct Compiler

**Files:**
- Modify: `crates/krw-presentation/src/lib.rs:195-575`
- Modify: `~/krw-ontology-front/src/lib/visualizations/contracts.ts`
- Modify: `~/krw-ontology-front/src/components/KrwVisualizationCard.test.tsx`

**Interfaces:**
- Consumes: `PresentationSeriesPackV2` from Task 4.
- Produces: visualization artifact schema v4 with `goal_ids`, `currency`, `basis`, comparable periods, and complete provenance.

- [ ] **Step 1: Add table-driven compiler tests before changing the algorithm**

Cover these exact cases:

```text
FY2023, FY2025                    -> trend allowed; YoY transform forbidden
FY2024, FY2025                    -> YoY allowed
FY2025Q1, FY2025Q3                -> QoQ forbidden
FY2025Q4, FY2026Q1                -> QoQ allowed
FY2025Q1, FY2026Q1                -> YoY allowed
CY2026Q1 mixed with FY2025 annual -> never grouped
USD mixed with EUR                -> never grouped
GAAP mixed with adjusted          -> never grouped
product plus geography scopes     -> never one composition
negative component                -> no donut/stacked composition
components not reconciling to total within 1% -> grouped bar or table, not composition
one reconciled period             -> donut
two or more reconciled periods    -> stacked_bar
qualitative goal                  -> no artifact
duplicate packs                   -> one artifact
```

- [ ] **Step 2: Verify current compiler fails the period and scope tests**

Run: `cargo test -p krw-presentation -- --nocapture`

Expected: lexical ordering, adjacency, mixed-scope, and duplicate tests fail.

- [ ] **Step 3: Introduce a strict comparability key**

```rust
struct ComparabilityKey {
    clause_id: String,
    canonical_metric: String,
    unit: String,
    currency: Option<String>,
    basis: String,
    duration: String,
    period_type: PeriodType,
    scope_dimension: String,
}
```

Ticker is excluded only for a multi-company comparison clause. Scope key is excluded only for a same-dimension composition clause.

- [ ] **Step 4: Replace lexical period logic**

Sort only by `period_sort_key`, then validate transforms:

```rust
fn annual_yoy_pair(a: &Point, b: &Point) -> bool {
    a.fiscal_year.zip(b.fiscal_year)
        .is_some_and(|(left, right)| right == left + 1)
        && a.period_type == PeriodType::Annual
        && b.period_type == PeriodType::Annual
}
```

Implement separate quarterly QoQ, quarterly YoY, and same-duration YTD YoY predicates. If no predicate passes, retain raw trend points but emit no derived growth view.

- [ ] **Step 5: Implement composition reconciliation**

A composition requires the same ticker, goal, metric, unit, currency, basis, duration, period, and scope dimension. It also requires `composition_eligible=true`, non-negative components, and:

```text
abs(sum(components) - total) / max(abs(total), 1) <= 0.01
```

One valid period produces donut; two or more valid periods produce stacked bar. Otherwise use grouped bar/table or omit.

- [ ] **Step 6: Add deterministic ranking and semantic deduplication**

Rank candidates by this stable tuple:

```text
required goal before optional goal
explicit trend/comparison/composition before inferred support
full comparability before partial comparability
more valid periods before fewer periods
newer maximum period_sort_key before older
goal_id, metric, semantic fingerprint as lexical tie-breakers
```

Deduplicate by the full semantic fingerprint, then retain at most three artifacts. Strip the `sha256:` prefix before taking the first sixteen digest characters for `artifact_ref`.

- [ ] **Step 7: Emit artifact schema v4 and update the frontend validator**

Add `goal_ids`, `currency`, `basis`, `period_sort_key`, and source document metadata to the artifact. Frontend validation must reject an individual invalid artifact and continue rendering the message and remaining valid artifacts.

- [ ] **Step 8: Run compiler and renderer tests**

Run:

```bash
cargo test -p krw-presentation
cd ../krw-ontology-front
npx vitest run src/components/KrwVisualizationCard.test.tsx src/lib/visualizations/db.test.ts
```

- [ ] **Step 9: Commit compiler and frontend contract changes**

```bash
git add crates/krw-presentation
git commit -m "fix: compile financially comparable visualizations"
git -C ../krw-ontology-front add src/lib/visualizations src/components/KrwVisualizationCard.test.tsx
git -C ../krw-ontology-front commit -m "feat: render visualization artifact v4"
```

---

### Task 6: Persist Private Presentation Data for Exact Replay

**Files:**
- Modify: `crates/run-engine/src/lib.rs:712-780,3425-3518,3704-3737,4841-5105,7056-7155`
- Modify: `crates/capability-runtime/src/lib.rs:735-814,1406-1489,1520-1557`
- Modify: `crates/run-engine/src/lib.rs:13850-13980,18057-18135` tests

**Interfaces:**
- Produces: `CapabilityResult.private_presentation: Option<PresentationSeriesPackV2>` persisted inside the durable action result but omitted from provider-visible projection.
- Produces: `ActiveRun.presentation_packs` reconstructed identically during live execution and recovery.

- [ ] **Step 1: Add a crash/recovery equivalence test**

```rust
#[tokio::test]
async fn recovered_run_commits_the_same_visualizations_as_live_run() {
    let live = run_to_completion_with_presentation_pack().await;
    let recovered = crash_after_observe_then_recover().await;
    assert_eq!(live.answer_bundle.visualizations, recovered.answer_bundle.visualizations);
    assert_eq!(live.answer_bundle_hash, recovered.answer_bundle_hash);
}
```

Add a provider transcript assertion that the serialized tool result does not contain `com.krwontology/presentationSeries`, `private_presentation`, or any presentation point.

- [ ] **Step 2: Verify recovery currently loses the pack**

Run: `cargo test -p krw-agent-run-engine recovered_run_commits_the_same_visualizations_as_live_run -- --nocapture`

Expected: live output contains a chart and recovered output does not.

- [ ] **Step 3: Add the private field to the durable internal result**

```rust
pub struct CapabilityResult {
    pub provider_content: Value,
    #[serde(default)]
    pub evidence: Vec<EvidenceRecord>,
    pub answerability: Option<Answerability>,
    #[serde(default)]
    pub calculations: Vec<Calculation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub private_presentation: Option<PresentationSeriesPackV2>,
}
```

Update `Drop` to clear the private pack. `model_visible_capability_result` must continue cloning only `provider_content` plus kernel-owned goal aliases.

- [ ] **Step 4: Remove the runtime-only mutex ledger and trait accessor**

Delete `PooledMcpCapabilityRuntime.presentation`, `CapabilityRuntime::presentation_packs`, and commit-time reads from the runtime object. On every accepted/replayed action, `ActiveRun::ingest` copies a valid private pack into bounded `ActiveRun.presentation_packs` and includes its semantic hash in the checkpoint.

- [ ] **Step 5: Compile from durable `ActiveRun` state**

Change:

```rust
let visualizations = self.compile_visualizations();
```

to:

```rust
let visualizations = compile_visualizations(&state.presentation_packs);
```

The compiler still returns an empty vector on any private error.

- [ ] **Step 6: Run persistence, recovery, and no-model-leak tests**

Run:

```bash
cargo test -p krw-agent-run-engine presentation
cargo test -p krw-agent-run-engine recovered_run_commits_the_same_visualizations_as_live_run
cargo test -p krw-agent-capability-runtime presentation
```

- [ ] **Step 7: Commit durable presentation replay**

```bash
git add crates/run-engine crates/capability-runtime
git commit -m "feat: persist private presentation attachments"
```

---

### Task 7: Keep Frontend Presentation Simple and Failure-Inert

**Files:**
- Modify: `~/krw-ontology-front/src/components/ResearchMarkdown.tsx:418-438`
- Modify: `~/krw-ontology-front/src/app/api/chat/[sessionId]/route.ts`
- Modify: `~/krw-ontology-front/src/app/api/chat/[sessionId]/route.test.ts`

**Interfaces:**
- Consumes: zero to three independently validated visualization rows.
- Produces: Markdown always renders first; a bounded “관련 데이터” gallery follows only when at least one valid artifact exists.

- [ ] **Step 1: Add frontend behavior tests**

```text
no visualization rows             -> Markdown only
one valid row                     -> Markdown plus one card
one invalid row                   -> Markdown only, HTTP 200
one valid and one invalid row     -> Markdown plus one card
four valid rows                   -> first three deterministic rows only
renderer exception in one card    -> remaining message content stays visible
```

- [ ] **Step 2: Use one deterministic fallback placement**

Do not infer chart placement from Markdown headings or model prose. When no explicit persisted block list exists, sort valid rows by artifact rank and semantic fingerprint, render them after Markdown in one accessible section, and retain the table fallback inside each card. Existing explicit persisted blocks remain readable but this pipeline does not create new fuzzy blocks.

- [ ] **Step 3: Bound and isolate parsing in the chat read route**

Each database row is parsed independently. Invalid rows are dropped; the route never rejects the full chat response because of a visualization row.

- [ ] **Step 4: Run focused frontend tests**

Run:

```bash
cd ../krw-ontology-front
npx vitest run src/app/api/chat/\[sessionId\]/route.test.ts src/components/KrwVisualizationCard.test.tsx src/lib/visualizations/db.test.ts
```

- [ ] **Step 5: Commit the stable frontend projection**

```bash
git add src/app/api/chat/\[sessionId\] src/components/ResearchMarkdown.tsx src/lib/visualizations
git commit -m "fix: isolate visualization rendering from chat"
```

---

### Task 8: Add Release Gates and Provider-Independent Acceptance

**Files:**
- Modify: `scripts/check_product_projection_compat.py`
- Modify: `scripts/build_dual_provider_release.sh`
- Modify: `~/krw-ontology-front/scripts/deploy-production-full.sh`
- Modify: `~/krw-ontology-front/scripts/verify-krw-agent-production-release.mjs`
- Modify: `docs/superpowers/plans/2026-08-15-presentation-pipeline-production-correction.md` only to mark completed checkboxes during execution

**Interfaces:**
- Produces: a sealed release is rejected before admission opens if private presentation, SQL ABI, Origin, or replay invariants fail.
- Produces: presentation absence never blocks deployment when the data itself is legitimately unchartable.

- [ ] **Step 1: Add static release assertions**

The release verifier must assert:

```text
no KRW_CHART_SERIES_ENABLED symbol in sealed launch/runtime files
chart_series.sqlite exists and its schema version is v2
gateway allowedOrigins is exactly https://krw-agent.local
Agent final projection returns all nine canonical fields
frontend migration does not define agent_v1.read_final_projection
artifact schema version is exactly 4 on both Rust and frontend sides
```

- [ ] **Step 2: Add three deterministic fixture acceptance cases**

```text
company profile question -> successful text, zero charts
three-period revenue trend -> successful text, one trend chart
malformed private pack -> successful text, zero charts
```

Run the same fixture matrix through GLM and DeepSeek release images. Compare visualization JSON and hashes; model prose may differ, but deterministic artifacts must match for identical private packs.

- [ ] **Step 3: Add recovery acceptance**

Interrupt after durable capability observation and before final commit, resume the run, and assert identical `answer_bundle_hash`, visualization fingerprints, and frontend rows.

- [ ] **Step 4: Fix full-deploy ordering**

The full deployment order becomes:

```text
build and verify GLM/DeepSeek sealed releases
apply frontend/public migrations
apply Agent migrations through 0021
run cross-repo projection compatibility check
install capabilityd and TLS gateway
verify sidecar/readiness/Origin
start krw-agentd
run provider-independent presentation canary
open admission
```

Do not open admission if the Agent projection ABI or gateway readiness fails. Do open admission when charts are legitimately absent for an unchartable question.

- [ ] **Step 5: Run the complete focused gate**

Run:

```bash
cargo test -p krw-presentation
cargo test -p krw-agent-capability-runtime presentation
cargo test -p krw-agent-run-engine presentation
cargo test -p krw-agent-persistence --test migration_apply
cd services/krw-ontology-runtime && uv run pytest tests/test_presentation_pack.py -q && cd ../../
node --test packaging/local-mcp-gateways/mcp_tls_proxy.test.mjs
python3 scripts/check_product_projection_compat.py --front-root ../krw-ontology-front
cd ../krw-ontology-front
npm run agent-v1:compat:verify
npx vitest run src/lib/agent-v1/migration-contract.test.ts src/app/api/chat/\[sessionId\]/route.test.ts src/components/KrwVisualizationCard.test.tsx src/lib/visualizations/db.test.ts
```

Expected: all pass without invoking a live provider. Live GLM/DeepSeek canaries run only after these deterministic gates pass.

- [ ] **Step 6: Commit deployment gates**

```bash
git add scripts docs/superpowers/plans/2026-08-15-presentation-pipeline-production-correction.md
git commit -m "build: gate the presentation pipeline release"
git -C ../krw-ontology-front add scripts
git -C ../krw-ontology-front commit -m "build: verify presentation pipeline before admission"
```

---

## Completion Criteria

- A generic company-profile question completes with text and no arbitrary chart.
- A valid trend/comparison/composition question emits no more than three relevant charts.
- FY/CY, annual/quarterly/YTD, currency, unit, basis, and scope mismatches cannot be charted together.
- Missing periods cannot be mislabeled as YoY or QoQ.
- Invalid private metadata, compiler rejection, SQL insertion failure, and frontend artifact rejection all preserve the text answer.
- A resumed run produces the same visualization artifacts and `answer_bundle_hash` as uninterrupted execution.
- Agent and frontend use one exact `read_final_projection` ABI.
- The sealed release requires the exact Origin and has no chart enable/disable flag.
- No presentation bytes appear in provider-visible tool results or model transcripts.
- Both GLM and DeepSeek releases pass the same deterministic presentation gates.
