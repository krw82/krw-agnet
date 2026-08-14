# GLM Recovery and Feed Credential Synchronization Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** GLM의 정상 응답이 하네스 경계에서 실패하는 원인을 안전하게 분류·호환하고, Feed MCP와 Rust 데몬이 같은 인증값을 사용하도록 배포 경로를 단일화한다.

**Architecture:** GLM 쪽은 모델 원문이나 chain-of-thought를 보존하지 않고, 이미 존재하는 실패 경로에 작은 폐쇄형 진단 코드만 추가한다. 재현된 프로토콜 형태가 의미적으로 동일할 때만 wire 단계에서 결정적으로 정규화하고, 그렇지 않으면 현재의 안전한 재시도 안내를 유지한다. Feed 쪽은 프런트 `.env`의 토큰을 임시로 덧붙이는 방식에서 끝내지 않고, 검증된 후보 운영 env를 원자적으로 활성화해 Feed worker와 Rust daemon이 재시작 뒤에도 동일한 값으로 동작하게 한다.

**Tech Stack:** Rust, Tokio, Anthropic-compatible GLM Messages/SSE, TypeScript/Node.js, Bash, macOS launchd, local TLS MCP gateway.

## Global Constraints

- 수정 대상은 `~/krw-agnet`와 `~/krw-ontology-front`만이다.
- `~/krw-ontology`와 ontology schema는 수정하지 않는다.
- 모델 호출 수, reasoning 수준, 모델별 tool allowlist, capability budget을 줄이지 않는다.
- 새 LLM turn, 자동 재질문, 무한 재시도를 추가하지 않는다. 기존 recovery budget 안에서만 동작한다.
- 최종 답변에 새 hard gate를 넣지 않는다. 복구할 수 없는 경우에도 현재의 안전한 상태 응답을 유지한다.
- provider 원문, 프롬프트, tool arguments, 토큰, 파일 경로를 사용자 응답·배포 로그·release artifact에 넣지 않는다.
- Feed 토큰은 release manifest, public descriptor, GCP archive에 포함하지 않는다.
- 라이브 품질 호출은 GLM만 사용한다. DeepSeek은 release contract 검사만 수행한다.
- 실제 production activation, launchd 재시작, runtime env 교체는 구현·검증 완료 후 별도 승인에서만 수행한다.

## Current Evidence

- `normal_now_genai_risk`는 `ontology.company_context`를 성공한 뒤 `provider_protocol_failure`로 종료됐다. MCP empty 결과가 아니다.
- `complex_intc_segments_ai_cashflow`는 6개의 provider episode와 3개의 accepted MCP action까지 진행했지만 600초 품질-runner 대기 한도를 넘었다. 이 계획은 workflow를 줄이지 않고, GLM protocol failure와 runner timeout을 분리해 관측한다.
- 현재 Feed worker는 프런트 `.env`의 토큰으로는 인증되지만 operator runtime의 `krw-agent-deploy.env` 토큰으로는 HTTP 401을 반환한다.
- `deploy-production-full.sh`의 임시 overlay는 새 agent activation에는 도움이 되지만, operator runtime env 자체를 갱신하지 않아 독립적인 재시작 뒤 다시 불일치할 수 있다.

## File Map

### GLM protocol recovery

- Modify: `crates/run-engine/src/lib.rs` — closed provider-protocol diagnostic와 의미 보존 정규화의 연결 지점.
- Modify: `crates/provider-wire/src/sse.rs` — GLM SSE event/body 일치와 `message_stop` body 검사.
- Modify: `crates/provider-wire/src/assembler.rs` — block lifecycle state machine과 정확한 tool-input 조립.
- Modify: `crates/runtime-persistence/src/executor.rs` — terminal outcome에 private-content 없는 provider failure class를 보존.
- Modify: `scripts/run_live_quality_matrix.py` — private report에 safe failure class와 terminal usage를 기록.
- Modify: `scripts/test_live_quality_matrix.py` — failure-class report shape 회귀 테스트.
- Test: `crates/provider-wire/src/sse.rs`, `crates/provider-wire/src/assembler.rs`, `crates/run-engine/src/lib.rs`, `crates/runtime-persistence/src/executor.rs` test modules.

### Feed credential synchronization

- Modify: `~/krw-ontology-front/scripts/init-mac-worker-env.sh` — canonical Feed token alias 처리와 conflict 검사.
- Modify: `~/krw-ontology-front/scripts/deploy-production-full.sh` — candidate operator runtime env 생성, authenticated Feed probe, 원자적 env activation/rollback.
- Modify: `~/krw-ontology-front/scripts/deploy-production-fast.sh` — Mac runtime verification 단계에서 Feed bearer probe를 필수 pre-activation check로 포함.
- Modify: `~/krw-ontology-front/src/lib/deploy-scripts.test.ts` — token conflict, no-secret output, probe-before-activation, atomic replacement contract 검사.
- Modify: `scripts/run_direct_mcp_audit.py` — operator env와 supplied front env의 auth-fingerprint 결과를 secret-free로 구분해 report한다.
- Test: `scripts/test_direct_mcp_audit.py`, `~/krw-ontology-front/src/lib/deploy-scripts.test.ts`.

---

### Task 1: Capture the Exact GLM Failure Class Without Retaining Model Content

**Files:**
- Modify: `crates/run-engine/src/lib.rs:11966-12040`
- Modify: `crates/runtime-persistence/src/executor.rs:751-827,860-990`
- Modify: `scripts/run_live_quality_matrix.py:701-820`
- Modify: `scripts/test_live_quality_matrix.py`

**Interfaces:**

- Extends the existing bounded terminal diagnostic:

```rust
pub struct EngineFailureDiagnosticV1 {
    pub kind: &'static str,
    pub identifier_hash: ContentHash,
}
```

- Allowed protocol `kind` values are only `provider_protocol_tool_arguments_not_object`, `provider_protocol_tool_call_id_invalid`, `provider_protocol_tool_call_count_exceeded`, the existing closed `provider_episode_*` values, `provider_protocol_sse_parse_error`, and `provider_protocol_unknown`.
- The diagnostic is stored under the existing terminal `release` diagnostic object and copied only into the private quality report. It never contains provider text, tool arguments, IDs, or a stack trace.

- [ ] **Step 1: Add failing run-engine diagnostic tests**

```rust
#[test]
fn tool_arguments_shape_has_a_closed_glm_protocol_diagnostic() {
    let diagnostic = durable_failure_diagnostic(&EngineError::ToolArgumentsMustBeObject(
        "tool-call".to_owned(),
    ))
    .expect("closed diagnostic");
    assert_eq!(diagnostic.kind, "provider_protocol_tool_arguments_not_object");
}
```

Add corresponding tests for an invalid tool ID, too many calls, and each known `InvalidProviderEpisode` string.

- [ ] **Step 2: Run the focused failure tests and confirm the current opaque outcome**

Run:

```bash
cargo test -p krw-agent-run-engine durable_failure_diagnostic
```

Expected before implementation: only `InvalidProviderEpisode` has a class; the other terminal protocol paths have no specific safe diagnostic.

- [ ] **Step 3: Add the closed diagnostic mapping**

Extend `durable_failure_diagnostic` so every `provider_protocol_failure` source maps to one constant class. Hash only the class string as today; do not preserve the tool name or provider output.

```rust
EngineError::ToolArgumentsMustBeObject(_) => Some(EngineFailureDiagnosticV1 {
    kind: "provider_protocol_tool_arguments_not_object",
    identifier_hash: ContentHash::sha256(b"provider_protocol_tool_arguments_not_object"),
}),
```

- [ ] **Step 4: Preserve the class in the private quality report**

Extend the runner’s terminal failure result with an optional `failure_class` that accepts only `^[a-z0-9_]{1,128}$`. Read it from the terminal release diagnostic; do not call a new endpoint or expose it in the product SSE response.

```python
failure_class = terminal.get("release", {}).get("failure_kind")
if failure_class is not None and not FAILURE_CLASS_RE.fullmatch(failure_class):
    raise GatewayProblem("terminal_failure_class_invalid")
```

- [ ] **Step 5: Verify terminal-message privacy remains unchanged**

Run:

```bash
cargo test -p krw-agent-run-engine durable_failure_diagnostic
cargo test -p krw-agent-runtime-persistence engine_execution_failure
cd ~/krw-ontology-front && npx vitest run src/lib/agent --reporter=dot
python3 ~/krw-agnet/scripts/test_live_quality_matrix.py
```

Assert that the user-facing retry copy still says only “모델 응답을 해석하거나 복구…” and never includes the new class.

### Task 2: Apply Only the Reproduced GLM Wire Compatibility Repair

**Files:**
- Modify: `crates/provider-wire/src/sse.rs:360-450`
- Modify: `crates/provider-wire/src/assembler.rs:150-400`
- Modify: `crates/run-engine/src/lib.rs:11491-11525`
- Test: `crates/provider-wire/src/sse.rs` and `crates/provider-wire/src/assembler.rs` test modules
- Test: `crates/run-engine/src/lib.rs` test module

**Interfaces:**

- Consumes the closed `EngineFailureDiagnosticV1.kind` observed in Task 1.
- Produces the same `ProviderEpisodeV1` as a canonical Anthropic event sequence when, and only when, the alternate GLM event sequence has identical semantic content.
- Does not change any public MCP contract, workflow state, model budget, or number of model turns.

- [ ] **Step 1: Add an exact captured-shape fixture before changing behavior**

Create a fixture with only the sanitized structural elements of the observed failure: event names, block indices, block kinds, and whether tool input is an object/string. Replace every text/tool value with fixed dummy literals.

```rust
#[test]
fn glm_stringified_object_tool_input_normalizes_to_one_object() {
    let mut assembler = test_assembler();
    for event in sanitized_glm_fixture_events() {
        assembler.push(event)?;
    }
    let episode = assembler.finish()?;
    assert!(episode.tool_calls[0].arguments.is_object());
}
```

If Task 1 identifies a different class, write the corresponding exact fixture instead. Do not add a speculative normalization.

- [ ] **Step 2: Strengthen SSE event integrity**

In `parse_sse_frame`, parse the JSON envelope even when an `event:` line exists. Require `event:` to equal JSON `type` when both are present, and parse `message_stop` as `{"type":"message_stop"}` rather than accepting arbitrary bytes.

```rust
fn json_type(data: &str) -> Result<Option<String>, WireError> {
    Ok(serde_json::from_str::<Value>(data)
        .map_err(|_| WireError::SseParseError("event_body_invalid".into()))?
        .get("type")
        .and_then(Value::as_str)
        .map(str::to_owned))
}

if let (Some(event_name), Some(body_type)) =
    (event_type.as_deref(), json_type(&data)?.as_deref())
{
    if event_name != body_type {
        return Err(WireError::SseParseError("event_type_mismatch".into()));
    }
}
```

- [ ] **Step 3: Make content-block ordering explicit**

Replace independent `BTreeMap` accumulation with an index state that records `{ kind, started, stopped }`. Reject delta-before-start, duplicate start, mixed text/tool use at one index, delta-after-stop, and missing stop at finish.

```rust
struct BlockState {
    kind: BlockKind,
    started: bool,
    stopped: bool,
}
```

This hardens malformed streams; it does not reject any valid GLM stream shape.

- [ ] **Step 4: Add the single observed semantic normalization**

Only if Task 1 proves `tool_arguments_not_object`, accept a tool input that is a single bounded UTF-8 JSON string whose decoded value is exactly one object. Convert it to the canonical object before `ProviderEpisodeV1` validation. Reject arrays, scalars, nested double-encoding, invalid JSON, and values above the existing tool-input bound.

```rust
let decoded: Value = serde_json::from_str(&raw_input)?;
let Value::Object(_) = decoded else {
    return Err(WireError::InvalidToolArguments);
};
```

Do not add a repair turn for this equivalent encoding; the existing turn continues.

- [ ] **Step 5: Retain the existing recovery policy for true invalid output**

For a non-equivalent output, retain the existing declared repair edge if it remains. If the budget is exhausted, retain the current terminal retry message. Do not defer the same immutable run and do not create a new model call.

- [ ] **Step 6: Run protocol and engine regression tests**

Run:

```bash
cargo test -p krw-agent-provider-wire
cargo test -p krw-agent-run-engine glm
cargo test -p krw-agent-runtime-persistence provider_protocol
```

Expected: the captured valid-equivalent GLM fixture completes; malformed stream fixtures fail closed with a safe class; ordinary DeepSeek fixtures remain unchanged.

### Task 3: Verify GLM With Existing Long-Lived Infrastructure

**Files:**
- Modify: `scripts/run_live_quality_matrix.py`
- Modify: `scripts/test_live_quality_matrix.py`
- Create: private `/tmp/krw-glm-recovery-<timestamp>/` report at execution time only

**Interfaces:**

- Reuses an already-running GLM gateway.
- Produces private report fields `failure_class`, `provider_turns`, `capability_calls`, `repairs`, `elapsed_seconds`, and original final Markdown only for completed cases.

- [ ] **Step 1: Add a runner test for a failed terminal response with a safe class**

```python
def test_failed_terminal_report_keeps_closed_failure_class_only() -> None:
    result = review_block(case, "failed", None, trace, failure_class="provider_protocol_tool_arguments_not_object")
    assert result["research_quality_verdict"] == "not_assessable"
    assert "tool_arguments" not in result["reason"]
```

- [ ] **Step 2: Run the short control and normal reproduction serially**

```bash
KRW_AGENT_PROVIDER=glm \
python3 scripts/run_live_quality_matrix.py \
  --case short_google_pullback_long_term \
  --case normal_now_genai_risk \
  --parallelism 1 \
  --timeout-seconds 900 \
  --report-dir /tmp/krw-glm-recovery-<timestamp>
```

Expected: the short control remains final; the normal case is final or carries a concrete closed `failure_class`, never an opaque `provider_protocol_failure` only.

- [ ] **Step 3: Run the complex case separately with its own deadline report**

```bash
KRW_AGENT_PROVIDER=glm \
python3 scripts/run_live_quality_matrix.py \
  --case complex_intc_segments_ai_cashflow \
  --parallelism 1 \
  --timeout-seconds 1200 \
  --report-dir /tmp/krw-glm-complex-<timestamp>
```

If it exceeds the runner deadline, request cancellation through the authenticated gateway and record `runner_timeout`; do not leave an active test run behind. This records the latency issue without changing the research workflow.

- [ ] **Step 4: Review answer content separately from transport**

For every final answer, review only: whether the question is answered, evidence is not described as absent when related support exists, causal language is conditional where needed, and the stated company/period are correct. A transport pass is not a quality pass.

### Task 4: Make Feed Credential Resolution Deterministic

**Files:**
- Modify: `~/krw-ontology-front/scripts/init-mac-worker-env.sh:145-165,300-312`
- Modify: `~/krw-ontology-front/scripts/deploy-production-full.sh:51-185`
- Modify: `~/krw-ontology-front/src/lib/deploy-scripts.test.ts`
- Modify: `scripts/run_direct_mcp_audit.py`
- Modify: `scripts/test_direct_mcp_audit.py`

**Interfaces:**

- Consumes `KRW_FEED_MCP_TOKEN` or legacy `FEED_MCP_AUTH_TOKEN` from the front source env.
- Produces exactly one canonical `KRW_FEED_MCP_TOKEN` in a mode-0600 operator runtime env.
- Returns only `match`, `mismatch`, `missing`, or `unauthorized` for token verification; never returns a token or a token hash.

- [ ] **Step 1: Add failing alias/conflict fixtures**

Use temporary env files containing: canonical only, legacy only, both equal, both different, and neither. Assert that only the first three resolve and that the resulting candidate has one canonical key.

```bash
fixture_dir=$(mktemp -d)
fixture_env="$fixture_dir/source.env"
printf '%s\n' \
  'NEXT_PUBLIC_SUPABASE_URL=https://example.invalid' \
  'NEXT_PUBLIC_SUPABASE_ANON_KEY=anon' \
  'SUPABASE_URL=https://example.invalid' \
  'SUPABASE_ANON_KEY=anon' \
  'SUPABASE_SERVICE_ROLE_KEY=service' \
  'DEEPSEEK_API_KEY=test' \
  'KRW_FEED_MCP_TOKEN=first' \
  'FEED_MCP_AUTH_TOKEN=second' > "$fixture_env"
if scripts/init-mac-worker-env.sh --from "$fixture_env" --out "$fixture_dir/out.env" \
  >"$fixture_dir/stdout" 2>"$fixture_dir/stderr"; then
  exit 1
fi
rg -q 'Feed MCP token aliases conflict' "$fixture_dir/stderr"
```

- [ ] **Step 2: Make `init-mac-worker-env.sh` reject a conflicting pair**

Parse both aliases with the existing quote/export-aware reader. If both are non-empty and differ, stop before writing the output. If only the legacy name exists, write it once as `KRW_FEED_MCP_TOKEN` and never copy the legacy key into the generated env.

```sh
if [ -n "$CANONICAL_TOKEN" ] && [ -n "$LEGACY_TOKEN" ] && [ "$CANONICAL_TOKEN" != "$LEGACY_TOKEN" ]; then
  echo "Feed MCP token aliases conflict in source env" >&2
  exit 1
fi
```

- [ ] **Step 3: Build a candidate operator runtime env atomically**

In `deploy-production-full.sh`, replace the append-only overlay with a candidate file in the same directory as `krw-agent-deploy.env`. Upsert one shell-quoted canonical key, chmod it `0600`, and leave the source file unchanged until the Feed probe passes.

```bash
upsert_env_value() {
  target=$1
  key=$2
  value=$3
  temp="${target}.tmp.$$"
  awk -F= -v wanted="$key" '
    function normalized_key(raw) {
      sub(/^[[:space:]]*export[[:space:]]+/, "", raw)
      gsub(/^[[:space:]]+|[[:space:]]+$/, "", raw)
      return raw
    }
    normalized_key($1) != wanted { print }
  ' "$target" > "$temp"
  printf "%s='%s'\n" "$key" "$(printf '%s' "$value" | sed "s/'/'\\\\''/g")" >> "$temp"
  chmod 600 "$temp"
  mv "$temp" "$target"
}

candidate_env="$agent_runtime_env_source.candidate.$$"
umask 077
cp -- "$agent_runtime_env_source" "$candidate_env"
upsert_env_value "$candidate_env" KRW_FEED_MCP_TOKEN "$feed_token"
chmod 600 "$candidate_env"
```

- [ ] **Step 4: Probe Feed with the exact candidate env before agent activation**

Source the candidate only in a subshell and run the existing Feed production verifier against the live loopback TLS endpoint. Its output may state success/failure but must not echo shell values.

```bash
( set -a; . "$candidate_env"; set +a; "$front_dir/scripts/verify-krw-feed-mcp-production.sh" )
```

On a 401, stop before building or activating the agent release. Do not disable Feed auth and do not silently continue with a stale token.

- [ ] **Step 5: Activate the verified runtime env with rollback**

After the probe succeeds, save the old regular mode-0600 env as a private rollback copy, atomically `mv` the candidate into `krw-agent-deploy.env`, then invoke the existing sealed-release build and fast deploy. If build or activation fails, restore the exact prior env before returning a failure status.

- [ ] **Step 6: Make the model-free audit show configuration status**

Add `feed_auth_source: "operator" | "front_override"` and `feed_auth_result: "ok" | "unauthorized" | "missing"` to the private direct-audit report. Do not include a token hash, URL query, authorization header, or any value from an env file.

- [ ] **Step 7: Run shell and script contract tests**

Run:

```bash
bash -n ~/krw-ontology-front/scripts/init-mac-worker-env.sh
bash -n ~/krw-ontology-front/scripts/deploy-production-full.sh
python3 -m unittest discover -s scripts -p 'test_direct_mcp_audit.py'
cd ~/krw-ontology-front && \
  npx vitest run src/lib/deploy-scripts.test.ts --reporter=dot
```

Expected: matching aliases and a valid Feed probe pass; mismatched aliases or a 401 fail before release activation; logs contain neither fixture secret.

### Task 5: Controlled Activation and Evidence-Based Rollback

**Files:**
- Modify: `~/krw-ontology-front/scripts/deploy-production-full.sh`
- Modify: `~/krw-ontology-front/scripts/deploy-production-fast.sh`
- Modify: `docs/superpowers/plans/2026-08-14-glm-recovery-feed-auth-sync.md`

**Interfaces:**

- Requires a clean committed `krw-agnet` and `krw-ontology-front` tree.
- Activates the exact sealed dual-provider release produced by the full deploy.
- Uses DeepSeek as the production provider unless the existing explicit `--agent-provider glm` option is supplied.

- [ ] **Step 1: Verify clean-tree and provider package prerequisites**

```bash
git -C ~/krw-agnet status --short
git -C ~/krw-ontology-front status --short
python3 ~/krw-agnet/scripts/test_dual_provider_release.py
```

Expected: no uncommitted source files and both sealed provider contracts pass before activation.

- [ ] **Step 2: Run the deploy-only Feed preflight**

Run the full deploy in its existing preflight mode, verify that it reports a successful authenticated Feed probe, and confirm no runtime env is replaced in dry-run/preflight mode.

- [ ] **Step 3: Run one post-activation direct MCP audit**

```bash
python3 ~/krw-agnet/scripts/run_direct_mcp_audit.py \
  --operator-root ~/krw-agnet-prod \
  --ticker AAPL \
  --strict-auth
```

Expected: Ontology, Feed, Filings, and Guru are ready; Feed `feed_auth_result` is `ok`; `provider_calls` remains `0`.

- [ ] **Step 4: Open admission only after activation evidence passes**

Keep the existing deploy behavior that writes `KRW_AGENT_ADMISSION_MODE=open` after deep readiness. Do not add a canary-only default or leave a successful deployment closed.

- [ ] **Step 5: Roll back only the changed release/env pair on failure**

If Feed preflight, daemon readiness, or direct MCP audit fails, restore the prior operator runtime env and prior release symlink/launchd service through the existing fast-deploy rollback path. Do not roll back ontology data or alter user chat history.

## Self-Review

- GLM coverage: Tasks 1–3 first reveal the exact closed failure class, then add only an observed semantic compatibility repair. They do not add calls, reasoning, final-answer gates, or raw provider retention.
- Feed coverage: Tasks 4–5 eliminate the stale operator-token path, check alias conflicts, probe the actual bearer request before activation, preserve secret handling, and restore the previous env on activation failure.
- Failure-point check: all new checks run at build/deploy time or produce private diagnostics. The research runtime still answers normally or returns the existing safe retry message; no new model-facing validation rule is added.
- Scope check: no ontology source or schema files are in the file map. DeepSeek receives package/contract verification only.
- Placeholder scan: every implementation task identifies an exact file, interface, test command, and expected behavior.

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-08-14-glm-recovery-feed-auth-sync.md`.

Two execution options:

1. **Inline execution** — implement Tasks 1–5 here with checkpoints and test evidence.
2. **Task-by-task review** — implement GLM recovery first, review the actual failure class, then implement Feed synchronization separately.

Because the two changes are independent and Feed activation touches production credentials, Task-by-task review is the safer sequencing.
