# Event-Premise Ladder Reminder (후보 2) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** When the committed research plan marks an event-premise goal and no event/news ladder capability has been dispatched, the kernel injects a deterministic `kernel_event_ladder_hint` note into the `ontology.query_context` result so the analyst cannot reach `evidence_sufficient` without seeing the ladder obligation.

**Architecture:** The planner model marks `qualitative_evidence` goals with an optional `event_premise: true` flag (ResearchProposal v4 ABI). The compiler carries the flag into `EvidenceGoal` in the durable `ResearchIntentReceipt`. The run engine's existing provider-visible-result wrapper (`model_visible_capability_result`, the same channel as `kernel_research_gap_hint`) renders the reminder when the receipt has a marked goal and the run's `capability_calls` shows no ladder rung yet. The analyst prompt documents the note's policy. r7 baseline: ladder firing 2/15 on event-premise cases (early-exit at 2 capability calls dominates); success bar for r8: ≥14/15 with control staying 0/3.

**Tech Stack:** Rust workspace (krw-contracts, research-planner, planning, run-engine), agent prompt/skill markdown under `agents/krw-ontology/`, local dev stack + GLM quality matrix for measurement.

## Global Constraints

- Local testing uses provider `glm` (`glm-5.3-flash`); production deploy is DeepSeek behind a separate user-approval gate — do not deploy.
- `scripts/dev-stack.sh reload` is allowed only while no quality matrix is in flight (r7 finished 2026-08-29; check `ps aux | grep run_live_quality_matrix` before reload).
- Mimosa blocks writing source files via Bash heredoc/redirect — use Edit/Write tools for all source edits.
- Never commit credentials or vendor identification strings (FMP/Yahoo 등) in code, logs, or commits.
- cargo invocations need `PATH=$HOME/.rustup/toolchains/1.97.1-aarch64-apple-darwin/bin:$PATH` prefix.
- DB access (if any) must use parameter binding, never string concatenation.
- Commit per task with per-path `git add` (repo uses conventional prefixes like `feat:`/`fix:`).
- Recovery compatibility: every newly serialized struct field gets `#[serde(default)]` so old durable checkpoints/receipts (field absent) still deserialize under `deny_unknown_fields`.
- The receipt is a privacy boundary: it must not retain the user question or retrieval text — only the boolean marking rides on opaque goal objects.

---

### Task 1: Contracts accept optional `event_premise` on qualitative goals

**Files:**
- Modify: `crates/krw-contracts/src/lib.rs:1056-1079` (qualitative_evidence validation arm), `:1262-1269` (shape-detail allowed-keys table)
- Test: `crates/krw-contracts/src/lib.rs` `mod tests` (starts `:1878`)

**Interfaces:**
- Produces: contract-level acceptance of `"event_premise": true|false` as an OPTIONAL boolean key on `qualitative_evidence` goals only (metric goals reject it via their existing exact key sets). Task 2's parser and Task 4's docs rely on this.

- [ ] **Step 1: Write the failing tests**

Inside the existing `mod tests`, find a currently-valid proposal fixture containing a `qualitative_evidence` goal (`grep -n "qualitative_evidence" crates/krw-contracts/src/lib.rs | awk -F: '$1 > 1878'`). Clone it into three tests:

```rust
#[test]
fn research_proposal_accepts_event_premise_true_on_qualitative_goal() {
    // <existing valid qualitative proposal fixture> with the goal mutated to:
    // "goal": {"kind":"qualitative_evidence","concepts":["executive change"],
    //          "predicates":["announced"],"event_premise":true}
    // assert the proposal validates (same assertion the existing fixture test uses).
}

#[test]
fn research_proposal_accepts_absent_event_premise_on_qualitative_goal() {
    // same fixture without the key — must still validate (backward compatibility).
}

#[test]
fn research_proposal_rejects_non_boolean_event_premise() {
    // same fixture with "event_premise":"yes" — must fail with the Shape error
    // the existing invalid-shape tests assert on.
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `PATH=$HOME/.rustup/toolchains/1.97.1-aarch64-apple-darwin/bin:$PATH cargo test -p krw-contracts event_premise 2>&1 | tail -20`
Expected: the `true`/`"yes"` tests FAIL (key rejected as unknown), the absent test PASSES.

- [ ] **Step 3: Implement**

In the `"qualitative_evidence"` arm (`:1056`), extend the allowed keys and add a boolean check:

```rust
"qualitative_evidence" => {
    exact_keys(
        goal,
        &["concepts", "event_premise", "kind", "predicates"],
        RESEARCH_PROPOSAL_V4,
    )?;
    // ... existing concepts/predicates checks unchanged ...
    if goal
        .get("event_premise")
        .is_some_and(|value| !value.is_boolean())
    {
        return Err(ContractValueError::Shape(RESEARCH_PROPOSAL_V4));
    }
    Ok(())
}
```

Note: `exact_keys` (`:726`) is an allowlist — keys in the list may be absent, so optionality is automatic. Mirror the same key in the shape-detail table (`:1267`):

```rust
"qualitative_evidence" => &["concepts", "event_premise", "kind", "predicates"],
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `PATH=$HOME/.rustup/toolchains/1.97.1-aarch64-apple-darwin/bin:$PATH cargo test -p krw-contracts 2>&1 | tail -5`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add crates/krw-contracts/src/lib.rs
git commit -m "feat(contracts): accept optional event_premise flag on qualitative goals"
```

---

### Task 2: Planner carries the marking into the durable intent graph

**Files:**
- Modify: `crates/planning/src/lib.rs:74-87` (`EvidenceGoal` struct)
- Modify: `crates/research-planner/src/initial_plan.rs:384-387` (`QualitativeEvidence` variant), `:236-244` (`IntentGoal`), `~566-585` (objective→`IntentGoal` build), `:1284-1294` (`EvidenceGoal` build in `index_and_validate_goals`)
- Test: `crates/research-planner/src/initial_plan.rs` `mod tests` (starts `:1741`)

**Interfaces:**
- Consumes: Task 1's contract acceptance.
- Produces: `EvidenceGoal.event_premise: bool` (pub field, `#[serde(default)]`), populated from `ResearchProposalGoal::QualitativeEvidence { event_premise }` for required objectives. Task 3's renderer reads it via `receipt.intent_graph.goals()`. Every other `EvidenceGoal { ... }` literal construction site in the workspace must be updated (the compiler enumerates them; set `event_premise: false` unless carrying intent).

- [ ] **Step 1: Write the failing tests**

In `mod tests` (`:1741`), find an existing test that compiles a proposal into `ResearchIntentCompilation` (grep `compile` / `ResearchIntentCompilation` under `mod tests`). Add:

```rust
#[test]
fn event_premise_marking_reaches_intent_graph() {
    // Take the existing valid proposal test input; mutate its required
    // qualitative objective's goal to include "event_premise": true.
    // Compile. Assert:
    //   compilation.receipt.intent_graph.goals()
    //     .any(|g| g.event_premise)
    // and that the marked goal's id appears in receipt.clause_goal_ids values.
}

#[test]
fn unmarked_proposal_leaves_event_premise_false() {
    // Same input without the key. Assert all graph goals have
    // event_premise == false.
}

#[test]
fn legacy_receipt_without_event_premise_field_still_recovers() {
    // Deserialize a receipt JSON string that has goals WITHOUT the
    // event_premise key (hand-written minimal JSON matching
    // ResearchIntentReceipt's shape). Assert validate_recovered() is Ok.
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `PATH=$HOME/.rustup/toolchains/1.97.1-aarch64-apple-darwin/bin:$PATH cargo test -p research-planner event_premise 2>&1 | tail -20`
Expected: FAIL (no such field on `QualitativeEvidence`/`EvidenceGoal`).

- [ ] **Step 3: Implement**

1. `crates/planning/src/lib.rs` `EvidenceGoal`:

```rust
    /// Planner-authored classification: this goal asserts or assumes a
    /// specific corporate event/announcement/report. Carried in the durable
    /// receipt so the run engine can surface the event-ladder obligation
    /// without retaining the user question.
    #[serde(default)]
    pub event_premise: bool,
```

2. `ResearchProposalGoal::QualitativeEvidence` variant gains `#[serde(default)] event_premise: bool,`.
3. `IntentGoal` gains `#[serde(default)] event_premise: bool,`.
4. In the required-objective loop (`~566`), before `lower_goal` consumes reads and `objective.alternatives` is moved:

```rust
let event_premise = matches!(
    &objective.goal,
    ResearchProposalGoal::QualitativeEvidence { event_premise: true, .. }
);
```

and pass `event_premise` into the `IntentGoal { ... }` literal.
5. In `index_and_validate_goals` (`:1284`), add `event_premise: goal.event_premise,` to the `EvidenceGoal` literal.
6. `cargo build` will enumerate remaining `EvidenceGoal {` construction sites (`grep -rn "EvidenceGoal {" crates/ --include="*.rs"`); set `event_premise: false` at literal sites that are not carrying intent.

Do NOT add `event_premise` to `ResearchObjectiveIdentity` — flipping the marking on a repaired proposal must not mint a new goal id.

- [ ] **Step 4: Run tests to verify they pass**

Run: `PATH=$HOME/.rustup/toolchains/1.97.1-aarch64-apple-darwin/bin:$PATH cargo test -p research-planner -p planning 2>&1 | tail -5`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add crates/planning/src/lib.rs crates/research-planner/src/initial_plan.rs
git commit -m "feat(planner): carry event_premise marking into the intent graph"
```

---

### Task 3: Kernel renders `kernel_event_ladder_hint` at the decision point

**Files:**
- Modify: `crates/run-engine/src/capability_dispatch.rs:215-258` (renderer), new const + helper near it
- Modify: `crates/run-engine/src/active_run.rs:2390-2397` (call site passes dispatch state)
- Modify: `crates/run-engine/src/lib.rs:73` (re-export helper for tests)
- Test: `crates/run-engine/src/lib.rs` `mod tests` (starts `:1709`; existing `model_research_gap_hint` tests at `:2135`, `:2186` are the pattern)

**Interfaces:**
- Consumes: Task 2's `EvidenceGoal.event_premise`.
- Produces:
  - `pub(crate) const EVENT_LADDER_CAPABILITY_IDS: [&str; 5]`
  - `pub(crate) fn model_event_ladder_hint(receipt: &ResearchIntentReceipt, ladder_dispatched: bool) -> Option<Value>`
  - `model_visible_capability_result(call, result, ladder_dispatched)` — signature gains one bool parameter.

- [ ] **Step 1: Write the failing tests**

In `mod tests`, mirror the `model_research_gap_hint` tests (`:2135`). Build a minimal `ResearchIntentReceipt` by hand (`EvidenceGoalGraph::new(vec![EvidenceGoal{...event_premise: true, ..}])`, `ContentHash::sha256(b"test")` for the hashes, one clause binding):

```rust
#[test]
fn event_ladder_hint_present_when_marked_and_undispatched() {
    let hint = model_event_ladder_hint(&receipt_with_marked_goal(), false).expect("hint");
    assert_eq!(hint["kind"], "event_premise_ladder_hint");
    assert_eq!(hint["marked_goal_ids"][0], "goal-test");
}

#[test]
fn event_ladder_hint_absent_when_ladder_dispatched() {
    assert!(model_event_ladder_hint(&receipt_with_marked_goal(), true).is_none());
}

#[test]
fn event_ladder_hint_absent_when_unmarked() {
    assert!(model_event_ladder_hint(&receipt_without_marked_goal(), false).is_none());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `PATH=$HOME/.rustup/toolchains/1.97.1-aarch64-apple-darwin/bin:$PATH cargo test -p run-engine event_ladder 2>&1 | tail -20`
Expected: FAIL (function undefined).

- [ ] **Step 3: Implement**

In `capability_dispatch.rs`:

```rust
/// Capability ids of the event-premise fallback ladder rungs (krw-ontology
/// workflow states event_filing_search..news_web_search). The reminder
/// clears itself once any rung has been dispatched.
pub(crate) const EVENT_LADDER_CAPABILITY_IDS: [&str; 5] = [
    "filing.search_events",
    "filing.event_brief",
    "news.feed_list",
    "news.feed_context",
    "news.web_search",
];

/// Kernel-owned note mirroring `model_research_gap_hint`: the committed plan
/// marks an event-premise goal and no ladder rung has run. Context only — it
/// does not force a tool choice and never carries question text.
pub(crate) fn model_event_ladder_hint(
    receipt: &ResearchIntentReceipt,
    ladder_dispatched: bool,
) -> Option<Value> {
    if ladder_dispatched {
        return None;
    }
    let marked: Vec<&str> = receipt
        .intent_graph
        .goals()
        .filter(|goal| goal.event_premise)
        .map(|goal| goal.goal_id.as_str())
        .take(12)
        .collect();
    if marked.is_empty() {
        return None;
    }
    Some(serde_json::json!({
        "schema_version": 1,
        "kind": "event_premise_ladder_hint",
        "marked_goal_ids": marked,
        "ladder_capabilities_dispatched": false,
        "note": "This run's committed plan marks an event-premise goal, and no filing-catalog or news-feed read has been dispatched. The filing event search is the direct evidence path for an event premise; ontology company facts cannot confirm the event.",
    }))
}
```

Change the renderer signature and query_context arm:

```rust
pub(crate) fn model_visible_capability_result(
    call: &PreparedCall,
    result: &CapabilityResult,
    ladder_dispatched: bool,
) -> Value {
    // ... unchanged until the query_context arm ...
    if call.capability.id == "ontology.query_context" {
        if let Some(hint) = model_research_gap_hint(&result.provider_content) {
            visible
                .as_object_mut()
                .expect("JSON object literal")
                .insert("kernel_research_gap_hint".into(), hint);
        }
        if let Some(hint) = model_event_ladder_hint(receipt, ladder_dispatched) {
            visible
                .as_object_mut()
                .expect("JSON object literal")
                .insert("kernel_event_ladder_hint".into(), hint);
        }
    }
    visible
}
```

`active_run.rs` call site (`:2395`):

```rust
let ladder_dispatched = self
    .capability_calls
    .keys()
    .any(|id| EVENT_LADDER_CAPABILITY_IDS.contains(&id.as_str()));
let content = model_visible_capability_result(call, result, ladder_dispatched);
```

Add `model_event_ladder_hint` to the `lib.rs:73` re-export list beside `model_research_gap_hint`. Fix any other `model_visible_capability_result(` callers the compiler finds.

- [ ] **Step 4: Run tests to verify they pass**

Run: `PATH=$HOME/.rustup/toolchains/1.97.1-aarch64-apple-darwin/bin:$PATH cargo test -p run-engine 2>&1 | tail -5`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add crates/run-engine/src/capability_dispatch.rs crates/run-engine/src/active_run.rs crates/run-engine/src/lib.rs
git commit -m "feat(run-engine): kernel event-premise ladder hint at the assessment decision point"
```

---

### Task 4: Teach the marking and the note policy

**Files:**
- Modify: `agents/krw-ontology/skills/research-planner/SKILL.md` (qualitative_evidence goal section — grep `qualitative_evidence`)
- Modify: `agents/krw-ontology/skills/research-planner/references/provider-proposal-contract.md` (goal schema table/example — grep `qualitative_evidence`)
- Modify: `agents/krw-ontology/prompts/evidence-analyst.md` (beside the existing `kernel_research_gap_hint` policy paragraph, `~:189-198` in the current file)
- Modify: `crates/agent-image/src/lib.rs:959-960` (ResearchProposalToSearchPlanV4 tool description)

**Interfaces:**
- Consumes: Tasks 1–3.
- Produces: planner emits the flag; analyst knows the note's policy. No code contract.

- [ ] **Step 1: SKILL.md marking rule** — in the goal-kind guidance, add:

> A `qualitative_evidence` goal whose premise is a specific corporate event, announcement, or report (최근 보도/발표/~가 사실인가요, a departure, deal, guidance change, post-results move) must set `"event_premise": true` on the goal. Omit the key (or `false`) for definitions, mechanisms, and ordinary business description. The flag is a classification of the claim, not proof — marking it does not change retrieval.

- [ ] **Step 2: provider-proposal-contract.md** — add `event_premise` (optional boolean, qualitative_evidence only) to the goal key table and one example snippet showing `"event_premise": true`.

- [ ] **Step 3: evidence-analyst.md note policy** — after the existing gap-hint paragraph, add:

> When an `ontology.query_context` result carries a `kernel_event_ladder_hint` note (kind `event_premise_ladder_hint`), the committed plan has marked an event-premise goal and no event/news ladder rung has run. Treat the event verification as open at the next `evidence_sufficient` decision: propose the filing event search edge in that assessment turn (or a later feed rung after the catalog returns nothing), the same way an exact candidate from a gap hint is chosen. The note is context, not a forced tool call — but answering an event-premise question as non-confirmed while it is present contradicts the checklist gate above.

- [ ] **Step 4: agent-image tool description** (`:960`) — extend the sentence listing tagged goals, e.g. after “qualitative_evidence”:

> ...; a qualitative goal whose premise is a specific corporate event or report sets `"event_premise": true`.

- [ ] **Step 5: Verify image build accepts the docs**

Run: `PATH=$HOME/.rustup/toolchains/1.97.1-aarch64-apple-darwin/bin:$PATH cargo build -p agent-image 2>&1 | tail -3`
Expected: compiles (tool description is a string literal).

- [ ] **Step 6: Commit**

```bash
git add agents/krw-ontology/skills/research-planner/SKILL.md agents/krw-ontology/skills/research-planner/references/provider-proposal-contract.md agents/krw-ontology/prompts/evidence-analyst.md crates/agent-image/src/lib.rs
git commit -m "feat(agents): teach event_premise marking and the kernel ladder-hint policy"
```

---

### Task 5: Regression, reload, and r8 measurement (gated)

**Files:** none (verification only)

**Interfaces:**
- Consumes: Tasks 1–4.

- [ ] **Step 1: Workspace regression**

Run: `PATH=$HOME/.rustup/toolchains/1.97.1-aarch64-apple-darwin/bin:$PATH cargo test --workspace 2>&1 | tail -10`
Expected: green. If a pre-existing failure is unrelated, record it and compare against `main`.

- [ ] **Step 2: agents/check-all.sh**

Run: `bash agents/check-all.sh 2>&1 | tail -10`
Expected: exit 0.

- [ ] **Step 3: Confirm no matrix in flight, then reload**

Run: `ps aux | grep run_live_quality_matrix | grep -v grep` (must be empty), then `PATH=$HOME/.rustup/toolchains/1.97.1-aarch64-apple-darwin/bin:$PATH scripts/dev-stack.sh reload 2>&1 | tail -5`
Expected: `dev stack ready: http://127.0.0.1:4318/healthz`. Verify the new pins differ from `77b158c6…` and `grep -rl "event_premise_ladder_hint" .local/agent-gateway/cache/<new-fingerprint>/images/` finds the compiled engine/prompt artifacts.

- [ ] **Step 4: r8 measurement (3 repeats × 6 cases)**

Same command shape as r7 (amat/dvn/qcom/shop/acn + ctrl_metric_no_ladder, `--parallelism 2 --provider glm --poll-seconds 10 --timeout-seconds 1500`, report dirs `/tmp/q-fallback/report-r8{a,b,c}`), sourcing `.local/agent-gateway/frontend.env` + `secrets.env`, exporting `KRW_AGENT_GATEWAY_URL=http://127.0.0.1:4318/v1/agent`. Never print env values.

- [ ] **Step 5: Verdict**

Tally `execution_trace.action_trace.capability_sequence` ladder prefixes as in r7. Bar: targets ≥14/15 (vs baseline 2/15) AND ctrl_metric_no_ladder 0/3. If the bar fails, the escalation is the verify_ir gate (후보 3), a separate plan.

---

## Self-Review

- Spec coverage: planner marking (Tasks 1–2), kernel deterministic reminder (Task 3), docs/policy (Task 4), measurement with the agreed bar (Task 5). Control-case over-firing check is inside Task 5 Step 5. ✔
- Placeholder scan: test bodies reference existing fixtures by grep-instruction rather than invented JSON (fixtures exist in all three test modules; the grep line numbers are given). No TBDs. ✔
- Type consistency: `event_premise: bool` used identically in contract JSON, `ResearchProposalGoal::QualitativeEvidence`, `IntentGoal`, `EvidenceGoal`; renderer name `model_event_ladder_hint` and note key `kernel_event_ladder_hint` consistent across Tasks 3–4. ✔
- Known limitation (accepted, matches the agreed design): a deferred (non-required) event objective does not carry the marking, and planner mislabeling suppresses the reminder — the analyst-prompt gate (후보 1) remains the second line.
