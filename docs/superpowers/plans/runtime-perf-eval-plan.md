# Runtime Performance & Eval Strengthening Plan

## Context

Architecture review (verified against code by 3 parallel reviewer subagents) identified 6 P0 issues + 6 speed priorities. This plan executes the items the user approved, excluding:
- **P0-1/P0-2** (front integration — `krw-ontology-front` is off-limits)
- **analyst turn skip (Speed 1)** — deferred per user decision; revisit after all else done

Verified facts driving this plan (file:line citations in exploration reports):
- `objective_frontier_is_exhausted` is dead code (Speed 1 — deferred)
- MCP `attested-stateless-v1` policy exists and fail-closes, but all bindings use `run-scoped`
- `read_final_output` returns only markdown, checks tenant+run_id (NOT principal/session)
- Concurrency defaults: `max_active_runs=16`, `worker_threads=2` (hardcoded), `db_max=8`, `deepseek_idle=8`
- `BudgetUsage` has zero `_ms` fields; only coarse run-duration histogram exists
- `parallel_safe` field has zero Rust read sites (dead)
- Numeric grader matches "closest number in whole answer" — ignores metric/fiscal_year/ticker
- `run_eval_v2.sh` PASS = `state=final` only; grader is informational
- `apply_migrations.sh` records checksums but never verifies them on re-run
- No fresh-DB migration test exists

## Global Constraints

- ONLY modify `~/krw-agnet`. NEVER read or write `~/krw-ontology`, `~/krw-ontology-data`, `krw-ontology-front`.
- `cargo` at `/opt/homebrew/Cellar/rustup/1.29.0_2/bin/cargo` (prepend to PATH).
- `KRW_ONTOLOGY_ROOT=$HOME/krw-ontology` required for tests (read-only access is fine).
- Rust toolchain 1.97.1 (rust-toolchain.toml).
- Every task must end with: `cargo build --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --check` all clean. Modified crates' tests must pass.
- Migrations are numbered `NNNN_*.sql`, applied in lexical order. Next free number is `0011`.
- Migration files own their own `BEGIN/COMMIT`; never wrap in outer transaction.
- `deny_unknown_fields` on all protocol structs — field additions need careful serde handling.
- No rolling compatibility for migrations (0002 drops prior forms).
- `answer_bundles.answer_bundle` JSONB already embeds `evidence_ledger_hash`, `evidence_ids`, `usage`, `agent_image_hash` — projection expansion reads existing data.
- `PublicCitation` lives only on `EvidenceRecord.citation` (in-memory during run); NOT serialized into answer_bundle. Citation projection requires commit-time capture (out of scope for this plan).
- `tool_session_reuse` has NO serde default — every binding declares it explicitly.
- `parallel_safe` is parsed into `CapabilitySpec` and hashed into `agent_image_hash`; removing it breaks ABI.

## Tasks

### Task 1: Migration safety net (infra prerequisite)

**Files:**
- `scripts/apply_migrations.sh` (modify — add checksum verification for already-applied migrations)
- `crates/persistence/tests/migration_apply.rs` (new — fresh-DB apply test using existing test infra)

**Spec:**
1. In `apply_migrations.sh`, after reading `APPLIED_MAX`, for each already-applied migration (version <= APPLIED_MAX), verify `shasum -a 256` of the file matches the stored `checksum` in `schema_migrations`. On mismatch, print `checksum_mismatch: <file>` to stderr and exit non-zero. The 0009 backfill rows use literal `'backfill'` — skip verification for those (checksum is not a real hash).
2. Add a Rust integration test that locates migrations via `include_str!` (follow the pattern in `crates/persistence/src/agent_v1.rs:26-36` which already includes 0001-0006) OR reads the `migrations/` directory. The test applies all migrations in order to a fresh in-memory or temp Postgres (use the existing test infrastructure in `crates/runtime-persistence/tests/` or `crates/persistence/` — find what's already there for DB tests). Assert no error. If no DB test infrastructure exists, write a test that at minimum validates every migration file is non-empty, has matching `BEGIN/COMMIT`, and the version number in filename is monotonic.
3. Update `migrations/README.md` to document the checksum verification policy.

**Test command:** `cargo test -p krw-agent-persistence migration` (or appropriate package name — check Cargo.toml).

---

### Task 2: read_final_projection ABI (P0-3 fix)

**Files:**
- `migrations/0011_read_final_projection.sql` (new)
- `packages/host-ts/src/client.ts` (modify — add `readFinalProjection` + parser)
- `packages/host-ts/src/contracts.ts` (modify — add `ReadFinalProjectionResponse`)
- `packages/host-ts/src/postgres.ts` (modify — add prepared statement name)

**Spec:**
1. Create `migrations/0011_read_final_projection.sql` defining `agent_v1.read_final_projection(p_request jsonb) RETURNS jsonb`. Model it on `0006_read_final_output.sql` but:
   - Required request fields: `['abi_version','run_id','tenant_id','principal_id','session_id']` (strict — no optionals).
   - Ownership: look up `runs` by `run_id`, assert `tenant_id` matches, AND assert `principal_id` matches `immutable_snapshot->'ownership'->>'principal_id'`, AND assert `session_id` matches `immutable_snapshot->'ownership'->>'session_id'`. Raise `K1017 tenant_mismatch` / new `K1027 principal_mismatch` / new `K1028 session_mismatch` on violation.
   - State check: `state <> 'final'` → `K1024 final_output_unavailable` (reuse existing code).
   - Join `answer_bundles` by `run_id`. Raise `K1025 final_bundle_missing` if absent.
   - Return `jsonb_build_object` with: `run_id`, `answer_bundle_hash` (from `answer_bundles.answer_bundle_hash`), `final_output_hash`, `markdown` (from `answer_bundle->>'rendered_markdown'`), `usage` (from `answer_bundles.usage`), `evidence_ledger_hash` (from `answer_bundle->>'evidence_ledger_hash'`), `memory_revision` (from `runs.terminal_outcome` if available, else null), `memory_frontier_hash` (from `answer_bundle->>'memory_frontier_hash'` if present, else null — check if this field exists in the bundle; if not, return null).
   - Header comment: "Host-only final projection. Exposes markdown, usage counters, and ledger hashes for product projection. Does NOT expose evidence bodies, provider episodes, or MCP results. Ownership is verified against tenant+principal+session+run (defense-in-depth matching commit_final)."
   - Use `CREATE OR REPLACE FUNCTION` — it's a new function, so plain `CREATE FUNCTION` is fine.
2. In `packages/host-ts/src/contracts.ts`, add `ReadFinalProjectionResponse` interface matching the SQL return fields. abi_version: 1.
3. In `packages/host-ts/src/client.ts`, add `readFinalProjection(ownership)` method mirroring `readFinalOutput` but calling `agent_v1.read_final_projection`. Add `parseFinalProjectionResponse` with strict `exactObject` field list matching the SQL return. Mark the old `readFinalOutput` JSDoc `@deprecated use readFinalProjection`.
4. In `packages/host-ts/src/postgres.ts`, add prepared statement name `"krw_host_agent_v1_read_final_projection"`.
5. Document error codes `K1027`, `K1028` in the contracts/host docs if an error code registry exists (grep for `K1017` to find it).

**Test:** `tsc` clean; add a host-ts unit test for the parser accepting valid input and rejecting unknown fields. For SQL, the migration test from Task 1 covers "applies cleanly." A full ownership-rejection test requires a live DB — if runtime-persistence live tests exist, add a case there; otherwise document as needing live verification.

---

### Task 3: parallel_safe deprecation documentation

**Files:**
- `crates/agent-image/src/lib.rs` (modify — doc comment on `parallel_safe` field)
- `docs/EMBEDDED_AGENT_ARCHITECTURE.md` (modify — add deprecation note; grep first to find the right doc file)

**Spec:**
1. In `crates/agent-image/src/lib.rs` around line 768, change the doc comment on `parallel_safe: bool` to:
   ```rust
   /// Declares a capability eligible for in-run parallel dispatch. Currently
   /// **not enforced** by the run engine or capability runtime — all
   /// capabilities execute sequentially regardless of this flag. Retained in
   /// the ABI for forward compatibility; do not author `parallel_safe: true`
   /// expecting concurrent execution today. Wiring this flag to real
   /// parallel dispatch requires careful handling of ordering, budget, and
   /// MCP-tail semantics (see architecture review).
   pub parallel_safe: bool,
   ```
   Do NOT add `#[deprecated]` attribute (breaks serde / image hash). Doc-only.
2. Find the architecture doc that mentions `parallel_safe` (grep `parallel_safe` in `docs/`). Add a section: "### parallel_safe field — currently advisory only" explaining it's parsed and hashed into the image but not enforced at runtime. Cite the architecture review's warning about parallel dispatch risks (ordering, budget, MCP tail).
3. Do NOT change any agent.yaml files. The one `parallel_safe: true` at `agents/krw-ontology/agent.yaml:448` stays — it's forward-compatible.

**Test:** `cargo clippy -p krw-agent-agent-image -- -D warnings` clean. Doc build clean.

---

### Task 4: Concurrency defaults + instrumentation (Speed 6)

**Files:**
- `bins/krw-agentd/src/main.rs` (modify — worker_threads CLI, deepseek idle CLI, runtime builder)
- `crates/deepseek-wire/src/lib.rs` (modify — `production()` takes `max_idle_per_host`)
- `crates/protocol/src/lib.rs` (modify — add duration fields to `BudgetUsage`)
- `crates/run-engine/src/lib.rs` (modify — measure durations, populate `BudgetUsage`)
- `crates/persistence/src/metrics.rs` (modify — per-capability duration histogram, per-turn TTFT histogram)
- `packages/host-ts/src/atomic-final.ts` (modify — `BillingUsageV1` duration fields)

**Spec:**
1. In `bins/krw-agentd/src/main.rs`:
   - Remove `#[tokio::main(worker_threads = 2)]`. Replace `async fn main()` with a `fn main()` that builds a `tokio::runtime::Builder::new_multi_thread().worker_threads(args.worker_threads).enable_all().build()?.block_on(async_main(args))`. Add CLI arg `#[arg(long, default_value_t = default_worker_threads())] worker_threads: usize` where `default_worker_threads()` returns `std::thread::available_parallelism().map(|n| n.get()).unwrap_or(8).min(8)`. Keep the existing `MAX_ACTIVE_RUNS=16` cap validation.
   - Add CLI arg `#[arg(long, default_value_t = 8)] deepseek_max_idle_per_host: usize`. Thread it through to `DeepSeekClientConfig` construction (follow how `DeepSeekProviderCatalog::compile_release_set` is called around line 203 — find the config construction site).
2. In `crates/deepseek-wire/src/lib.rs`, change `DeepSeekClientConfig::production(api_base, allowed_models)` to `production(api_base, allowed_models, max_idle_per_host)`. Update all callers (grep `production(`). Default param at call sites that don't have a value: pass 8.
3. In `crates/protocol/src/lib.rs`, add to `BudgetUsage` (keep `deny_unknown_fields` satisfied — serde default):
   ```rust
   #[serde(default)]
   pub provider_total_ms: u64,
   #[serde(default)]
   pub capability_total_ms: u64,
   #[serde(default)]
   pub compact_total_ms: u64,
   ```
   These default to 0 so existing persisted bundles (without these fields) still deserialize.
4. In `crates/run-engine/src/lib.rs`, wrap the provider call site, capability call site, and compaction call site with `let _t0 = Instant::now();` ... `let dt = _t0.elapsed().as_millis() as u64;` and accumulate into `self.usage.provider_total_ms` / `capability_total_ms` / `compact_total_ms`. Find the provider call (grep for the `Provider::generate` or episode execution), the capability call (`call_tool`), and compaction (`compact_settled_phase`). Add fields to `ActiveRun` if usage is stored there.
5. In `crates/persistence/src/metrics.rs`, add two histograms: `krw_capability_duration_seconds{capability_id,outcome}` and `krw_provider_turn_duration_seconds`. Observe them at the same sites the counters are observed (find the existing `krw_capability_calls_total` observation site).
6. In `packages/host-ts/src/atomic-final.ts`, add `provider_total_ms`, `capability_total_ms`, `compact_total_ms` to `BillingUsageV1` (lines 107-115) as optional fields.

**Test:** `cargo test -p krw-agent-protocol` (BudgetUsage serde roundtrip with and without new fields — old bundles without fields must deserialize with 0). `cargo test -p krw-agent-run-engine` (duration accumulation smoke test if feasible). `tsc` clean.

---

### Task 5: MCP attested-stateless-v1 activation (Speed 2)

**Files:**
- `deployments/local/deployment-binding.example.yaml` (modify)
- `deployments/prod/deployment-binding.krw-ontology.example.yaml` (modify)
- `deployments/prod/deployment-binding.example.yaml` (modify)
- `crates/tool-mcp/src/lib.rs` (modify — integration test for pool reuse)
- `docs/MCP_SESSION_ISOLATION.md` (modify or create — check existence first)

**Spec:**
1. In the three deployment binding YAMLs, change `tool_session_reuse: run-scoped` to `tool_session_reuse: attested-stateless-v1` ONLY for read-only ontology capabilities (those with `permission: read` — verify by grepping the capability spec in agent.yaml). Specifically: `krw_ontology_query_context`, `krw_ontology_query`, `krw_ontology_trace`, `krw_ontology_chain`, `krw_skill_local`, `krw_guru_query_context`, `krw_guru_company_brief`, `krw_guru_review_company_evidence`. LEAVE all `krw_feed_*`, `krw_filings_*` / filing capabilities, and any write-capability as `run-scoped`. The `auth_scope` stays `tenant` for these.
2. Add an integration test in `crates/tool-mcp/src/lib.rs` test module: construct two `McpHttpPoolKey`s from the same binding with `attested-stateless-v1`, assert they produce the same `pool_key` hash (pool reuse eligible). Then construct one with `run-scoped` and assert it produces a different (Run-scoped) key. This verifies the pooling eligibility without needing a live server.
3. Find or create `docs/MCP_SESSION_ISOLATION.md`. Document: which capabilities are `attested-stateless-v1` vs `run-scoped`, the attestation contract (`STATELESS_TOOL_SESSION_CONTRACT_ID`), and the requirement that `krw-capabilityd` MUST emit attestation in both readiness and initialize results (else fail-closed). Note: activating this in production requires capabilityd to support attestation — the agent-side code is ready; capabilityd readiness is a deployment dependency.

**Test:** `cargo test -p krw-agent-tool-mcp` (new pool-key test passes). `cargo test -p krw-agent-runtime-config` (binding schema validation passes). Manual: `cargo run --bin krw-agentd -- --check` against the modified binding should pass structurally (attestation is verified at runtime, not check-time).

---

### Task 6: Role-specific compaction view (Speed 4)

**Files:**
- `crates/context-compaction/src/lib.rs` (modify — add `view_for_role`)
- `crates/run-engine/src/lib.rs` (modify — use role view in `build_trusted_messages`)

**Spec:**
1. In `crates/context-compaction/src/lib.rs`, add method on `CompactedProviderContext`:
   ```rust
   pub fn view_for_role(&self, role_id: &str) -> CompactedContextView
   ```
   Returns a struct with `canonical: String` (the role-filtered context text) and `byte_len: u64`. Filtering rules:
   - For `composer`: include `retained_facts` (all), `calculations` (all), citation handles from `evidence_index`, omit `research_projection.conflicts` and `research_projection.unresolved_goals`.
   - For `analyst`: include `research_projection.unresolved_goals`, `research_projection.conflicts`, top evidence by grade, omit detailed `calculations`.
   - For `repair`: include only the validator-defect-relevant slice (minimal — just `state` artifact and the relevant fact subset). Since repair context is small anyway, return a trimmed view.
   - For `planner` (and any other role): return the full canonical (current behavior).
   The full `canonical` field on `CompactedProviderContext` is unchanged (durable). The view is a runtime projection.
2. In `crates/run-engine/src/lib.rs` `build_trusted_messages` around line 7593-7598, replace the unconditional read of `state.compacted_context.canonical` with `state.compacted_context.view_for_role(role_id).canonical` when a compacted context exists. The role_id is already in scope (line 7530).
3. Add a `CompactedContextView` struct. Receipt hash unchanged (computed over full context, not view).

**Test:** `cargo test -p krw-agent-context-compaction` — add test that `view_for_role("composer")` differs in byte length from `view_for_role("analyst")` for a fixture with both facts and conflicts. `cargo test -p krw-agent-run-engine` — existing tests must pass (receipt hash invariant).

---

### Task 7: Prompt emission cache optimization (Speed 3)

**Files:**
- `crates/run-engine/src/lib.rs` (modify — `build_trusted_messages` stable prefix separation)

**Spec:**
1. In `build_trusted_messages` (line 7521+), split the system message into two when stable-prefix segments exist:
   - **System message 1 (cacheable prefix):** the fixed preamble (line 7543-7545) + all `static_segments` where `segment.stable_prefix == true` (line 7546-7560 loop, filtered). This must be byte-identical across calls with the same image+role.
   - **System message 2 (dynamic):** the `<kernel-state-contract>` (7561-7576), `model_output_instruction` (7577), `<trusted-run-scope>` (7579-7587).
   - If no stable-prefix segments exist, keep single system message (backward compatible).
2. Provider message order becomes: `[system_stable, system_dynamic, user, ...transcript]`. Update `TRUSTED_PREFIX_MESSAGE_COUNT` constant accordingly (find it — grep).
3. The receipt hash computation (line 7667-7673) must still hash the logical concatenation — verify the `context.receipt(...)` and `verify_receipt(...)` still pass. The receipt is over content hashes, not message boundaries, so splitting should be safe. Verify in tests.

**Test:** `cargo test -p krw-agent-run-engine` — all existing prompt/receipt tests pass. Add a test asserting that for the same image+role, the stable-prefix system message bytes are identical across two calls with different dynamic scope.

---

### Task 8: Numeric grader scoped matching (eval fix)

**Files:**
- `scripts/grade_numeric_accuracy.py` (modify)
- `scripts/run_eval_v2.sh` (modify)
- `evals/README.md` (modify)

**Spec:**
1. In `scripts/grade_numeric_accuracy.py`, add function `extract_numbers_near(text: str, metric_keywords: list[str], fiscal_year: str) -> list[float]`:
   - Split text into sentences (split on `. ` / `다.` / `\n`).
   - For each sentence, check if ANY metric_keyword appears (case-insensitive substring) AND fiscal_year appears. Metric keywords derived from the `metric` field (e.g., "revenue" → ["revenue", "매출", "sales"]; build a keyword map).
   - Return numbers from matching sentences only.
   - If no sentences match, return empty list (→ grader reports "metric context not found", NOT a pass).
2. Modify `check_metric` (line 134-163): for absolute values, call `extract_numbers_near` with the metric's keywords and fiscal_year. If the scoped list is empty, the check fails with reason `"metric_context_not_found"`. Otherwise pick closest from scoped list. Percent branch: same scoping.
3. Add a `metric_keyword_map` dict mapping metric identifiers to keyword lists. Cover common metrics: revenue, operating_income, net_income, eps, fcf, market_cap, debt, cash, margin (percent). Korean + English keywords.
4. In `scripts/run_eval_v2.sh` (line 63), change PASS decision:
   - Keep `state=final` check as prerequisite.
   - After `state=final`, run the numeric grader. If `--question-id` has expected metrics and any fails, set `STATUS="FAIL"` with reason `numeric_mismatch`. If no expected metrics exist for the qid, PASS on `state=final` alone.
   - The grader's exit code (0 = all pass, 1 = some fail) now gates PASS.
5. Update `evals/README.md` to document the new scoped-matching semantics and the PASS-on-numeric-failure behavior.

**Test:** Add Python unit tests (pytest or unittest) in `scripts/test_grade_numeric.py`:
- Answer with correct 2024 revenue → pass.
- Answer with 2023 revenue (wrong year) and 2024 revenue absent → fail (context not found for 2024).
- Answer with 2024 revenue + 2024 unrelated metric nearby → picks the revenue-context number.
- Percent metric in a paragraph without the metric keyword → fail.

---

### Task 9: company_research 50-case eval set (eval strengthening)

**Files:**
- `evals/numeric-accuracy/ground_truth.json` (modify — expand to 50+10 questions)
- `scripts/run_eval_v2.sh` (modify — support expanded qid set)

**Spec:**
1. Expand `evals/numeric-accuracy/ground_truth.json` from 19 to 60 questions. Add 41 new questions across 10 categories (5 each) + 10 attack cases:
   - **단일 숫자 (q20-q24):** single-metric (revenue, operating income, etc.) for AAPL/MSFT/NVDA/GOOGL/TSLA FY2024.
   - **여러 기간 비교 (q25-q29):** "X의 2023 vs 2024 변화" — expected_metrics has 2 entries (both years).
   - **segment 비교 (q30-q34):** segment-level (iPhone vs Services, AWS vs ads, etc.).
   - **causal mechanism (q35-q39):** "왜 마진이 변했나" — NO numeric expected_metrics (PASS = state=final + no internal-term exposure; grader skips). Mark these with `expected_metrics: []`.
   - **사업구조 해석 (q40-q44):** "주요 매출 원천?" — `expected_metrics: []`.
   - **리스크 (q45-q49):** "주요 리스크?" — `expected_metrics: []`.
   - **자본배치 (q50-q54):** buyback/dividend amounts — numeric.
   - **충돌 공시 (q55-q59):** questions where filings might disagree — `expected_metrics: []`, graded on state=final only.
   - **근거 부족 (q60-q64):** questions that should yield "답 불가" — these are hard to grade numerically; mark `expected_metrics: []` and document as needing human review.
   - **잘못된 전제 (q65-q69):** false-premise questions — `expected_metrics: []`.
   - **공격형 (q70-q79):** 10 cases with numeric traps (wrong year number present, wrong metric present) to verify the scoped grader from Task 8 rejects them.
2. For numeric questions, source expected values from the existing `evals/numeric-accuracy/ground_truth.json` format. Values must be accurate — if uncertain, mark `expected_metrics: []` rather than guess. **Do NOT read `krw-ontology-data` for values (off-limits); use the existing 19 questions' values as reference and add new ones conservatively.**
3. Update `scripts/run_eval_v2.sh` `QUESTIONS` array (lines 16-36) to include the new qids.
4. Document the 50-case taxonomy in `evals/README.md`.

**Test:** `python3 scripts/grade_numeric_accuracy.py --validate evals/numeric-accuracy/ground_truth.json` (add a `--validate` mode if not present — checks JSON schema, no duplicate ids, expected_metrics well-formed). Run grader against one known answer to confirm scoped matching works.

---

## Sequencing

- **Task 1** (migration safety) → unblocks Tasks 2, 4 (which add migrations/fields).
- **Task 2** (read_final_projection) — independent after Task 1.
- **Task 3** (parallel_safe docs) — fully independent, do anytime.
- **Task 4** (concurrency + instrumentation) — independent after Task 1 (migration for usage fields — actually JSONB, may not need migration; verify in-task).
- **Task 5** (MCP session reuse) — independent.
- **Task 6** (role compaction view) — independent.
- **Task 7** (prompt cache) — independent but should come after Task 6 (both touch `build_trusted_messages`).
- **Task 8** (grader fix) → unblocks Task 9.
- **Task 9** (50-case eval) — after Task 8.

Parallelizable: Tasks 2, 3, 4, 5 can run concurrently after Task 1 (different files). Tasks 6→7 are sequential. Task 8→9 sequential.
