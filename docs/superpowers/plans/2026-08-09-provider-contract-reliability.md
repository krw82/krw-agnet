# Provider Contract Reliability Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make provider-output guarantees explicit, use the strongest exact structured-output mode admitted by each provider, and enforce the existing request-size boundary against the complete serialized provider body before network dispatch.

**Architecture:** Keep the current closed workflow/state-machine design. A `TypedJson` state continues to own exactly one canonical output contract; startup context compilation derives a provider-compatible schema projection and pins its hash. The runtime sends GLM's admitted `response_format.type=json_object` for every TypedJson turn, while `output_config.format=json_schema` or `strict: true` remain capability-gated for endpoints that explicitly support them. DeepSeek remains on the existing prompt-plus-local-validation path with no live calls in this phase. GLM is the only provider eligible for a live structured-output admission probe.

**Tech Stack:** Rust 1.97.1, serde/serde_json/serde_jcs, reqwest, Tokio, existing AgentImage/ContextPlanner/ProviderWire crates.

**Current execution note (2026-08-09):** The implementation now activates GLM's provider-native JSON
mode for all TypedJson states. Workspace compilation and rustfmt checks pass. The live GLM probe accepts
baseline, JSON mode, and strict transition-tool input; Z.AI's Anthropic endpoint rejects the newer JSON
Schema output shape, so only that unsupported flag remains disabled. All three deterministic quality
replays pass at score 100. The quality fixture now allows up to twelve dispatched clauses and maps
semantically related clauses to fixture evidence without silently dropping extra objectives. Live GLM
quality reaches the provider and fixture plan gate, but the current multi-objective proposal is still
rejected by the kernel's canonical research-plan contract; this is proposal quality/contract fit, not a
wire/Structured Outputs failure. No DeepSeek credential, endpoint, or live request was used.

The checklist below is retained as the implementation design record; the current execution note and
`docs/IMPLEMENTATION_STATUS.md` are authoritative for what is already implemented and verified.

## Global Constraints

- Do not expose provider selection, schema selection, or data-provider fallback to the model.
- Do not turn the Korean `final-markdown/v1` path into JSON. Structured output applies only to existing `TypedJson` states.
- `supports_json_schema_output` and `supports_strict_tool_input` remain `false` for DeepSeek V4 Flash and GLM-5.2 in every mode. GLM's separate `supports_json_object` capability is already admitted and sends the provider-native JSON mode for TypedJson states.
- Provider-network tests are GLM-only in this phase. DeepSeek may appear in static capability and serialization fixtures to prove unsupported features stay disabled, but no DeepSeek credential, endpoint, or live request is used.
- Preserve canonical local validation for evidence linkage, period policy, calculation lineage, product context, and agent rules. Provider schema conformance is not evidence correctness.
- A provider-constrained response that is malformed, refused, or truncated is a typed provider-contract failure, not an LLM repair prompt. Semantic validation may still use the existing single bounded repair.
- Do not introduce model-driven tool discovery, dynamic plugin loading, broad tool catalogs, or automatic provider fallback.
- The current worktree already has unstaged changes in `crates/provider-wire/` and `crates/run-engine/`. Execute this plan in a fresh `codex/` worktree or coordinate and preserve those changes; never use `git add -A` or overwrite their baseline.
- This planning turn runs no tests. Every `Run:` command below belongs to implementation time only.

## Phase-1 verification boundary

- Offline unit and serialization checks use existing GLM fixtures where a provider-shaped fixture is needed, plus static capability assertions that DeepSeek remains disabled. They do not read provider credentials or make network requests.
- The only live provider operation is the GLM-5.2 admission/quality path using `GLM_API_KEY` and the configured z.ai Anthropic endpoint. The fixed probe checks baseline, JSON mode, and one strict `krw_agent_transition` tool request as separate results.
- DeepSeek has no live probe and no endpoint-dependent acceptance claim in this phase. A failed or unavailable GLM JSON-mode probe leaves the structured-output capability disabled; the JSON Schema/strict flags are never enabled automatically.

---

## File Structure

| File | Action | Responsibility |
|---|---|---|
| `crates/protocol/src/lib.rs` | Modify | Add explicit constrained-output and strict-tool capability facts; pin provider context capacity in execution snapshots. |
| `crates/runtime-config/src/lib.rs` | Modify | Validate the schema-v4 model registry and populate the new snapshot field. |
| `deployments/local/model-registry.yaml` | Modify | Declare current provider modes honestly as prompt-JSON only. |
| `deployments/prod/model-registry.yaml` | Modify | Mirror the production DeepSeek capability contract. |
| `crates/provider-wire/src/lib.rs` | Modify | Represent `output_config.format`, per-tool `strict`, schema projection, bounded retry metadata, and request footprint. |
| `crates/context-planner/src/lib.rs` | Modify | Compile and hash one provider-schema projection for each `TypedJson` state. |
| `crates/run-engine/src/lib.rs` | Modify | Encode constrained output conditionally, bind it into request receipts, preserve semantic-only repair, and preflight full request context. |
| `bins/krw-agent/src/main.rs` | Modify | Make the live provider probe GLM-only for this phase, keep it compatible with the expanded request type, and add the Task 6 redacted provider-error report. |
| `crates/provider-wire/src/lib.rs`, `crates/context-planner/src/lib.rs`, `crates/protocol/src/lib.rs`, `crates/runtime-config/src/lib.rs`, `crates/run-engine/src/lib.rs` | Modify tests | Add unit and integration coverage next to the affected behavior. |
| `bins/krw-agent/tests/release_cli.rs`, `crates/research-quality/src/lib.rs`, `crates/perf-harness/src/main.rs` | Modify | Update their direct `PinnedExecutionContract`, `ResolvedExecutionSnapshot`, and `MessagesRequest` constructors after the new fields are added. |
| `README.md`, `docs/IMPLEMENTATION_STATUS.md` | Modify | Align provider terminology and supported guarantees with the actual registry. |

## Task 1: Make provider guarantees explicit and snapshot-stable

**Files:**

- Modify: `crates/protocol/src/lib.rs:13-27, 308-407, 1011-1077`
- Modify: `crates/runtime-config/src/lib.rs:25-27, 969-983, 1135-1179, 1270-1281`
- Modify: `deployments/local/model-registry.yaml:1-48`
- Modify: `deployments/prod/model-registry.yaml:1-42`
- Test: `crates/protocol/src/lib.rs`
- Test: `crates/runtime-config/src/lib.rs`
- Test: direct `ResolvedExecutionSnapshot` fixtures in `crates/run-engine/src/lib.rs`, `crates/research-quality/src/lib.rs`, and `crates/perf-harness/src/main.rs`; the direct `PinnedExecutionContract` fixture in `bins/krw-agent/tests/release_cli.rs`.

**Interfaces:**

- Consumes: existing `ProviderWireModeCapabilities`, `ModelDescriptor`, `ResolvedExecutionSnapshot`, and `PinnedExecutionContract`.
- Produces: a truthful capability matrix and a run-pinned `provider_max_context_tokens: u32` available to the run engine.

- [ ] **Step 1: Write protocol deserialization and invariant tests**

Add tests beside the current protocol capability tests. They must prove that an older serialized `ProviderWireModeCapabilities` value deserializes with both new feature flags disabled, and that a strict-tool declaration without tool support is rejected. Do not deserialize a pre-v7 full execution snapshot in the v7 path: the protocol-version bump deliberately keeps it owned by the prior runtime.

```rust
#[test]
fn missing_new_provider_capability_fields_default_to_false() {
    let mode: ProviderWireModeCapabilities = serde_json::from_value(serde_json::json!({
        "supported": true,
        "supports_tools": true,
        "supports_tool_choice": false,
        "supports_json_object": true
    }))
    .unwrap();

    assert!(!mode.supports_json_schema_output);
    assert!(!mode.supports_strict_tool_input);
}

#[test]
fn strict_tool_input_requires_tools() {
    let capabilities = ProviderWireCapabilities {
        thinking: ProviderWireModeCapabilities {
            supported: true,
            supports_tools: false,
            supports_tool_choice: false,
            supports_json_object: true,
            supports_json_schema_output: false,
            supports_strict_tool_input: true,
        },
        non_thinking: ProviderWireModeCapabilities::default(),
        requires_thinking_block_replay: false,
        requires_assistant_content_for_tool_calls: false,
    };

    assert!(!capabilities.is_well_formed());
}
```

- [ ] **Step 2: Run the protocol tests to verify the new fields are absent**

Run:

```bash
cargo test -p krw-agent-protocol --lib
```

Expected: compilation fails because the fields and `Default` implementation do not exist yet.

- [ ] **Step 3: Extend the capability matrix without changing current provider behavior**

In `ProviderWireModeCapabilities`, retain `supports_json_object` as the legacy/prompted JSON capability and add two separately documented fields. Derive `Default` so test fixtures can safely use a disabled mode.

```rust
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(clippy::struct_excessive_bools)]
pub struct ProviderWireModeCapabilities {
    pub supported: bool,
    pub supports_tools: bool,
    pub supports_tool_choice: bool,
    /// Prompted JSON plus local parse/validation; not a provider grammar guarantee.
    pub supports_json_object: bool,
    /// Exact JSON Schema output through the active provider wire protocol.
    #[serde(default)]
    pub supports_json_schema_output: bool,
    /// Provider-enforced input schema for an individual advertised tool.
    #[serde(default)]
    pub supports_strict_tool_input: bool,
}
```

Update `is_well_formed()` so both `supports_tool_choice` and `supports_strict_tool_input` require `supports_tools`. Set the two new fields to `false` in `deepseek_v4_flash()` and `glm_5_2()`.

- [ ] **Step 4: Pin the model context limit into the execution contract**

Add `provider_max_context_tokens: u32` immediately after `provider_api_version` in both `ResolvedExecutionSnapshot` and `PinnedExecutionContract`, copy it in `From<&ResolvedExecutionSnapshot>`, and set it from `ModelDescriptor.max_context_tokens` in `runtime-config`.

Increment both versions together because a persisted claim must not silently acquire a changed execution contract.

```rust
pub const PROTOCOL_VERSION: u16 = 7;
pub const CLAIM_PAYLOAD_SCHEMA_VERSION: u16 = 7;

// Insert this field between the two existing adjacent fields in both structs.
pub provider_api_version: String,
pub provider_max_context_tokens: u32,
pub provider_wire_capabilities: ProviderWireCapabilities,

// Copy it in From<&ResolvedExecutionSnapshot> for PinnedExecutionContract.
provider_max_context_tokens: snapshot.provider_max_context_tokens,
```

Do not make `provider_max_context_tokens` optional: old persisted claims must remain owned by the old runtime/release rather than being resumed with an unknown context limit.

- [ ] **Step 5: Migrate the model registry schema and validate exact current facts**

Set `MODEL_REGISTRY_SCHEMA_VERSION` to `4`, update both registry files to `schema_version: 4`, and add these fields under every `thinking` and `non_thinking` mapping:

```yaml
supports_json_object: true
supports_json_schema_output: false
supports_strict_tool_input: false
```

Keep the existing model-specific equality checks in `validate_deepseek_model()` and `validate_glm_model()`. They must reject a hand-edited `true` flag until a dedicated admission change updates the compiled capability constant and its live evidence.

- [ ] **Step 6: Update all snapshot fixtures deliberately**

Add `provider_max_context_tokens` to each direct snapshot constructor. Use the exact fixture model capacity, not a generic test value:

```rust
provider_max_context_tokens: 1_000_000,
```

Current direct constructors are in:

- `crates/run-engine/src/lib.rs`
- `crates/research-quality/src/lib.rs`
- `crates/perf-harness/src/main.rs`

Update the direct `PinnedExecutionContract` fixture in `bins/krw-agent/tests/release_cli.rs` with the same `provider_max_context_tokens: 1_000_000` value.

- [ ] **Step 7: Run focused protocol and runtime-config tests**

Run:

```bash
cargo test -p krw-agent-protocol --lib
cargo test -p krw-agent-runtime-config --lib
```

Expected: both local and production registry fixtures load as schema version 4; any configuration that changes the new DeepSeek/GLM flags fails exact validation.

- [ ] **Step 8: Commit only the contract-matrix change**

```bash
git add crates/protocol/src/lib.rs crates/runtime-config/src/lib.rs \
  deployments/local/model-registry.yaml deployments/prod/model-registry.yaml \
  crates/research-quality/src/lib.rs crates/perf-harness/src/main.rs \
  crates/run-engine/src/lib.rs bins/krw-agent/tests/release_cli.rs
git commit -m "feat(protocol): pin structured-output provider capabilities"
```

Before committing, inspect the staged diff and remove unrelated pre-existing changes from the index.

---

## Task 2: Add provider-wire representations and a loss-aware schema projection

**Files:**

- Modify: `crates/provider-wire/src/lib.rs:138-189, 655-758, 760-860, 1273-1375`
- Test: `crates/provider-wire/src/lib.rs`
- Modify as constructors require: `bins/krw-agent/src/main.rs`, `crates/run-engine/src/lib.rs`, `crates/perf-harness/src/main.rs`

**Interfaces:**

- Produces: `OutputConfig`, `OutputFormat::JsonSchema`, `ProviderToolDefinition::with_strict`, `project_anthropic_json_schema`, and `ProviderRequestFootprint`.
- Consumes: canonical JSON Schema values from `krw-contracts`; never trusts a model-authored schema.

- [ ] **Step 1: Write serialization tests for the exact wire shape**

Write a provider-wire unit test that serializes one request with one JSON schema and one strict transition tool. Assert the provider-facing shape, not Rust field names.

```rust
assert_eq!(
    serde_json::to_value(&request).unwrap()["output_config"],
    serde_json::json!({
        "format": {
            "type": "json_schema",
            "schema": {"type": "object", "additionalProperties": false}
        }
    })
);
assert_eq!(
    serde_json::to_value(&request).unwrap()["tools"][0]["strict"],
    serde_json::json!(true)
);
```

Also test that a normal request omits both `output_config` and `strict`, preserving current DeepSeek/GLM payloads byte-for-byte apart from the new absent fields.

- [ ] **Step 2: Run the provider-wire test to verify it fails**

Run:

```bash
cargo test -p krw-agent-provider-wire --lib structured_output
```

Expected: compilation fails because `OutputConfig`, `OutputFormat`, and `with_strict` are not defined.

- [ ] **Step 3: Add minimal wire data types**

Add the following types to `provider-wire`; use `Option` fields so unsupported fields are absent rather than sent as `false` or `{}`.

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputConfig {
    pub format: OutputFormat,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum OutputFormat {
    JsonSchema { schema: JsonSchemaDocument },
}

impl OutputConfig {
    pub fn json_schema(schema: JsonSchemaDocument) -> Self {
        Self { format: OutputFormat::JsonSchema { schema } }
    }
}
```

Extend `ProviderToolDefinition` and `MessagesRequest`:

```rust
pub struct ProviderToolDefinition {
    pub name: ProviderFunctionName,
    pub description: String,
    pub input_schema: JsonSchemaDocument,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strict: Option<bool>,
}

pub struct MessagesRequest {
    pub model: String,
    pub messages: Vec<ProviderMessage>,
    pub system: String,
    pub max_tokens: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ProviderToolDefinition>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_config: Option<OutputConfig>,
    pub thinking: ThinkingConfig,
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<RequestMetadata>,
}

impl ProviderToolDefinition {
    pub fn with_strict(mut self) -> Self {
        self.strict = Some(true);
        self
    }
}
```

Initialize `strict: None` in `ProviderToolDefinition::new()` and `output_config: None` at all request-construction call sites found by `rg "MessagesRequest \{"`.

- [ ] **Step 4: Implement a pure canonical-to-Anthropic projection**

Implement this public, side-effect-free function in `provider-wire`:

```rust
pub fn project_anthropic_json_schema(canonical: Value) -> Result<JsonSchemaDocument, WireError>
```

Rules:

1. Require an object root; reject boolean schemas and non-object roots.
2. Preserve property names, `required`, `enum`, `const`, `$defs`, `$ref`, `items`, and union branches.
3. Remove `$schema` and these grammar-unsupported validation keywords recursively: `minimum`, `maximum`, `exclusiveMinimum`, `exclusiveMaximum`, `multipleOf`, `minLength`, `maxLength`, `pattern`, `format`, `minItems`, `maxItems`, `uniqueItems`, `minProperties`, and `maxProperties`.
4. For every object-shaped schema that has declared `properties`, insert `"additionalProperties": false` when it is absent.
5. If an object-shaped schema explicitly permits arbitrary keys (`additionalProperties: true` or schema-valued `additionalProperties`), return `WireError::UnsupportedStructuredOutputSchema` rather than silently narrowing its canonical meaning.
6. Do not alter the canonical schema or its hash. The projection is an optimization/transport grammar; `validate_canonical_value` remains authoritative after response receipt.

Add explicit `WireError::UnsupportedStructuredOutputSchema` and `WireError::RequestFootprintOverflow` variants while adding this helper. Neither error may contain a schema body, provider error body, or prompt text.

Use one recursive walker over only schema-bearing keys (`properties`, `$defs`, `items`, `allOf`, `anyOf`, `oneOf`, `not`, `if`, `then`, `else`). Do not recursively mutate arbitrary `examples` or enum values.

- [ ] **Step 5: Add projection tests that protect semantics**

Add these exact cases:

```rust
#[test]
fn projection_removes_unsupported_constraints_but_preserves_required_and_enum() {
    let canonical = serde_json::json!({
        "type": "object",
        "required": ["label", "score"],
        "properties": {
            "label": {
                "type": "string",
                "enum": ["HIGH", "LOW"],
                "maxLength": 8,
                "pattern": "[A-Z]+"
            },
            "score": {"type": "number", "minimum": 1}
        }
    });

    let projected = project_anthropic_json_schema(canonical).unwrap();
    assert_eq!(projected.as_value()["required"], serde_json::json!(["label", "score"]));
    assert_eq!(projected.as_value()["properties"]["label"]["enum"], serde_json::json!(["HIGH", "LOW"]));
    assert!(projected.as_value()["properties"]["label"].get("maxLength").is_none());
    assert!(projected.as_value()["properties"]["label"].get("pattern").is_none());
    assert!(projected.as_value()["properties"]["score"].get("minimum").is_none());
}

#[test]
fn projection_adds_closed_object_boundary_when_safe() {
    let projected = project_anthropic_json_schema(serde_json::json!({
        "type": "object",
        "properties": {"event": {"type": "string"}}
    }))
    .unwrap();
    assert_eq!(projected.as_value()["additionalProperties"], serde_json::json!(false));
}

#[test]
fn projection_rejects_open_ended_map_schema() {
    let error = project_anthropic_json_schema(serde_json::json!({
        "type": "object",
        "additionalProperties": true
    }))
    .unwrap_err();
    assert!(matches!(error, WireError::UnsupportedStructuredOutputSchema));
}

#[test]
fn projection_is_canonical_and_hash_stable() {
    let canonical = serde_json::json!({
        "type": "object",
        "properties": {"event": {"type": "string"}}
    });
    let first = project_anthropic_json_schema(canonical.clone()).unwrap();
    let second = project_anthropic_json_schema(canonical.clone()).unwrap();
    assert_eq!(canonical, serde_json::json!({
        "type": "object",
        "properties": {"event": {"type": "string"}}
    }));
    assert_eq!(serde_jcs::to_vec(first.as_value()).unwrap(), serde_jcs::to_vec(second.as_value()).unwrap());
    assert_eq!(
        ContentHash::sha256(serde_jcs::to_vec(first.as_value()).unwrap()),
        ContentHash::sha256(serde_jcs::to_vec(second.as_value()).unwrap())
    );
}
```

- [ ] **Step 6: Add a full-request footprint helper**

Add a helper that measures the actual outbound JSON, including `system`, `messages`, tools, `output_config`, and metadata.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderRequestFootprint {
    pub canonical_bytes: usize,
    /// Conservative transport-independent upper bound: one token per UTF-8 byte.
    pub input_tokens_upper_bound: u64,
}

pub fn provider_request_footprint(
    request: &MessagesRequest,
) -> Result<ProviderRequestFootprint, WireError> {
    let bytes = serde_jcs::to_vec(request)?;
    Ok(ProviderRequestFootprint {
        canonical_bytes: bytes.len(),
        input_tokens_upper_bound: u64::try_from(bytes.len())
            .map_err(|_| WireError::RequestFootprintOverflow)?,
    })
}
```

This intentionally does not call a remote tokenizer endpoint and does not claim to be billed-token exact.

- [ ] **Step 7: Run provider-wire tests**

Run:

```bash
cargo test -p krw-agent-provider-wire --lib
```

Expected: schema projection is deterministic; ordinary current-provider requests omit every new wire field.

- [ ] **Step 8: Commit only provider-wire API and constructor changes**

```bash
git add crates/provider-wire/src/lib.rs bins/krw-agent/src/main.rs \
  crates/run-engine/src/lib.rs crates/perf-harness/src/main.rs
git commit -m "feat(provider-wire): model structured output constraints"
```

Do not stage unrelated changes in the same files.

---

## Task 3: Compile one output-schema projection per typed state

**Files:**

- Modify: `crates/context-planner/src/lib.rs:118-143, 242-280, 496-629, 761-803`
- Test: `crates/context-planner/src/lib.rs`

**Interfaces:**

- Consumes: `StateOperation::ModelDecision`, `ModelOutputMode::TypedJson`, canonical `ContractPin`, and `project_anthropic_json_schema`.
- Produces: `CompiledStateContext.provider_output_schema: Option<ProviderOutputSchemaRef>` and a receipt/plan hash that changes when the provider projection changes.

- [ ] **Step 1: Write context-planner tests for typed and Markdown states**

Use existing compiled agent fixtures. Assert that a known TypedJson state (for example `krw-router` or `krw-display`) gets one projected schema, while `krw-ontology`’s Markdown composer has none.

```rust
assert!(router_plan.provider_output_schema.is_some());
assert!(markdown_plan.provider_output_schema.is_none());
```

Add a test that two independently compiled plans for the same TypedJson state produce equal canonical-contract and projection hashes.

- [ ] **Step 2: Run the focused planner test to verify it fails**

Run:

```bash
cargo test -p krw-context-planner --lib provider_output_schema
```

Expected: compilation fails because `provider_output_schema` does not exist.

- [ ] **Step 3: Add a compact projected-schema reference**

Add the following type near `CapabilitySchemaRef`:

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderOutputSchemaRef {
    pub output_contract_id: String,
    pub canonical_schema_hash: ContentHash,
    pub projected_schema_hash: ContentHash,
    pub projected_schema: JsonSchemaDocument,
}
```

Add `provider_output_schema: Option<ProviderOutputSchemaRef>` to `CompiledStateContext`. Import `ModelOutputMode`, `StateOperation`, and `JsonSchemaDocument` where needed.

- [ ] **Step 4: Compile the projection at startup, not in the hot run loop**

In `compile_state_context`, inspect `state.operation`:

```rust
let provider_output_schema = match &state.operation {
    StateOperation::ModelDecision {
        output_mode: ModelOutputMode::TypedJson,
        output_contracts,
        ..
    } => Some(build_provider_output_schema(output_contracts)?),
    _ => None,
};
```

`build_provider_output_schema` must require exactly one output contract, call `verify_pin`, fetch the contract with `canonical_contract`, assert the canonical schema hash matches the pin, project it with `project_anthropic_json_schema`, and calculate its JCS hash. An unsupported projection is a startup `ContextPlanError`: all current canonical TypedJson contracts must be projectable before this release lands. This is a release-time failure, not a model-facing fallback or a runtime prompt failure.

- [ ] **Step 5: Bind the projection identity into planning receipts**

Add an optional projected-schema hash to both `PlanHashInput` and `PromptAssemblyReceipt`. Include it in `verify_receipt` comparisons and increment `PROMPT_ASSEMBLY_RECEIPT_SCHEMA_VERSION`.

Do not serialize the full schema into durable receipts. The immutable canonical contract pin plus projected schema hash is enough to detect drift without retaining duplicate payloads.

- [ ] **Step 6: Add negative tests**

Create an in-memory `AgentImageManifest` fixture with a TypedJson state that references either zero or two output contracts. Assert `ContextPlanner::compile` returns a `ContextPlanError` before any provider request can be created.

- [ ] **Step 7: Run context-planner tests**

Run:

```bash
cargo test -p krw-context-planner --lib
```

Expected: all agent images compile; only TypedJson states hold a projected schema; a changed projection changes the plan receipt hash.

- [ ] **Step 8: Commit the startup compilation boundary**

```bash
git add crates/context-planner/src/lib.rs
git commit -m "feat(context): precompile typed output schema projections"
```

---

## Task 4: Encode constraints conditionally and keep repair semantic-only

**Files:**

- Modify: `crates/run-engine/src/lib.rs:1789-2006, 3361-3488, 7789-8067, 8183-8201, 8278-8299, 13526-13556`
- Test: `crates/run-engine/src/lib.rs`

**Interfaces:**

- Consumes: `CompiledStateContext.provider_output_schema`, `ProviderWireModeCapabilities`, `OutputConfig`, and `ProviderRequestFootprint`.
- Produces: an exact constrained request only in admitted modes, a receipt hash bound to constraint mode/schema, and non-repairable provider-contract errors for violated constrained syntax.

- [ ] **Step 1: Write encoding tests using a synthetic admitted capability matrix**

Do not change deployment configuration for this test. Construct a local `ProviderWireCapabilities` copy with `supports_json_schema_output: true` and test only `encode_provider_output_channel`/request building.

```rust
let mut constrained = ProviderWireCapabilities::default();
constrained.non_thinking = ProviderWireModeCapabilities {
    non_thinking: ProviderWireModeCapabilities {
        supported: true,
        supports_tools: true,
        supports_tool_choice: true,
        supports_json_object: true,
        supports_json_schema_output: true,
        supports_strict_tool_input: true,
};
```

Assert all of the following:

1. A TypedJson request gets `output_config.format.type == "json_schema"` only with the synthetic capability.
2. The shipped DeepSeek/GLM constants still produce `output_config: None`; this is a static assertion and does not call either provider.
3. A plain Markdown state never gets an output schema.
4. A workflow-transition-only state marks only `krw_agent_transition` strict when the synthetic capability supports strict tools.
5. The tool schema hash and prompt receipt hash change when strictness or output-schema projection changes.

- [ ] **Step 2: Run the focused encoding test to verify it fails**

Run:

```bash
cargo test -p krw-agent-run-engine --lib structured_output_encoding
```

Expected: compilation fails until the output encoding includes the new fields.

- [ ] **Step 3: Expand the provider output encoding without adding workflow meaning**

Replace the one-field encoding with this representation:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum ProviderConstraintMode {
    None,
    PromptedJson,
    JsonSchema,
}

#[derive(Debug, Clone)]
struct ProviderWireOutputEncoding {
    tool_choice: Option<ToolChoice>,
    output_config: Option<OutputConfig>,
    constraint_mode: ProviderConstraintMode,
    strict_transition_tool: bool,
}
```

Pass `context.provider_output_schema.as_ref()` into `encode_provider_output_channel`. Its TypedJson branch must behave exactly as follows:

```rust
if mode.supports_json_schema_output {
    let schema = output_schema.ok_or(EngineError::Invariant(
        "typed JSON context lacks a precompiled provider schema",
    ))?;
    Ok(ProviderWireOutputEncoding {
        tool_choice: None,
        output_config: Some(OutputConfig::json_schema(schema.projected_schema.clone())),
        constraint_mode: ProviderConstraintMode::JsonSchema,
        strict_transition_tool: false,
    })
} else if mode.supports_json_object {
    Ok(ProviderWireOutputEncoding {
        tool_choice: None,
        output_config: None,
        constraint_mode: ProviderConstraintMode::PromptedJson,
        strict_transition_tool: false,
    })
} else {
    Err(EngineError::ProviderWireFeatureUnavailable("JSON object output"))
}
```

`Markdown` returns `None`; it must not acquire a JSON constraint merely because a provider supports one.

For `ModelOutputMode::WorkflowTransition`, retain its existing forced tool-choice behavior and set `strict_transition_tool` to `mode.supports_strict_tool_input`. For `CapabilityCall` and `CapabilityOrWorkflowTransition`, leave `strict_transition_tool` false unless the chosen output disposition is the kernel transition tool. This makes strictness an endpoint capability plus a single kernel-owned tool decision, never a model-selected option.

- [ ] **Step 4: Apply strictness only to the kernel transition tool**

Change `workflow_transition_tool_definition` to accept `strict: bool`, build the existing closed `{event enum}` schema, and finish with `.with_strict()` only when requested.

```rust
fn workflow_transition_tool_definition(
    allowed_events: &[String],
    strict: bool,
) -> Result<ProviderToolDefinition, EngineError> {
    if allowed_events.is_empty() {
        return Err(EngineError::WorkflowResolution {
            outcome: "typed transition frontier",
        });
    }
    let mut definition = ProviderToolDefinition::new(
        WORKFLOW_TRANSITION_TOOL_NAME,
        "Select exactly one allowed workflow transition. This function is a local kernel control and performs no external action; the kernel derives all state facts from durable evidence and the pinned execution contract.",
        serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["event"],
            "properties": {"event": {"type": "string", "enum": allowed_events}}
        }),
    )?;
    if strict {
        definition = definition.with_strict();
    }
    Ok(definition)
}
```

Do not mark capability tools strict in this release. Their schema projection and provider admission are a separate, measurable follow-up; keeping them locally validated avoids an all-tools grammar limit or a mixed-provider compatibility surprise.

- [ ] **Step 5: Bind constraint identity into the dynamic prompt receipt**

Add these fields to `DynamicProviderPromptReceipt`:

```rust
provider_constraint_mode: ProviderConstraintMode,
provider_output_schema_hash: Option<&ContentHash>,
```

Populate the hash only for `JsonSchema`. Calculate `tool_schema_hash` after adding the strict transition flag. This ensures a recovery replay never treats a non-strict request as the same provider decision as a strict one.

- [ ] **Step 6: Refactor final TypedJson validation into syntax and semantics**

In `finish`, pass `built.constraint_mode` into the final-output path. Split the current TypedJson logic into:

1. Parse JSON and run `validate_canonical_value` exactly once.
2. Run product linkage, fixed-author checks, evidence/calculation checks, and answer-rule checks.

When `constraint_mode == JsonSchema`:

- JSON parse failure or canonical schema failure returns `EngineError::ProviderConstrainedOutputViolation` immediately.
- `finish_reason == "length"`, refusal, or empty final content returns `EngineError::ProviderConstrainedOutputIncomplete` immediately.
- Do not call `reserve_repair` for those provider-contract failures.

For `PromptedJson`, retain the current single bounded syntax/schema repair. For both modes, semantic failure still uses the existing bounded answer-repair path.

Make `validate_typed_output_semantics` contain only checks that JSON Schema cannot prove; remove its duplicate `validate_canonical_value` call.

- [ ] **Step 7: Add recovery-behavior tests**

Add two `#[tokio::test]` cases beside `router_finishes_with_its_non_answer_ir_contract` using `router_fixture()` and `ScriptedProvider`:

1. `constrained_invalid_json_is_not_reprompted` sets the fixture’s non-thinking synthetic capability to `supports_json_schema_output: true`, scripts final content `{`, and asserts `EngineError::ProviderConstrainedOutputViolation`, exactly one provider request, and no second repair request.
2. `constrained_schema_valid_but_ungrounded_answer_uses_one_semantic_repair` first scripts a `routing-decision/v2` value with a valid `sha256:`-formatted but wrong `input_hash`; it then scripts the same decision with the fixture request’s actual input hash. Assert successful completion after exactly two provider requests. Inspect the second request to prove it carries the bounded semantic/product-linkage repair directive and does not carry the raw first response or a JSON-shape repair directive.

- [ ] **Step 8: Run run-engine unit tests**

Run:

```bash
cargo test -p krw-agent-run-engine --lib
```

Expected: the GLM scripted fixture retains prompted JSON behavior; a static DeepSeek capability assertion remains disabled; synthetic constrained mode has the exact wire request and no format-repair loop. No live provider call is made by this test.

- [ ] **Step 9: Commit the conditional encoding change**

```bash
git add crates/run-engine/src/lib.rs
git commit -m "feat(run-engine): gate structured outputs by provider capability"
```

---

## Task 5: Preflight the full serialized request without inventing a token-count failure

**Files:**

- Modify: `crates/run-engine/src/lib.rs:1477-1523, 7938-8067, 9181-9268`
- Test: `crates/run-engine/src/lib.rs`
- Test: `crates/runtime-persistence/src/claim.rs`

**Interfaces:**

- Consumes: `provider_request_footprint`, `EngineConfig.max_conversation_bytes`, `ResolvedExecutionSnapshot.provider_max_context_tokens`, and the selected turn’s `max_tokens`.
- Produces: the existing numeric `EngineError::SizeLimit { resource: "provider_request" }` before `Provider::complete` is invoked when the complete wire body exceeds its configured byte cap. The pinned provider context limit is used for exact output-limit validation and diagnostics, not for a false-precise input-token rejection.

- [ ] **Step 1: Write a full-body preflight test**

Create a request where message text alone fits a tiny synthetic `max_conversation_bytes` limit but the same request plus a tool definition or `output_config` does not. Assert the existing numeric size error is raised before the scripted provider receives a request.

```rust
assert!(matches!(
    preflight_provider_request_size(&request, 256),
    Err(EngineError::SizeLimit {
        resource: "provider_request",
        observed: _,
        limit: 256,
    })
));
```

Also test equality at the byte boundary: `canonical_bytes == max_conversation_bytes` is accepted. Add a separate invariant test that rejects `request.max_tokens > provider_max_context_tokens`; this is an exact provider limit check and does not estimate prompt tokens.

- [ ] **Step 2: Run the focused preflight test to verify it fails**

Run:

```bash
cargo test -p krw-agent-run-engine --lib provider_request_size
```

Expected: compilation fails because the full-body preflight function does not exist.

- [ ] **Step 3: Add a pure run-engine preflight function**

Add this helper near request construction:

```rust
fn preflight_provider_request_size(
    request: &MessagesRequest,
    max_request_bytes: usize,
) -> Result<ProviderRequestFootprint, EngineError> {
    let footprint = provider_request_footprint(request)?;
    ensure_size(
        footprint.canonical_bytes,
        max_request_bytes,
        "provider_request",
    )?;
    Ok(footprint)
}
```

The byte count is exact for the outbound JSON payload. Do not equate `input_tokens_upper_bound` with actual provider tokens: tokenizers vary by provider and UTF-8 byte length would create false rejections, especially for Korean. Do not call a remote `/count_tokens` API in the correctness path and do not use the measurement to retroactively charge billing usage.

- [ ] **Step 4: Invoke preflight after the complete request is built**

Replace the current messages-only `ensure_size(serde_jcs::to_vec(&messages))` check with this helper after `MessagesRequest` is fully constructed, including tool definitions and `output_config`, but before `BuiltProviderRequest` returns. This counts the complete outbound body exactly once. Use the existing engine byte cap:

```rust
let footprint = preflight_provider_request_size(
    &request,
    config.max_conversation_bytes,
)?;
```

Before building the request, reject `max_tokens > input.snapshot.provider_max_context_tokens` with a numeric invariant/configuration error. Store only `canonical_bytes`, `input_tokens_upper_bound`, `max_tokens`, and the pinned context limit in tracing/diagnostics. Do not persist prompt text or raw request bodies.

- [ ] **Step 5: Define safe failure behavior**

Do not add `ProviderRequestContextExceeded`: a full request’s byte count is not an exact token count. Keep the existing `SizeLimit` failure for an oversized full wire body and do not map it through `model_recovery_directive`; asking the model to repair a request it never saw cannot resolve a deterministic byte limit. The orchestration layer can only resume after its existing safe compaction boundary produces a smaller state; it must not automatically create a new summarization model call here.

Keep post-response `BudgetUsage.input_tokens` enforcement unchanged. If a future provider needs hard input-token context rejection, add it only with a pinned local tokenizer and an endpoint-admission test showing that its count is exact for that endpoint/model/mode.

- [ ] **Step 6: Update snapshot and claim tests**

Extend claim validation tests to prove a snapshot with `provider_max_context_tokens == 0` is rejected and that the model descriptor’s context limit is copied exactly into the resolved snapshot.

- [ ] **Step 7: Run focused tests**

Run:

```bash
cargo test -p krw-agent-run-engine --lib provider_request_size
cargo test -p krw-agent-runtime-persistence --lib claim
```

Expected: the provider call count remains zero for an oversized full byte request; a boundary-sized request reaches the scripted provider normally. No current provider receives a new hard input-token heuristic rejection.

- [ ] **Step 8: Commit request admission independently**

```bash
git add crates/run-engine/src/lib.rs crates/runtime-persistence/src/claim.rs
git commit -m "feat(run-engine): preflight full provider request size"
```

---

## Task 6: Add bounded provider diagnostics and align documentation

**Files:**

- Modify: `crates/provider-wire/src/lib.rs:1125-1219, 1273-1375`
- Modify: `crates/run-engine/src/lib.rs` failure classification helpers
- Modify: `bins/krw-agent/src/main.rs:183-248, 566-641`
- Modify: `README.md:3-185`
- Modify: `docs/IMPLEMENTATION_STATUS.md:1-220`
- Test: `crates/provider-wire/src/lib.rs`
- Test: `bins/krw-agent/tests/release_cli.rs`

**Interfaces:**

- Produces: bounded `Retry-After` behavior, redacted request-ID hashes in provider failures, and an operator-readable report that distinguishes prompted JSON from schema-constrained JSON.

- [ ] **Step 1: Write pure header-parsing tests**

Add unit tests for a delta-seconds `Retry-After` parser and a safe request-ID hash extractor. The tests must prove that invalid, negative, date-form, overlong, or control-character values are discarded.

```rust
assert_eq!(parse_retry_after_ms("2"), Some(2_000));
assert_eq!(parse_retry_after_ms("999999"), None);
assert_eq!(safe_request_id_hash("req_abc-123").is_some(), true);
assert_eq!(safe_request_id_hash("bad\nvalue"), None);
```

- [ ] **Step 2: Run the focused provider-wire diagnostics tests to verify they fail**

Run:

```bash
cargo test -p krw-agent-provider-wire --lib retry_after
```

Expected: compilation fails because the parsing helpers do not exist.

- [ ] **Step 3: Preserve only bounded, redacted response metadata**

Before dropping a retryable response, read only:

- `Retry-After` as a bounded delta-seconds delay of 1–8,000 ms;
- `request-id` and `x-request-id` as printable ASCII identifiers no longer than 128 bytes, then store only `ContentHash::sha256(value)`.

Extend `WireError::ApiStatus`:

```rust
ApiStatus {
    status: u16,
    body_prefix_hash: ContentHash,
    provider_code: Option<String>,
    request_id_hash: Option<ContentHash>,
    retry_after_ms: Option<u64>,
}
```

Use a valid server retry delay instead of local jitter only when it is inside the existing 1–8 second ceiling. Never retain header values or human-readable error bodies.

- [ ] **Step 4: Keep retry classification unchanged**

Update exhaustive `WireError` match sites in `run-engine` without widening retries. The only retryable statuses remain the current pre-byte 429/500/503 and connect/timeout failures. A 400 schema rejection, structured-output refusal, or post-byte stream failure remains non-retryable.

- [ ] **Step 5: Extend the fixed provider probe report, not the LLM surface**

Add a `constraint_capabilities` object to `ProviderProbeReport` that reports only the selected GLM model/mode booleans and configured API version. Change the live probe implementation from `probe_deepseek_flash`/`DEEPSEEK_API_KEY` to a fixed GLM-5.2 probe using `GLM_API_KEY` and the configured z.ai Anthropic base. Do not add model-authored diagnostics or a generic plugin doctor in this task.

```json
{
  "prompted_json_object": true,
  "json_schema_output": false,
  "strict_tool_input": false
}
```

The baseline live probe remains fixed-prompt, no-tool, and read-only. Add a separate explicit GLM admission probe in the same CLI module that sends two machine-built requests: one `output_config.format.type = "json_schema"` request and one request containing only the `krw_agent_transition` tool with `strict: true`. It must report each result separately, never enable a flag automatically, and never send either probe to DeepSeek.

- [ ] **Step 6: Rewrite stale documentation around the actual wire**

Update `README.md` and `docs/IMPLEMENTATION_STATUS.md` to say:

- the runtime currently supports DeepSeek V4 Flash and GLM-5.2 through Anthropic-compatible Messages endpoints;
- `TypedJson` currently falls back to prompt plus local validation for those profiles;
- exact JSON Schema output and strict tool input are capability-gated and disabled for both profiles until the GLM-only admission probe is accepted;
- the current operator command is `krw-agent provider probe`, not the historical unimplemented global `doctor` command.

Do not copy historical `DEVELOPER_EXPERIENCE.md` commands into current documentation; that document identifies itself as superseded.

- [ ] **Step 7: Run diagnostics and CLI tests**

Run:

```bash
cargo test -p krw-agent-provider-wire --lib
cargo test -p krw-agent --test release_cli
```

Expected: header parsing is bounded/redacted, existing request error classification remains stable, and the probe JSON includes truthful constraint flags.

- [ ] **Step 8: Commit diagnostics and documentation separately**

```bash
git add crates/provider-wire/src/lib.rs crates/run-engine/src/lib.rs \
  bins/krw-agent/src/main.rs bins/krw-agent/tests/release_cli.rs \
  README.md docs/IMPLEMENTATION_STATUS.md
git commit -m "feat(provider): expose bounded contract diagnostics"
```

---

## Task 7: Run the release gates and admit no provider automatically

**Files:**

- Modify: none. Source and fixture version changes belong to the task that introduced the contract-version change.
- Test: workspace unit tests, formatting, clippy, recorded quality replay.

**Interfaces:**

- Consumes: completed Tasks 1–6.
- Produces: a verified capability-gated implementation with DeepSeek disabled and GLM disabled until its explicit live admission evidence is accepted.

- [ ] **Step 1: Run focused crate tests first**

Run:

```bash
cargo test -p krw-agent-protocol --lib
cargo test -p krw-agent-provider-wire --lib
cargo test -p krw-context-planner --lib
cargo test -p krw-agent-run-engine --lib
cargo test -p krw-agent-runtime-config --lib
cargo test -p krw-agent-runtime-persistence --lib
```

- [ ] **Step 2: Run formatting and lint gates**

Run:

```bash
cargo fmt --check
cargo clippy --workspace -- -D warnings
```

- [ ] **Step 3: Run deterministic replay quality gates**

Run the existing recorded fixture suite only; do not use a provider credential or live provider call for this regression gate.

```bash
cargo run -p krw-agent -- quality check --suite evals/krw-research-quality/v4 --root .
cargo run -p krw-agent -- quality replay --suite evals/krw-research-quality/v4 --case company-research-cash-generation --root .
cargo run -p krw-agent -- quality replay --suite evals/krw-research-quality/v4 --case company-research-independent-cash-and-debt --root .
cargo run -p krw-agent -- quality replay --suite evals/krw-research-quality/v4 --case company-research-partial-evidence-selective-replan --root .
```

Expected: current deployments stay on prompt-plus-local-validation and produce the same recorded acceptance results unless a receipt/schema-version fixture is intentionally updated.

- [ ] **Step 4: Add a separate provider admission record before enabling a flag**

For the current phase, run this admission record for GLM-5.2 only; do not run a DeepSeek live probe. For every provider/model/mode admitted in a later phase, add a dedicated admission change containing:

1. the exact endpoint, API version, model ID, and thinking mode;
2. a fixed live probe that exercises one simple JSON Schema and, separately, the single `krw_agent_transition` strict tool;
3. expected behavior for refusal and `max_tokens`;
4. a recorded wire fixture and release hash;
5. an explicit update to the provider capability constant and matching registry YAML.

Do not enable a capability based on documentation alone or on an Anthropic-compatible endpoint name. A failed GLM probe leaves both GLM flags `false` and does not add a retry or repair path.

- [ ] **Step 5: Record the release-gate evidence without creating an empty commit**

```bash
git status --short
git diff --check
```

Record the exact commands and pass/fail output in the PR description. Do not create a source commit solely for command output; any remaining source or fixture diff must be assigned back to its owning Task 1–6 change before merge.

## Deferred Scope: OpenBB data-plane admission

OpenBB is intentionally excluded from this implementation plan. Its provider-normalization pattern is useful only after an evidence-quality evaluation identifies a concrete missing data class such as price history, consensus revisions, or macro series. It must be planned as a separate adapter project with these entry criteria:

1. A current acceptance fixture documents the answer gap and the unavailable canonical source.
2. A single semantic capability contract is selected (for example, `market.snapshot/v1`), rather than exposing raw OpenBB commands to the model.
3. Provider choice, fallback, source freshness, and degradation stay kernel-owned and are recorded in `EvidenceLedger` provenance.
4. Licensing, authentication, data-release hash, and freshness policy are explicitly approved.

That follow-up must not adopt OpenBB MCP’s model-driven browse/activate tool flow, and it must not expand the current closed capability frontier without an evaluation showing that the new source changes answer quality.

## Plan Self-Review

- **Structured Outputs:** Tasks 1–4 add explicit capability facts, the provider wire representation, startup schema projection, receipt binding, exact constrained behavior, and preserved semantic validation.
- **Failure-surface control:** Tasks 4–5 eliminate format repair in constrained mode, prohibit a new model-driven compaction loop, and reject only a deterministically oversized serialized request before network dispatch.
- **Operations:** Task 6 adds only bounded/redacted provider diagnostics and a GLM-only admission probe; it does not import claw-code’s plugin/session/subagent surface.
- **OpenBB:** The deferred gate prevents a data-source expansion without measured need, source policy, and a typed semantic capability.
- **No placeholder scan:** Every implementation task names files, interfaces, exact conditions, expected test behavior, and a scoped commit. GLM admission is deliberately a separate decision; DeepSeek has no live test in this phase.
