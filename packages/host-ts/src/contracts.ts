import type { ContentHash, JsonObject, JsonValue } from "./json.js";

export const AGENT_V1_ABI_VERSION = 1 as const;
/** Must match Rust `krw_protocol::PROTOCOL_VERSION`; release entrypoints pin it. */
export const KRW_PROTOCOL_VERSION = 7 as const;
/** Must match Rust `krw_protocol::CLAIM_PAYLOAD_SCHEMA_VERSION`. */
export const CLAIM_SCHEMA_VERSION = 7 as const;
export const RELEASE_DESCRIPTOR_SCHEMA_VERSION = 3 as const;
export const SESSION_MEMORY_CARRIER_SCHEMA_VERSION = 3 as const;
export const GLM_PROVIDER_API_VERSION = "anthropic-messages-v1" as const;
export const GLM_MODEL_ID = "glm-5.2" as const;
export const GLM_MAX_CONTEXT_TOKENS = 204_800 as const;
export const ALLOWED_MODEL_IDS: readonly string[] = [GLM_MODEL_ID] as const;

export type ThinkingMode = "enabled" | "disabled";
export type ReasoningEffort = "high" | "max";
export type GlmModelProfile = "glm_high" | "glm_max" | "glm_direct";

export interface ProviderWireModeCapabilities extends JsonObject {
  readonly supported: boolean;
  readonly supports_tools: boolean;
  readonly supports_tool_choice: boolean;
  readonly supports_json_object: boolean;
  readonly supports_json_schema_output: boolean;
  readonly supports_strict_tool_input: boolean;
}

/** Pinned wire matrix; the host copies it but never chooses provider behavior. */
export interface ProviderWireCapabilities extends JsonObject {
  readonly thinking: ProviderWireModeCapabilities;
  readonly non_thinking: ProviderWireModeCapabilities;
  readonly requires_thinking_block_replay: boolean;
  readonly requires_assistant_content_for_tool_calls: boolean;
}

export interface BudgetLimits extends JsonObject {
  readonly max_provider_turns: number;
  readonly max_capability_calls: number;
  readonly max_replans: number;
  readonly max_repairs: number;
  readonly max_input_tokens: number;
  readonly max_output_tokens: number;
  readonly max_evidence_bytes: number;
  readonly deadline_ms: number;
  readonly capability_call_limits: Readonly<Record<string, number>>;
}

export type NotebookUpdateMode =
  | "merge-research-answer"
  | "extract-open-questions"
  | "append-note"
  | "reorganize-notebook";

export interface CommittedAnswerSourceV1 extends JsonObject {
  readonly schema_version: 1;
  readonly final_receipt_hash: ContentHash;
  readonly answer_bundle_hash: ContentHash;
  readonly answer_ir_hash: ContentHash;
  readonly canonical_source_hash: ContentHash;
  readonly source_unit_hashes: Readonly<Record<string, ContentHash>>;
  readonly canonical_source: JsonValue;
}

export interface SessionMemoryCarrierV3 extends JsonObject {
  readonly schema_version: typeof SESSION_MEMORY_CARRIER_SCHEMA_VERSION;
  readonly source_revision: number;
  readonly view_hash: ContentHash;
  readonly source_frontier_hash: ContentHash;
  readonly canonical_view: JsonValue;
}

export type RunContextV1 =
  | { readonly kind: "company_ticker_set"; readonly tickers: readonly string[] }
  | { readonly kind: "covered_universe"; readonly universe: "covered" }
  | { readonly kind: "selected_feed_items"; readonly feed_item_ids: readonly string[] }
  | { readonly kind: "source_filing"; readonly filing_event_id: string }
  | {
      readonly kind: "research_notebook";
      readonly ticker: string;
      readonly input_hash: ContentHash;
      readonly typed_input: JsonValue;
    }
  | {
      readonly kind: "existing_answer";
      readonly committed_source: CommittedAnswerSourceV1;
    }
  | {
      readonly kind: "routing_request";
      readonly input_hash: ContentHash;
      readonly typed_input: JsonValue;
    }
  | { readonly kind: "question_only" };

export type RunContextKind = RunContextV1["kind"];

export interface RunRequest extends JsonObject {
  readonly run_id: string;
  readonly session_id: string;
  readonly tenant_id: string;
  readonly principal_id: string;
  readonly run_kind: string;
  readonly locale: string;
  readonly question: string;
  readonly requested_model: string;
  readonly model_profile: string;
  readonly budget: BudgetLimits;
  readonly context: RunContextV1;
  readonly session_memory: SessionMemoryCarrierV3 | null;
}

export interface PinnedExecutionContract extends JsonObject {
  readonly protocol_version: typeof KRW_PROTOCOL_VERSION;
  readonly agent_image_hash: ContentHash;
  readonly deployment_binding_hash: ContentHash;
  readonly model_registry_hash: ContentHash;
  readonly budget_registry_hash: ContentHash;
  readonly model_profile: string;
  readonly requested_model: string;
  readonly resolved_model: string;
  readonly provider_api_version: typeof GLM_PROVIDER_API_VERSION;
  readonly provider_max_context_tokens: typeof GLM_MAX_CONTEXT_TOKENS;
  readonly provider_wire_capabilities: ProviderWireCapabilities;
  readonly thinking: ThinkingMode;
  readonly reasoning_effort: ReasoningEffort | null;
  readonly capability_release_hashes: Readonly<Record<string, ContentHash>>;
  readonly budget: BudgetLimits;
}

export interface RunResourceProfileV1 extends JsonObject {
  readonly schema_version: typeof CLAIM_SCHEMA_VERSION;
  readonly workload_class: "read_only_interactive";
}

export interface ImmutableRunClaimV1 extends JsonObject {
  readonly schema_version: typeof CLAIM_SCHEMA_VERSION;
  readonly request: RunRequest;
  readonly execution: PinnedExecutionContract;
  readonly resource_profile: RunResourceProfileV1;
}

export interface EntrypointScopeV1 extends JsonObject {
  readonly context_kind: RunContextKind;
  readonly cardinality: "exact" | "max";
  readonly value: number;
}

/**
 * Public, secret-free deployment artifact generated from the same resolved
 * release set loaded by the daemon. The host selects an exact entry; it never
 * recreates model, budget, binding or capability resolution.
 */
export interface PublicReleaseEntrypoint extends JsonObject {
  readonly run_kind: string;
  readonly locale: string;
  readonly agent_image_hash: ContentHash;
  readonly model_profile: GlmModelProfile;
  readonly scope: EntrypointScopeV1;
  readonly execution: PinnedExecutionContract;
}

export interface PublicReleaseDescriptor extends JsonObject {
  readonly schema_version: typeof RELEASE_DESCRIPTOR_SCHEMA_VERSION;
  readonly release_set_hash: ContentHash;
  readonly runtime_version: string;
  readonly entries: readonly PublicReleaseEntrypoint[];
}

export interface HostRunInput {
  readonly mutationId: string;
  readonly runId: string;
  readonly tenantId: string;
  readonly principalId: string;
  readonly sessionId: string;
  readonly runKind: string;
  readonly locale: string;
  readonly question: string;
  readonly context: RunContextV1;
  /** Host enqueue must be null; the fenced Rust worker resolves carrier v3. */
  readonly sessionMemory: null;
}

export interface EnqueueRunRequest extends JsonObject {
  readonly mutation_id: string;
  readonly run_id: string;
  readonly tenant_id: string;
  readonly principal_id: string;
  readonly session_id: string;
  readonly agent_image_hash: ContentHash;
  readonly runtime_version: string;
  readonly priority: 0;
  readonly immutable_snapshot_hash: ContentHash;
  readonly immutable_snapshot: ImmutableRunClaimV1;
  readonly resource_profile: RunResourceProfileV1;
  readonly budgets: BudgetLimits;
}

/** Exact JSON envelope accepted by `agent_v1.enqueue_run`. */
export interface EncodedEnqueueRunRequest extends EnqueueRunRequest {
  readonly abi_version: typeof AGENT_V1_ABI_VERSION;
  readonly mutation_hash: ContentHash;
}

export interface EnqueueRunResponse {
  readonly outcome: "enqueued" | "already_enqueued";
  readonly run_id: string;
  readonly run_version: number;
}

export interface CancelRunInput {
  readonly mutationId: string;
  readonly runId: string;
  readonly tenantId: string;
  readonly reasonCode: string;
  readonly release: JsonValue;
}

export interface CancelRunResponse {
  readonly outcome: "cancelled" | "already_final" | "already_cancelled" | "already_failed";
  readonly run_id: string;
  readonly fencing_token: number;
  readonly run_version: number;
  readonly cancel_generation: number;
}

export interface ReadCommittedOutcomeInput {
  readonly runId: string;
  readonly tenantId: string;
}

export interface ReadCommittedOutcomeResponse {
  readonly run_id: string;
  readonly state: "queued" | "deferred" | "active" | "final" | "cancelled" | "failed";
  readonly fencing_token: number;
  readonly run_version: number;
  readonly cancel_generation: number;
  readonly terminal_outcome: JsonValue | null;
}

/** Host-only final Markdown, available only after an authenticated final run. */
export interface ReadFinalOutputResponse {
  readonly run_id: string;
  readonly final_output_hash: ContentHash;
  readonly markdown: string;
}

/**
 * Host-only final projection. Carries the rendered Markdown plus the public
 * ledger/usage counters required for product projection. Mirrors the SQL
 * return of `agent_v1.read_final_projection`, which verifies full ownership
 * (tenant+principal+session+run) before exposing anything.
 *
 * `memory_revision` and `memory_frontier_hash` are nullable today; the
 * current answer-bundle schema does not always populate them.
 */
export interface ReadFinalProjectionResponse {
  readonly run_id: string;
  readonly answer_bundle_hash: ContentHash;
  readonly final_output_hash: ContentHash;
  readonly markdown: string;
  readonly usage: JsonValue;
  readonly evidence_ledger_hash: ContentHash | null;
  readonly memory_revision: number | null;
  readonly memory_frontier_hash: ContentHash | null;
}

export interface OutboxReceipt {
  readonly outbox_id: number;
  readonly run_id: string;
  readonly event_kind: string;
  readonly dedupe_key: string;
  readonly payload: JsonValue;
  readonly delivery_attempts: number;
  readonly delivery_deadline: string;
}
