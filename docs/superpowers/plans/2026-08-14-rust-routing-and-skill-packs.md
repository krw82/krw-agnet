# Rust Routing and Skill Packs Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove Claude Agent SDK from the production research-chat path, route every question to the KRW Rust research harness without making routing a new failure point, and give each research mode a compact planning skill from its first provider turn while keeping only genuinely specialist material dynamically loadable.

**Architecture:** The browser-facing API owns authentication, session scope, admission, and a deterministic run-kind decision. It does not invoke Claude or wait for a separate routing run: explicit UI intent wins, high-confidence question forms map to a mode, and all remaining questions fall back to the existing company or wide-research workflow. Inside the immutable Rust image, common planning instructions remain role-scoped; each planner receives one small mode-specific planning pack, while analyst-only deep references remain local `skill.load` material discovered from a mode-curated catalog.

**Tech Stack:** Next.js/TypeScript, Supabase product queue, Rust `krw-agentd`, AgentImage v3, context planner, GLM live-quality lane.

## Global Constraints

- Modify only `~/krw-agnet` and `~/krw-ontology-front`.
- Do not modify `~/krw-ontology` or the ontology schema.
- Do not add a second research run, an automatic retry, or an extra LLM turn merely to choose a route.
- The normal safe fallback is `company_research` for a company room and `wide_research` for a tickerless global room; routing uncertainty must never block a user answer.
- Preserve an explicit UI-selected run kind, Guru lens, selected feed item, URL/attachment boundary, and authenticated ticker scope. Do not infer a ticker from user prose.
- Planner packs must be short operating instructions, not duplicated analyst/composer manuals. Each pack is loaded only in a planner role.
- `skill.load` remains a local immutable image lookup. It must not call MCP, the network, the filesystem, or the ontology.
- Dynamic skills may improve a deep analysis but must never be a prerequisite for reaching a final response. If a dynamic load is unavailable, continue with the role’s static core and the existing workflow.
- Do not implement structured cross-run handoff cards for scenario, idea-generation, or wide-research follow-ups in this change. That is a separate later project.
- Keep existing dirty-worktree changes intact; stage only files belonging to each completed task.
- Live answer-quality calls use GLM only. DeepSeek is checked through build/release and deterministic contract tests unless separately approved.

---

## Locked Decisions

1. The web app will not replace Claude routing with another frontend LLM call. A separate classifier call adds latency and can fail before research starts.
2. The existing `agents/krw-router` image stays a valid, tested internal contract, but is **not** put in front of every chat request in this release. It is reserved for a future daemon-owned compound run only after that compound-run lifecycle exists.
3. `EntrypointSpec.pinned_skills` is not used for planner packs: it force-loads a prompt in every model state, including the composer. Required base skills therefore belong in their owning `RoleSpec.prompt_segments`.
4. Earnings and scenario are the first specialist modes. Idea and wide get the same shape in the first implementation so the architecture does not fork later.
5. Every role gets its existing, validated base skill automatically. The model must not have to remember a `skill.load` call to obtain its base planning or analysis method.
6. Markdown files under `references/` are progressive-disclosure material: register them as `loadable: true`, give each a concrete `when_to_use`, and expose them only through the owning role's dynamic catalog. The composer is the sole exception because it cannot call tools; its three existing writing references remain static role segments.

## Existing Skill Preservation Matrix

The existing long documents are the validated knowledge base. This project does
not delete, rewrite, or create abbreviated replacements for them.

| Existing document | Current size | Treatment in this plan | When the full body is used |
| --- | ---: | --- | --- |
| `skills/research-planner/SKILL.md` | 23,340 bytes | Keep unchanged as the common, static planner contract for every planner. | First planning turn of every research mode. |
| `prompts/evidence-analyst.md` | 8,795 bytes | Keep unchanged as the common, static analyst contract. | Every analyst turn. |
| `prompts/research-analysis.md` | 51,750 bytes | Keep unchanged as the full company-research master skill; attach it automatically to the ordinary `analyst` role. Do not copy its nine research paths into new small files. | Every ordinary-company analyst turn, without a model `skill.load` call. |
| `prompts/earnings-analysis.md` | 11,306 bytes | Keep unchanged and static on `earnings_analyst`. | Every earnings analyst turn. |
| `prompts/scenario-analysis.md` | 9,866 bytes | Keep unchanged and static on `scenario_analyst`. | Every scenario analyst turn. |
| `prompts/idea-screen.md` and `prompts/wide-research-analysis.md` | mode-specific | Keep unchanged and static on their owning analyst roles. | Every idea/wide analyst turn. |
| Existing earnings/scenario/idea reference policies | mode-specific | Keep unchanged and loadable; show only to the matching analyst role. | Only after current evidence raises the policy's stated trigger. |

The five new `*-mode-planning.md` files are deliberately small **navigation
layers**. They decide the ResearchProposal shape and name the relevant dynamic
reference category; they never restate the detailed analysis, citation, style,
or stop rules from the master documents. The long base skills are not split or
weakened in this project.

## Common Reference Review and Target Ownership

The current image registers many common references but leaves most as
`loadable: false`. This is a real mismatch: `research-analysis.md` explicitly
instructs the analyst to consult period, tool, trace/chain, evidence-synthesis,
financial-statement, and investment-decision references, but the current
runtime cannot expose many of those bodies through `skill.load`.

| Reference category | Files | Dynamic owner and concrete trigger |
| --- | --- | --- |
| Planner repair support | `provider-proposal-contract.md`, `research-proposal-examples.md`, `recovery-loop.md` | Planner only; proposal repair, ambiguous goal type, or provider correction. |
| Retrieval/state interpretation | `research-query-context-contract.md`, `research-pack-rendering.md`, `research-tool-policy.md`, `research-ontology-layer-map.md`, `research-ontology-schema-reference.md` | Analyst only; unfamiliar ResearchState field, exact object-type choice, or an evidence gap needing a targeted/trace action. |
| Period/numeric interpretation | `research-period-and-latest-policy.md`, `research-financial-statement-interpretation.md`, `research-trace-chain-policy.md` | Analyst only; current/latest claim, CY/FY/quarter comparison, cash-flow/P&L interpretation, metric lineage, or causal-chain expansion. |
| Evidence-to-insight discipline | `research-evidence-to-analyst-synthesis.md`, `research-grounded-investment-inference.md`, `research-investment-decision-questions.md` | Analyst only; evidence is partial/conflicting, a conclusion has investment implications, or the user asks buy/hold/sell-style language. |
| Runtime/answer guardrails | `research-web-chat-runtime.md`, `research-bounded-autonomy-and-stop-rules.md`, `research-forbidden-user-facing-language.md` | Analyst only; filing-anchor ambiguity, bounded recovery, or an answer must distinguish unavailable evidence from company non-disclosure. |
| Future handoff/evaluation | `research-artifact-contract.md`, `research-structured-handoff-contract.md`, `research-research-synthesis-contract.md`, `research-evaluation-gates.md` | Registered as loadable but omitted from current mode catalogs until the separately deferred handoff project begins. |
| Writer core | `research-synthesis.md`, `plain-korean-investor-language.md`, `final-markdown-contract.md` | Remain static on composer because composer has no tools and must always finish an answer. |

The implementation updates misleading generic frontmatter such as `when_to_use:
"when the relevant analysis context arises"` to the concrete triggers above.
That makes `when_to_use` executable guidance for the model rather than a label
that asks it to guess.

## Run-Kind Decision Table

| Priority | Input condition | Chosen run kind | Why it is safe |
| --- | --- | --- | --- |
| 1 | Explicit supported UI mode, Guru lens, selected feed item, or URL/attachment boundary | Preserve the submitted mode | The product already made a deliberate choice. |
| 2 | Company room + clear current price movement question | `market_move_research` | Needs time-sensitive/news path. |
| 3 | Clear news discovery request | `news_discovery` or `news_discovery_wide` by scope | Discovery precedes company filing analysis. |
| 4 | Explicit candidate discovery/screening | `idea_generation` | User asks for candidates, not a sector explanation. |
| 5 | Company room + explicit scenario, sensitivity, or thesis-break condition | `scenario_sensitivity` | It needs forward-condition analysis. |
| 6 | Company room + explicit earnings, results, call, guidance, or quarter reconciliation request | `earnings_deep_dive` | It needs period/reconciliation planning. |
| 7 | Tickerless sector, cohort, transmission-path, or macro impact question | `wide_research` | Scope is the covered universe. |
| 8 | All other company questions | `company_research` | The company planner can broaden a short question into evidence plus one relevant insight. |
| 9 | All other tickerless questions | `wide_research` | Never invent a company ticker. |

## File Map

### `~/krw-ontology-front`

- Modify: `src/lib/question-router/chat-question-router.ts` — make routing pure TypeScript; remove all Claude SDK imports, SDK diagnostics, sessions, and streaming code.
- Modify: `src/lib/question-router/chat-question-router.test.ts` — test the priority table without mocking an SDK.
- Modify: `src/app/api/chat/run/route.ts` — invoke the pure route resolver once, preserve explicit intent, and enqueue the selected Rust run kind/context.
- Modify: `src/app/api/chat/run/route.test.ts` or the existing route test module — assert the actual enqueue payload for company, earnings, scenario, idea, wide, news, and fallback cases.
- Modify: `src/lib/agent/job-runner.ts` — remove its auto-routing invocation from retired Claude-worker execution; it must not become a second routing authority.
- Modify: `src/lib/agent/runner.ts` and its direct consumers — retire/replace only after every remaining user-facing research operation has a Rust run-kind mapping.
- Modify: `package.json`, `package-lock.json`, `scripts/verify-sdk-freeze.mjs`, `scripts/deploy-plugin-worker-production.sh`, `scripts/init-mac-worker-env.sh`, and retired plugin-worker scripts — remove Claude SDK-specific install, verification, and deployment assumptions after the import inventory is empty.

### `~/krw-agnet`

- Modify: `agents/krw-ontology/agent.yaml` — register mode planning packs, role-specific skill catalogs, and role prompt-segment assignments.
- Create: `agents/krw-ontology/skills/research-planner/modes/company-mode-planning.md` — compact company planning rules for direct answer plus investor insight.
- Create: `agents/krw-ontology/skills/research-planner/modes/earnings-mode-planning.md` — compact period/reconciliation/guidance planning rules.
- Create: `agents/krw-ontology/skills/research-planner/modes/scenario-mode-planning.md` — compact condition, base/up/down, and falsification planning rules.
- Create: `agents/krw-ontology/skills/research-planner/modes/idea-mode-planning.md` — compact candidate-discovery and rejection planning rules.
- Create: `agents/krw-ontology/skills/research-planner/modes/wide-mode-planning.md` — compact cohort/transmission-path planning rules.
- Create: `agents/krw-ontology/prompts/company-skill-catalog.md`, `earnings-skill-catalog.md`, `scenario-skill-catalog.md`, `idea-skill-catalog.md`, `wide-skill-catalog.md` only if static per-role catalog text proves simpler than compiler-generated filtered catalogs. Do not keep both approaches.
- Modify: `crates/agent-image/src/lib.rs` — let an image-defined skill catalog select an explicit subset of registered loadable skill IDs and reject invalid/duplicate subset declarations at image build time.
- Modify: `crates/context-planner/src/lib.rs` — retain tests proving planning packs appear only in their planner states and role-specific catalog bytes are part of the immutable prompt receipt.
- Modify: `crates/run-engine/src/lib.rs` — make repeated parent `skill.load` requests replay-safe within the visible transcript, without changing capability/MCP budgets or turning a duplicate load into a terminal error.
- Modify: `agents/krw-router/prompts/router.md` and `crates/krw-contracts/src/product.rs` only if their route enum/test fixture must be aligned with the final frontend decision table. Do not wire the router image into live chat in this project.

---

### Task 1: Establish One SDK-Free Routing Authority for New Research Chat

**Files:**

- Modify: `~/krw-ontology-front/src/lib/question-router/chat-question-router.ts`
- Modify: `~/krw-ontology-front/src/lib/question-router/chat-question-router.test.ts`
- Modify: `~/krw-ontology-front/src/app/api/chat/run/route.ts`
- Test: `~/krw-ontology-front/src/app/api/chat/run/route.test.ts` (or its existing equivalent)

**Interfaces:**

- Replace SDK-dependent route input with an explicit-vs-default marker:

```ts
export interface ChatQuestionRouteInput {
  content: string;
  requestedRunKind: RunKind;
  hasExplicitRunKind: boolean;
  currentAnalysisMode: AnalysisMode;
  defaultTickers: string[];
  scopeType: "global" | "company";
  hasUserUrls: boolean;
}

export interface ChatQuestionRouteDecision {
  analysisMode: AnalysisMode;
  runKind: RunKind;
  source: "preserve" | "deterministic" | "safe_default";
  reasonCode:
    | "preserved_explicit_route"
    | "market_price_move"
    | "current_news_discovery"
    | "candidate_discovery"
    | "scenario_question"
    | "earnings_question"
    | "sector_cohort_research"
    | "safe_company_default"
    | "safe_wide_default";
}
```

- The API route calls `resolveChatQuestionRoute` before budget reservation and uses only `decision.runKind` when it computes `rustRunKind`, product entitlement, analytics, and the Rust context.

- [ ] **Step 1: Write failing routing tests for the priority table**

Add table-driven cases for:

```ts
const cases = [
  ["CAT 실적과 가이던스가 바뀐 이유", companyInput, "earnings_deep_dive"],
  ["AAPL 상승·하락 시나리오와 판단이 바뀌는 조건", companyInput, "scenario_sensitivity"],
  ["AI 수혜주 후보를 찾아줘", globalInput, "idea_generation"],
  ["반도체 업종이 전력 부족의 영향을 어떻게 받나", globalInput, "wide_research"],
  ["CAT가 무슨 회사야?", companyInput, "company_research"],
];
for (const [content, base, expected] of cases) {
  expect(resolveChatQuestionRoute({ ...base, content })).toMatchObject({ runKind: expected });
}
```

Add an explicit-mode case proving an explicitly selected `guru_advisor` or `idea_generation` cannot be overwritten by question keywords. Add a company/tickerless fallback pair proving no classifier exception can block enqueue.

- [ ] **Step 2: Run the router test before implementation**

Run:

```bash
cd ~/krw-ontology-front
npm exec vitest run src/lib/question-router/chat-question-router.test.ts
```

Expected: the new earnings and SDK-free decision tests fail because the router imports and calls `@anthropic-ai/claude-agent-sdk` and has no `earnings_question` path.

- [ ] **Step 3: Replace `routeWithClaude` with pure, total routing**

Delete `resolveClaudeAgentCwd`, `buildClaudeAgentEnv`, `getClaudeAgentModel`, `ROUTER_SCHEMA`, `RouterDiagnosticError`, `shouldDisableClaudeRouter`, and `routeWithClaude` from the router module.

Implement this total final branch instead of throwing or returning `null`:

```ts
function safeDefault(input: ChatQuestionRouteInput): ChatQuestionRouteDecision {
  const company = input.scopeType === "company" || input.defaultTickers.length > 0;
  return company
    ? decision("company", "company_research", "safe_default", "safe_company_default")
    : decision("company", "wide_research", "safe_default", "safe_wide_default");
}
```

Use the priority table above exactly. A question merely containing a filing word remains company research unless it contains a specialist earnings/scenario/candidate/news signal. Do not add a broad keyword that makes ordinary company questions leave the company workflow.

- [ ] **Step 4: Wire the API route to the single resolver**

In `src/app/api/chat/run/route.ts`, derive:

```ts
const hasExplicitRunKind = body.run_kind !== undefined;
const route = resolveChatQuestionRoute({
  content,
  requestedRunKind,
  hasExplicitRunKind,
  currentAnalysisMode: effectiveAnalysisMode,
  defaultTickers: session.defaultTickers,
  scopeType: session.scopeType,
  hasUserUrls: userUrls.length > 0,
});
```

Use `route.runKind` consistently for the support check, entitlement selection, credit reservation, analytics, `toRustRunKind`, and `rustContext`. Keep URL/attachment rejection ahead of the resolver until those routes have a Rust mapping. Do not reintroduce `routingMode: "auto"` as an asynchronous worker concern.

- [ ] **Step 5: Add API enqueue-payload tests**

Mock only `getRustAgentRuntime().enqueue` and assert exact payloads:

```ts
expect(enqueue).toHaveBeenCalledWith(expect.objectContaining({
  runKind: "earnings_deep_dive",
  context: { kind: "company_ticker_set", tickers: ["CAT"] },
}));
```

Also assert a global sector question enqueues `wide_research` with `covered_universe`, and a normal company question still enqueues `company_research` with its authenticated room ticker.

- [ ] **Step 6: Run focused frontend verification**

Run:

```bash
cd ~/krw-ontology-front
npm exec vitest run \
  src/lib/question-router/chat-question-router.test.ts \
  src/app/api/chat/run/route.test.ts
```

Expected: all routing cases pass without any mock or import of `@anthropic-ai/claude-agent-sdk`.

- [ ] **Step 7: Commit the routing authority change**

```bash
git -C ~/krw-ontology-front add \
  src/lib/question-router/chat-question-router.ts \
  src/lib/question-router/chat-question-router.test.ts \
  src/app/api/chat/run/route.ts \
  src/app/api/chat/run/route.test.ts
git -C ~/krw-ontology-front commit -m "refactor: route research chat without Claude SDK"
```

### Task 2: Retire Claude SDK Research Execution from the Frontend

**Files:**

- Modify: `~/krw-ontology-front/src/lib/agent/job-runner.ts`
- Modify: `~/krw-ontology-front/src/lib/agent/runner.ts`
- Modify: `~/krw-ontology-front/src/lib/watchlist/notebook-refresh.ts`
- Modify: `~/krw-ontology-front/src/lib/visualizations/create-visualization-tool.ts`
- Modify: relevant consumer tests listed by the import inventory
- Modify: `~/krw-ontology-front/package.json`
- Modify: `~/krw-ontology-front/package-lock.json`
- Delete or modify: `~/krw-ontology-front/scripts/verify-sdk-freeze.mjs`
- Modify: `~/krw-ontology-front/scripts/deploy-plugin-worker-production.sh`
- Modify: `~/krw-ontology-front/scripts/init-mac-worker-env.sh`

**Interfaces:**

- All new research initiations use the existing `RustAgentRuntime.enqueue` API from `src/lib/agent-v1/runtime.ts`.
- `agent_backend = 'rust_agent'` remains the sole backend for new `chat_sessions`.
- No source file under `src/` imports `@anthropic-ai/claude-agent-sdk` after this task.

- [ ] **Step 1: Make the import inventory a failing test**

Create or extend a Vitest/Node test that scans `src/` and fails on the exact package name:

```ts
expect(sourceFiles.join("\n")).not.toContain("@anthropic-ai/claude-agent-sdk");
```

The test must exclude `node_modules`, `.next`, archived test fixtures, and generated coverage files, but must include TypeScript source and test files. It should print every offending relative filename.

- [ ] **Step 2: Run the inventory test and record each migration**

Run:

```bash
cd ~/krw-ontology-front
npm exec vitest run src/lib/agent/sdk-freeze.test.ts
rg -n '@anthropic-ai/claude-agent-sdk' src scripts package.json
```

Expected: the current chat router, legacy runner/job runner, notebook refresh, visualization helper, tests, and SDK freeze/deployment scripts appear. Do not remove the dependency before each live caller has either been retired or mapped to an existing Rust run kind.

- [ ] **Step 3: Retire the legacy research worker rather than maintaining two executors**

Remove `resolveAutoRoutedJob` and any Claude-worker invocation from `job-runner.ts`. For every endpoint that still calls `runAgentJob`/`runner.ts`, replace the call with the established sequence:

```ts
const runtime = await getRustAgentRuntime();
await runtime.enqueue({ runId, requestId, userId, sessionId, userMessageId, runKind, locale, question, context });
```

Map notebook operations to the existing `research_notebook` run kind and answer-display transformations to the existing `answer_composition` run kind. If an endpoint has no existing Rust entrypoint, return its existing bounded "currently unavailable" response and remove it from product navigation; do not silently route it through a generic Claude runner.

- [ ] **Step 4: Remove SDK-only deployment and test plumbing**

After `rg` reports no application import:

1. remove the Claude dependency and lockfile subtree with `npm uninstall @anthropic-ai/claude-agent-sdk`;
2. replace `verify-sdk-freeze.mjs` with `verify-rust-agent-runtime.mjs`, checking the release descriptor, local frontend runtime env file in development, and daemon heartbeat rather than a Claude binary;
3. remove Claude executable and SDK environment setup from `init-mac-worker-env.sh`;
4. remove plugin-worker deployment/health checks that import the SDK;
5. delete abandoned SDK-only test mocks and fixtures.

- [ ] **Step 5: Verify no SDK reference and preserve the Rust queue path**

Run:

```bash
cd ~/krw-ontology-front
rg -n '@anthropic-ai/claude-agent-sdk' src scripts package.json package-lock.json
npm exec vitest run src/lib/agent-v1 src/lib/question-router src/app/api/chat/run
npm run prod:check
```

Expected: `rg` exits 1 (no matches), focused tests pass, and `prod:check` validates the Rust release/host contract without seeking a Claude plugin worker.

- [ ] **Step 6: Commit the SDK retirement separately**

```bash
git -C ~/krw-ontology-front add src scripts package.json package-lock.json
git -C ~/krw-ontology-front commit -m "refactor: retire frontend Claude SDK research worker"
```

### Task 3: Automatically Load Existing Base Skills and Add Compact Planning Navigation

**Files:**

- Create: `~/krw-agnet/agents/krw-ontology/skills/research-planner/modes/company-mode-planning.md`
- Create: `~/krw-agnet/agents/krw-ontology/skills/research-planner/modes/earnings-mode-planning.md`
- Create: `~/krw-agnet/agents/krw-ontology/skills/research-planner/modes/scenario-mode-planning.md`
- Create: `~/krw-agnet/agents/krw-ontology/skills/research-planner/modes/idea-mode-planning.md`
- Create: `~/krw-agnet/agents/krw-ontology/skills/research-planner/modes/wide-mode-planning.md`
- Modify: `~/krw-agnet/agents/krw-ontology/agent.yaml`
- Test: `~/krw-agnet/crates/context-planner/src/lib.rs`
- Test: `~/krw-agnet/crates/agent-image/src/lib.rs`

**Interfaces:**

- New prompt segment IDs:

```text
company_mode_planning
earnings_mode_planning
scenario_mode_planning
idea_mode_planning
wide_mode_planning
```

- Planner assignments:

```yaml
planner:          [ ..., research_planner_skill, company_mode_planning ]
earnings_planner: [ ..., research_planner_skill, earnings_mode_planning ]
scenario_planner: [ ..., research_planner_skill, scenario_mode_planning ]
idea_planner:     [ ..., research_planner_skill, research_scope, idea_mode_planning ]
wide_planner:     [ ..., research_planner_skill, research_scope, wide_mode_planning ]
```

- Required existing analysis assignments:

```yaml
analyst:           [ ..., evidence_analyst, research_analysis ]
earnings_analyst:  [ ..., evidence_analyst, earnings_analysis ]
scenario_analyst:  [ ..., evidence_analyst, scenario_analysis ]
idea_analyst:      [ ..., evidence_analyst, idea_screen ]
wide_analyst:      [ ..., evidence_analyst, wide_research_analysis ]
```

- [ ] **Step 1: Write failing context-plan tests before changing YAML**

Add a table asserting a planning state receives exactly one matching mode pack and a composer state receives none. Also prove the ordinary company analyst receives the existing full `research_analysis` body without first proposing `skill.load`:

```rust
assert!(planner_segments.contains("earnings_mode_planning"));
assert!(!planner_segments.contains("scenario_mode_planning"));
assert!(!composer_segments.contains("earnings_mode_planning"));
assert!(company_analyst_segments.contains("research_analysis"));
```

Repeat for company, scenario, idea, and wide entrypoints. Assert `LoadReason::RoleMatch`, not `LoadReason::PinnedSkill`.

- [ ] **Step 2: Run the focused context planner test**

Run:

```bash
cd ~/krw-agnet
cargo test -p krw-context-planner specialized_roles_keep_core_skills_and_offer_related_skill_loading
```

Expected: new pack assertions fail because the specialized planner roles currently receive only the general `research_planner_skill`.

- [ ] **Step 3: Write the five small planning packs**

Every file has YAML frontmatter with `name`, `description`, and `when_to_use`, then only the following decision rules:

| Pack | Required planning behavior |
| --- | --- |
| Company | Preserve the direct question; add one linked investor insight; company-introduction questions map business model, operating segments, value drivers, and one relevant watch item; explain a general concept plainly if no filing fact is needed. |
| Earnings | Separate reported result, period comparison, operational driver, guidance/commentary, and one thesis implication. Never treat FY and CY quarters as interchangeable. |
| Scenario | Define the decision variable and observable base/upside/downside conditions. Keep reported evidence distinct from forward inference and name the condition that would change the reading. |
| Idea | Define the screen, candidate inclusion evidence, and rejection condition. Candidate discovery is not buy/sell/hold advice. |
| Wide | Define the external change, transmission path, exposed cohort, counter-channel, and monitorable indicators. Do not turn a sector explanation into an unrequested stock screen. |

Do not copy analyst instruction, final Markdown language, tool names, or long examples from `research_analysis.md` into these packs.

- [ ] **Step 4: Register packs as static role segments**

Add each file to `agent.yaml` `prompt_segments` with `private: true`, `stable_prefix: true`, and no `loadable` flag. Attach it only to its planner role. Keep the original `research_planner_skill` first because it owns the ResearchProposal v4 contract.

Treat the long base documents as role-owned instructions, not dynamic skills: remove `loadable: true` from `research_planner_skill`, `evidence_analyst`, `research_analysis`, `earnings_analysis`, `scenario_analysis`, `idea_screen`, and `wide_research_analysis`. Add `research_analysis` to the ordinary `analyst` role. This makes the skill boundary unambiguous: base methods are automatically present; references are what `skill.load` retrieves.

Do not add these packs to `entrypoints.*.pinned_skills`: pinned skills are injected into orienter, analyst, and composer states and would waste context or blur role boundaries.

- [ ] **Step 5: Add image compilation coverage**

Extend the image test to compile the updated agent and assert each new planning prompt is registered as a non-loadable role segment. Assert the long base skills are absent from generated dynamic catalogs, while an explicitly registered reference remains available to local `skill.load`. The test must not expect the composer to include a planning pack.

- [ ] **Step 6: Run focused verification**

Run:

```bash
cd ~/krw-agnet
cargo test -p krw-agent-image skill_catalog_exposes_only_registered_loadable_ids
cargo test -p krw-context-planner specialized_roles_keep_core_skills_and_offer_related_skill_loading
```

Expected: planning roles receive their mode pack and analysts receive their existing long base skill at the first relevant provider turn; no `skill.load` call is required for baseline mode quality.

- [ ] **Step 7: Commit the planner packs**

```bash
git -C ~/krw-agnet add agents/krw-ontology/agent.yaml \
  agents/krw-ontology/skills/research-planner/modes/*-mode-planning.md \
  crates/context-planner/src/lib.rs crates/agent-image/src/lib.rs
git -C ~/krw-agnet commit -m "feat: bind planning skills to research modes"
```

### Task 4: Make Dynamic `skill.load` Relevant, Bounded, and Replay-Safe

**Files:**

- Modify: `~/krw-agnet/crates/agent-image/src/lib.rs`
- Modify: `~/krw-agnet/agents/krw-ontology/agent.yaml`
- Modify: `~/krw-agnet/crates/run-engine/src/lib.rs`
- Test: `~/krw-agnet/crates/agent-image/src/lib.rs`
- Test: `~/krw-agnet/crates/run-engine/src/lib.rs`

**Interfaces:**

- Extend generated catalog configuration without changing the public `skill.load/v1` input:

```rust
pub struct SkillCatalogSource {
    pub skills_dirs: Vec<String>,
    #[serde(default)]
    pub include_skill_ids: Vec<String>,
}
```

- Catalog mapping:

| Catalog | Dynamic choices shown to the role |
| --- | --- |
| `planner_reference_catalog` | `provider_proposal_contract`, `research_proposal_examples`, `research_recovery_loop`; used only when planning needs correction/examples, never for the base proposal method. |
| `company_skill_catalog` | Common analyst references for period/latest, financial-statement interpretation, tool/state interpretation, trace/chain, evidence-to-insight, and investment-decision questions. It does **not** contain the now-static `research_analysis`. |
| `earnings_skill_catalog` | Common analyst references plus `thesis_change_policy`, `earnings_quality_policy`, `commentary_reconciliation`, and `earnings_period_map`. |
| `scenario_skill_catalog` | Common analyst references plus `scenario_construction`, `scenario_sensitivity_policy`, `scenario_event_liquidity_policy`, and `scenario_action_threshold_policy`. |
| `idea_skill_catalog` | Common analyst references plus `idea_search_order`, `idea_candidate_funnel`, `idea_candidate_evidence_policy`, `idea_rejection_policy`, and `idea_url_screen_compression`. |
| `wide_skill_catalog` | Common retrieval/period/evidence references. Do not show earnings, scenario, or candidate-selection policies to wide research. |

- [ ] **Step 1: Add failing image tests for catalog subsets**

Create an image fixture with two loadable segments and two generated catalogs. Assert that an `include_skill_ids: ["earnings_quality_policy"]` catalog renders only that ID and its `when_to_use` metadata. Add failures for an unknown ID, duplicate ID, and an ID outside the catalog directories.

```rust
assert!(earnings_catalog.contains("**earnings_quality_policy**"));
assert!(!earnings_catalog.contains("**scenario_construction**"));
```

- [ ] **Step 2: Run the image tests before implementation**

Run:

```bash
cd ~/krw-agnet
cargo test -p krw-agent-image skill_catalog
```

Expected: the current global catalog exposes all registered loadable skills to every role and has no subset validation.

- [ ] **Step 3: Implement catalog-subset rendering and validation**

In `render_skill_catalog`, filter registered `loadable` segments by `include_skill_ids` when that list is non-empty. In `validate_loadable_skill_sources`, reject duplicate IDs, an ID that is not a registered loadable segment, and an ID whose resolved path is outside the catalog directories. Preserve the existing empty-list behavior as “all loadable IDs in these directories,” so other agents remain backward compatible.

- [ ] **Step 4: Replace the global ontology catalog in model roles**

First mark every applicable file in the two existing reference directories as `loadable: true`:

```text
agents/krw-ontology/references/*.md
agents/krw-ontology/skills/research-planner/references/*.md
```

Keep composer-only writing references static and non-loadable (`research_synthesis`, `plain_korean_investor_language`, `final_markdown_contract`, plus the existing mode output contracts) because the composer cannot call tools. Keep future handoff/evaluation references registered but out of all current catalogs. Register generated catalog prompt segments in `agent.yaml` and assign `planner_reference_catalog` to planners and the matching analyst catalog to each analyst. The composer continues to receive no catalog and no `skill.load` capability. Remove the global `skill_catalog` from ontology roles after subset tests cover all mappings.

Rewrite each dynamic reference's frontmatter `when_to_use` from a generic phrase to the exact trigger in **Common Reference Review and Target Ownership**. The catalog must never say merely "when relevant".

Add a compact static instruction to analyst roles: load one named policy only when the current evidence introduces that policy’s trigger, apply it to the next existing action/analysis turn, and do not load a policy merely because it is listed. This is guidance, not a new workflow state or mandatory model turn.

- [ ] **Step 5: Make repeated parent loads non-destructive**

Change `resolve_local_skill_load` to receive `&ActiveRun` and detect a prior visible tool result with the same `{skill_id, content}`. For a duplicate that is still in the transcript, return:

```json
{
  "skill_id": "earnings_quality_policy",
  "status": "already_available_in_context"
}
```

Do not throw, retry, cache an MCP action, or charge a capability budget. If compaction has removed the original body, treat the later request as a normal local load and return the immutable body again.

- [ ] **Step 6: Add runtime regressions**

Add one provider-episode fixture that loads `earnings_quality_policy`, then asks for it again before any compaction. Assert that the second response contains `already_available_in_context`, one full body remains in the transcript, no MCP action appears, and the run can still continue to its existing analyst transition. Add a second fixture that compacts the first body and proves a later reload returns full content rather than an unusable acknowledgement.

- [ ] **Step 7: Run focused verification**

Run:

```bash
cd ~/krw-agnet
cargo test -p krw-agent-image skill_catalog
cargo test -p krw-agent-run-engine skill_load
cargo test -p krw-context-planner specialized_roles_keep_core_skills_and_offer_related_skill_loading
```

Expected: every mode sees a small relevant catalog, duplicate local loads do not bloat the active transcript, and a missing/irrelevant dynamic skill never prevents final composition.

- [ ] **Step 8: Commit dynamic-skill reliability separately**

```bash
git -C ~/krw-agnet add \
  crates/agent-image/src/lib.rs \
  crates/run-engine/src/lib.rs \
  agents/krw-ontology/agent.yaml
git -C ~/krw-agnet commit -m "feat: curate and replay dynamic research skills"
```

### Task 5: Align the Dormant Rust Router Contract Without Making It a Production Dependency

**Files:**

- Modify only if required by the final table: `~/krw-agnet/crates/krw-contracts/src/product.rs`
- Modify only if required by the final table: `~/krw-agnet/agents/krw-router/prompts/router.md`
- Test: `~/krw-agnet/crates/krw-contracts/src/product.rs`
- Test: `~/krw-agnet/crates/run-engine/src/lib.rs`

**Interfaces:**

- The typed `routing-decision/v2` must represent every run kind the pure frontend can select. If `earnings_deep_dive` or `wide_research` are absent from `ProductRunKind`/reason matching, add explicit enum values and exact route reason codes; do not overload `company_research`.

- [ ] **Step 1: Add contract tests for the final table’s specialist kinds**

```rust
assert!(validate_routing_linkage(
    &routing_request("CAT 실적과 가이던스"),
    &routing_decision(ProductRunKind::EarningsDeepDive, RouteReasonCode::EarningsQuestion),
).is_ok());
```

Add a matching wide-research test and a negative test that refuses a company ticker scope for a wide decision when scope semantics would be inconsistent.

- [ ] **Step 2: Run the focused contract test**

Run:

```bash
cd ~/krw-agnet
cargo test -p krw-contracts routing_decision
```

Expected: it either already supports every selected mode or identifies the exact missing enum/reason pairing before release.

- [ ] **Step 3: Align the prompt only with supported contract values**

If a value is missing, add it to `ProductRunKind`, `RouteReasonCode`, `route_reason_matches`, and the router prompt in the same commit. The prompt must still say `company_research`/`wide_research` are safe fallbacks and must never claim the router is a prerequisite for a chat answer.

- [ ] **Step 4: Verify the router remains isolated**

Run:

```bash
cd ~/krw-agnet
cargo test -p krw-contracts routing_decision
cargo test -p krw-agent-run-engine router_finishes_with_its_non_answer_ir_contract
```

Expected: the router image remains executable and contract-correct, while the frontend does not enqueue a standalone `route` run.

- [ ] **Step 5: Commit only if alignment changed files**

```bash
git -C ~/krw-agnet add crates/krw-contracts/src/product.rs agents/krw-router/prompts/router.md crates/run-engine/src/lib.rs
git -C ~/krw-agnet commit -m "chore: align Rust router contract with chat routes"
```

### Task 6: Build, Test, and Release Without Structured Follow-Up Handoff Changes

**Files:**

- Modify: release/deployment manifests only if image hashes or package files require regeneration.
- Test: existing image/context/engine frontend suites listed above.

- [ ] **Step 1: Run static and focused unit checks**

```bash
cd ~/krw-agnet
git diff --check
cargo test -p krw-agent-image skill_catalog
cargo test -p krw-context-planner specialized_roles_keep_core_skills_and_offer_related_skill_loading
cargo test -p krw-agent-run-engine skill_load

cd ~/krw-ontology-front
rg -n '@anthropic-ai/claude-agent-sdk' src scripts package.json package-lock.json
npm exec vitest run src/lib/question-router/chat-question-router.test.ts src/app/api/chat/run/route.test.ts
```

Expected: no whitespace errors; relevant Rust tests pass; the final `rg` has no match; routing tests confirm fallback always produces a Rust run kind.

- [ ] **Step 2: Build the sealed dual-provider release**

Run the established release command from the actual repository spelling:

```bash
cd ~/krw-agnet
scripts/build_dual_provider_release.sh \
  --output-root "$HOME/.local/share/krw-agent/releases/current"
```

Expected: the release contains both GLM and DeepSeek descriptors and all updated image hashes. This command builds artifacts only; it does not start a second routing service.

- [ ] **Step 3: GLM quality checks after deployment to development**

Use the existing live runner for five questions, one per mode:

```text
CAT는 무슨 회사야?
CAT 최근 실적과 가이던스의 핵심 변화는?
CAT의 상승·하락 시나리오와 판단이 바뀌는 조건은?
AI 수혜주 후보를 찾아줘.
반도체 업종이 전력 병목의 영향을 어떻게 받나?
```

For each trace, retain run kind, static prompt-segment IDs, dynamic `skill.load` IDs if any, capability sequence, final answer text, and completion state. A missing dynamic load alone is not failure when its specialist trigger was absent; a missing mode planning segment is failure.

- [ ] **Step 4: Release only after the frontend and release descriptor agree**

Run the frontend’s full deployment command only after its committed tree and the generated release root agree:

```bash
cd ~/krw-ontology-front
npm run prod:deploy:full
```

Verify the post-deploy daemon heartbeat points to the new release artifact and admission mode is `open`. Do not modify any scenario/idea/wide cross-run handoff schema in this release.

---

## Self-Review

- **Spec coverage:** Task 1 removes Claude from active question classification and preserves a safe fallback. Task 2 removes the legacy frontend SDK execution/deployment surface. Task 3 makes baseline mode quality static from planner turn one. Task 4 retains `skill.load` for specialised material but makes discovery relevant and repeated loads safe. Task 5 keeps the Rust router contract healthy without inserting a fragile extra pre-run. Task 6 validates and releases both providers. Structured cross-run follow-up handoff is explicitly excluded.
- **Failure-point check:** no new mandatory model call, external service, MCP action, or terminal validation gate is introduced. Routing has a total fallback; dynamic skills are local and optional for completion; planner skills are role-static.
- **Type consistency:** `ChatQuestionRouteInput.hasExplicitRunKind`, `ChatQuestionRouteDecision.reasonCode`, `SkillCatalogSource.include_skill_ids`, and `already_available_in_context` are defined once above and used consistently by their tasks.
- **Placeholder scan:** no undecided implementation branches remain. Conditional file edits in Task 5 are deliberately no-ops when contract tests demonstrate the values already exist; they do not defer an unspecified design.

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-08-14-rust-routing-and-skill-packs.md`.

Two execution options:

1. **Subagent-Driven (recommended)** — dispatch a fresh subagent per task and review between tasks.
2. **Inline Execution** — execute the tasks in this session in small checkpoints.
