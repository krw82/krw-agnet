import {
  ALLOWED_MODEL_IDS,
  CLAIM_SCHEMA_VERSION,
  GLM_MAX_CONTEXT_TOKENS,
  GLM_PROVIDER_API_VERSION,
  KRW_PROTOCOL_VERSION,
  RELEASE_DESCRIPTOR_SCHEMA_VERSION,
  SESSION_MEMORY_CARRIER_SCHEMA_VERSION,
  type BudgetLimits,
  type CommittedAnswerSourceV1,
  type EntrypointScopeV1,
  type PublicReleaseDescriptor,
  type PublicReleaseEntrypoint,
  type RunContextV1,
  type SessionMemoryCarrierV3,
} from "./contracts.js";
import {
  canonicalHash,
  canonicalJson,
  isContentHash,
  type ContentHash,
  type JsonObject,
  type JsonValue,
} from "./json.js";

const UUID_PATTERN = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
const TICKER_PATTERN = /^[A-Z0-9][A-Z0-9.-]{0,31}$/;
const ID_PATTERN = /^[A-Za-z0-9][A-Za-z0-9._:/-]*$/;
const GLM_PROFILE_POLICY = {
  glm_high: { thinking: "enabled", reasoning_effort: "high" },
  glm_max: { thinking: "enabled", reasoning_effort: "max" },
  glm_direct: { thinking: "disabled", reasoning_effort: null },
} as const;
const MODEL_PROFILE_POLICY = GLM_PROFILE_POLICY;

export class ContractViolation extends Error {
  constructor(readonly code: string) {
    super(code);
    this.name = "ContractViolation";
  }
}

export function validateBoundedIdentifier(value: string, field: string, maxBytes = 128): void {
  if (
    value.length === 0 ||
    Buffer.byteLength(value, "utf8") > maxBytes ||
    value.includes("\0") ||
    !ID_PATTERN.test(value)
  ) {
    throw new ContractViolation(`invalid_${field}`);
  }
}

export function validateBudget(value: BudgetLimits): void {
  exactKeys(value, [
    "max_provider_turns",
    "max_capability_calls",
    "max_replans",
    "max_repairs",
    "max_input_tokens",
    "max_output_tokens",
    "max_evidence_bytes",
    "deadline_ms",
    "capability_call_limits",
  ]);
  boundedSafeInteger(value.max_provider_turns, 1, 65_535, "max_provider_turns");
  boundedSafeInteger(value.max_capability_calls, 1, 65_535, "max_capability_calls");
  boundedSafeInteger(value.max_replans, 0, 255, "max_replans");
  boundedSafeInteger(value.max_repairs, 0, 255, "max_repairs");
  boundedSafeInteger(value.max_input_tokens, 1, 0xffff_ffff, "max_input_tokens");
  boundedSafeInteger(value.max_output_tokens, 1, 0xffff_ffff, "max_output_tokens");
  boundedSafeInteger(
    value.max_evidence_bytes,
    1,
    Number.MAX_SAFE_INTEGER,
    "max_evidence_bytes",
  );
  boundedSafeInteger(value.deadline_ms, 1, Number.MAX_SAFE_INTEGER, "deadline_ms");
  if (!isPlainObject(value.capability_call_limits)) {
    throw new ContractViolation("invalid_capability_call_limits");
  }
  const entries = Object.entries(value.capability_call_limits);
  if (entries.length > 64) throw new ContractViolation("capability_call_limits_too_large");
  for (const [capability, limit] of entries) {
    validateBoundedIdentifier(capability, "capability_id");
    boundedSafeInteger(limit, 1, 65_535, "capability_call_limit");
    if (limit > value.max_capability_calls) {
      throw new ContractViolation("capability_limit_exceeds_total");
    }
  }
}

export function validateRunContext(value: RunContextV1): void {
  switch (value.kind) {
    case "company_ticker_set":
      exactKeys(value, ["kind", "tickers"]);
      boundedUniqueStrings(value.tickers, 1, 50, 32, TICKER_PATTERN, "tickers");
      break;
    case "covered_universe":
      exactKeys(value, ["kind", "universe"]);
      if (value.universe !== "covered") throw new ContractViolation("invalid_universe");
      break;
    case "selected_feed_items":
      exactKeys(value, ["kind", "feed_item_ids"]);
      boundedUniqueStrings(value.feed_item_ids, 1, 8, 36, UUID_PATTERN, "feed_item_ids");
      break;
    case "source_filing":
      exactKeys(value, ["kind", "filing_event_id"]);
      if (!UUID_PATTERN.test(value.filing_event_id)) {
        throw new ContractViolation("invalid_filing_event_id");
      }
      break;
    case "research_notebook":
      exactKeys(value, ["kind", "ticker", "input_hash", "typed_input"]);
      if (!TICKER_PATTERN.test(value.ticker)) throw new ContractViolation("invalid_ticker");
      requireHash(value.input_hash, "notebook_input_hash");
      validateTypedCarrier(value.typed_input, 1024 * 1024, "notebook_input");
      if (canonicalHash(value.typed_input) !== value.input_hash) {
        throw new ContractViolation("notebook_input_hash_mismatch");
      }
      break;
    case "existing_answer":
      exactKeys(value, ["kind", "committed_source"]);
      validateCommittedAnswerSource(value.committed_source);
      break;
    case "routing_request":
      exactKeys(value, ["kind", "input_hash", "typed_input"]);
      requireHash(value.input_hash, "routing_input_hash");
      validateTypedCarrier(value.typed_input, 256 * 1024, "routing_input");
      if (canonicalHash(value.typed_input) !== value.input_hash) {
        throw new ContractViolation("routing_input_hash_mismatch");
      }
      break;
    case "question_only":
      exactKeys(value, ["kind"]);
      break;
    default:
      throw new ContractViolation("unknown_run_context");
  }
}

export function validateSessionMemoryCarrier(value: SessionMemoryCarrierV3): void {
  exactKeys(value, [
    "schema_version",
    "source_revision",
    "view_hash",
    "source_frontier_hash",
    "canonical_view",
  ]);
  if (value.schema_version !== SESSION_MEMORY_CARRIER_SCHEMA_VERSION) {
    throw new ContractViolation("session_memory_version");
  }
  boundedSafeInteger(value.source_revision, 0, Number.MAX_SAFE_INTEGER, "source_revision");
  requireHash(value.view_hash, "session_memory_view_hash");
  requireHash(value.source_frontier_hash, "session_memory_frontier_hash");
  validateTypedCarrier(value.canonical_view, 256 * 1024, "session_memory_view");
  if (canonicalHash(value.canonical_view) !== value.view_hash) {
    throw new ContractViolation("session_memory_view_hash_mismatch");
  }
}

function validateCommittedAnswerSource(value: CommittedAnswerSourceV1): void {
  exactKeys(value, [
    "schema_version",
    "final_receipt_hash",
    "answer_bundle_hash",
    "answer_ir_hash",
    "canonical_source_hash",
    "source_unit_hashes",
    "canonical_source",
  ]);
  if (value.schema_version !== 1) throw new ContractViolation("committed_source_version");
  requireHash(value.final_receipt_hash, "final_receipt_hash");
  requireHash(value.answer_bundle_hash, "answer_bundle_hash");
  requireHash(value.answer_ir_hash, "answer_ir_hash");
  requireHash(value.canonical_source_hash, "canonical_source_hash");
  if (!isPlainObject(value.source_unit_hashes)) {
    throw new ContractViolation("invalid_source_unit_hashes");
  }
  const units = Object.entries(value.source_unit_hashes);
  if (units.length < 1 || units.length > 64) {
    throw new ContractViolation("source_unit_hashes_count");
  }
  for (const [unitId, hash] of units) {
    validateBoundedIdentifier(unitId, "source_unit_id");
    requireHash(hash, "source_unit_hash");
  }
  validateTypedCarrier(value.canonical_source, 1024 * 1024, "canonical_source");
  if (canonicalHash(value.canonical_source) !== value.canonical_source_hash) {
    throw new ContractViolation("canonical_source_hash_mismatch");
  }
}

export function validateReleaseDescriptor(value: PublicReleaseDescriptor): void {
  exactKeys(value, ["schema_version", "release_set_hash", "runtime_version", "entries"]);
  if (value.schema_version !== RELEASE_DESCRIPTOR_SCHEMA_VERSION) {
    throw new ContractViolation("release_descriptor_version");
  }
  requireHash(value.release_set_hash, "release_set_hash");
  validateBoundedIdentifier(value.runtime_version, "runtime_version", 64);
  if (!Array.isArray(value.entries) || value.entries.length < 1 || value.entries.length > 256) {
    throw new ContractViolation("release_entries_count");
  }
  const routes = new Set<string>();
  for (const entry of value.entries) {
    validateReleaseEntrypoint(entry);
    const route = `${entry.run_kind}\u0000${entry.locale}`;
    if (routes.has(route)) throw new ContractViolation("duplicate_release_route");
    routes.add(route);
  }
}

function validateReleaseEntrypoint(value: PublicReleaseEntrypoint): void {
  exactKeys(value, ["run_kind", "locale", "agent_image_hash", "model_profile", "scope", "execution"]);
  validateBoundedIdentifier(value.run_kind, "run_kind");
  validateBoundedIdentifier(value.locale, "locale", 32);
  validateBoundedIdentifier(value.model_profile, "model_profile");
  requireHash(value.agent_image_hash, "agent_image_hash");
  validateScope(value.scope);

  const execution = value.execution;
  exactKeys(execution, [
    "protocol_version",
    "agent_image_hash",
    "deployment_binding_hash",
    "model_registry_hash",
    "budget_registry_hash",
    "model_profile",
    "requested_model",
    "resolved_model",
    "provider_api_version",
    "provider_max_context_tokens",
    "provider_wire_capabilities",
    "thinking",
    "reasoning_effort",
    "capability_release_hashes",
    "budget",
  ]);
  if (execution.protocol_version !== KRW_PROTOCOL_VERSION) {
    throw new ContractViolation("protocol_version");
  }
  if (execution.agent_image_hash !== value.agent_image_hash) {
    throw new ContractViolation("entry_image_mismatch");
  }
  validateBoundedIdentifier(execution.model_profile, "execution_model_profile");
  if (execution.model_profile !== value.model_profile) {
    throw new ContractViolation("entry_model_profile_mismatch");
  }
  for (const [field, hash] of [
    ["agent_image_hash", execution.agent_image_hash],
    ["deployment_binding_hash", execution.deployment_binding_hash],
    ["model_registry_hash", execution.model_registry_hash],
    ["budget_registry_hash", execution.budget_registry_hash],
  ] as const) {
    requireHash(hash, field);
  }
  validateBoundedIdentifier(execution.requested_model, "requested_model");
  validateBoundedIdentifier(execution.resolved_model, "resolved_model");
  if (!ALLOWED_MODEL_IDS.includes(execution.requested_model)) {
    throw new ContractViolation("model_not_allowlisted");
  }
  if (execution.requested_model !== execution.resolved_model) {
    throw new ContractViolation("silent_model_alias");
  }
  if (execution.provider_api_version !== GLM_PROVIDER_API_VERSION) {
    throw new ContractViolation("provider_api_version");
  }
  if (execution.provider_max_context_tokens !== GLM_MAX_CONTEXT_TOKENS) {
    throw new ContractViolation("provider_context_capacity");
  }
  validateProviderWireCapabilities(execution.provider_wire_capabilities);
  if (execution.thinking === "enabled") {
    if (execution.reasoning_effort !== "high" && execution.reasoning_effort !== "max") {
      throw new ContractViolation("thinking_requires_reasoning_effort");
    }
  } else if (execution.thinking === "disabled") {
    if (execution.reasoning_effort !== null) {
      throw new ContractViolation("non_thinking_forbids_reasoning_effort");
    }
  } else {
    throw new ContractViolation("thinking_mode");
  }
  const profilePolicy = MODEL_PROFILE_POLICY[value.model_profile as keyof typeof MODEL_PROFILE_POLICY];
  if (
    profilePolicy === undefined ||
    execution.thinking !== profilePolicy.thinking ||
    execution.reasoning_effort !== profilePolicy.reasoning_effort
  ) {
    throw new ContractViolation("model_profile_policy");
  }
  if (!isPlainObject(execution.capability_release_hashes)) {
    throw new ContractViolation("invalid_capability_release_hashes");
  }
  const capabilityReleases = Object.entries(execution.capability_release_hashes);
  if (capabilityReleases.length > 64) {
    throw new ContractViolation("capability_release_hashes_too_large");
  }
  for (const [capability, hash] of capabilityReleases) {
    validateBoundedIdentifier(capability, "capability_id");
    requireHash(hash, "capability_release_hash");
  }
  validateBudget(execution.budget);
}

function validateProviderWireCapabilities(value: unknown): void {
  exactKeys(value, [
    "thinking",
    "non_thinking",
    "requires_thinking_block_replay",
    "requires_assistant_content_for_tool_calls",
  ]);
  const record = value as Record<string, unknown>;
  for (const mode of [record.thinking, record.non_thinking]) {
    exactKeys(mode, [
      "supported",
      "supports_tools",
      "supports_tool_choice",
      "supports_json_object",
      "supports_json_schema_output",
      "supports_strict_tool_input",
    ]);
    for (const field of [
      "supported",
      "supports_tools",
      "supports_tool_choice",
      "supports_json_object",
      "supports_json_schema_output",
      "supports_strict_tool_input",
    ] as const) {
      if (typeof (mode as Record<string, unknown>)[field] !== "boolean") {
        throw new ContractViolation("provider_wire_capabilities");
      }
    }
  }
  if (
    typeof record.requires_thinking_block_replay !== "boolean" ||
    typeof record.requires_assistant_content_for_tool_calls !== "boolean"
  ) {
    throw new ContractViolation("provider_wire_capabilities");
  }
}

function validateScope(value: EntrypointScopeV1): void {
  exactKeys(value, ["context_kind", "cardinality", "value"]);
  if (!RUN_CONTEXT_KINDS.has(value.context_kind)) throw new ContractViolation("scope_context_kind");
  if (value.cardinality !== "exact" && value.cardinality !== "max") {
    throw new ContractViolation("scope_cardinality");
  }
  if (!Number.isSafeInteger(value.value) || value.value < 0 || value.value > 64) {
    throw new ContractViolation("scope_value");
  }
}

export function validateClaimShape(
  request: {
    readonly budget: BudgetLimits;
    readonly requested_model: string;
    readonly model_profile: string;
  },
  execution: PublicReleaseEntrypoint["execution"],
): void {
  if (
    execution.protocol_version !== KRW_PROTOCOL_VERSION ||
    !ALLOWED_MODEL_IDS.includes(request.requested_model) ||
    !ALLOWED_MODEL_IDS.includes(execution.requested_model) ||
    !ALLOWED_MODEL_IDS.includes(execution.resolved_model) ||
    execution.model_profile !== request.model_profile ||
    execution.requested_model !== request.requested_model ||
    execution.resolved_model !== request.requested_model ||
    canonicalHash(execution.budget) !== canonicalHash(request.budget)
  ) {
    throw new ContractViolation("claim_snapshot_mismatch");
  }
}

export function assertClaimSchemaVersion(value: number): asserts value is typeof CLAIM_SCHEMA_VERSION {
  if (value !== CLAIM_SCHEMA_VERSION) throw new ContractViolation("claim_schema_version");
}

function exactKeys(value: unknown, expected: readonly string[]): void {
  if (!isPlainObject(value)) throw new ContractViolation("expected_plain_object");
  const actual = Object.keys(value).sort();
  const wanted = [...expected].sort();
  if (actual.length !== wanted.length || actual.some((key, index) => key !== wanted[index])) {
    throw new ContractViolation("unknown_or_missing_field");
  }
}

function requireHash(value: unknown, field: string): asserts value is ContentHash {
  if (!isContentHash(value)) throw new ContractViolation(`invalid_${field}`);
}

function boundedSafeInteger(value: number, min: number, max: number, field: string): void {
  if (!Number.isSafeInteger(value) || value < min || value > max) {
    throw new ContractViolation(`invalid_${field}`);
  }
}

function boundedText(value: string, maxBytes: number, field: string): void {
  if (value.length === 0 || value.includes("\0") || Buffer.byteLength(value, "utf8") > maxBytes) {
    throw new ContractViolation(`invalid_${field}`);
  }
}

function boundedUniqueStrings(
  values: readonly string[],
  min: number,
  max: number,
  maxBytes: number,
  pattern: RegExp,
  field: string,
): void {
  if (values.length < min || values.length > max || new Set(values).size !== values.length) {
    throw new ContractViolation(`invalid_${field}_count`);
  }
  if (values.some((value) => Buffer.byteLength(value, "utf8") > maxBytes || !pattern.test(value))) {
    throw new ContractViolation(`invalid_${field}_value`);
  }
}

function sumUtf8Bytes(values: readonly string[]): number {
  return values.reduce((total, value) => total + Buffer.byteLength(value, "utf8"), 0);
}

function validateTypedCarrier(value: unknown, maxBytes: number, field: string): asserts value is JsonValue {
  const stack: Array<{ value: unknown; depth: number }> = [{ value, depth: 0 }];
  const seen = new WeakSet<object>();
  let visited = 0;
  while (stack.length > 0) {
    const current = stack.pop();
    if (!current) break;
    visited += 1;
    if (visited > 16_384 || current.depth > 32) {
      throw new ContractViolation(`${field}_shape_limit`);
    }
    const item = current.value;
    if (item === null || typeof item === "boolean") continue;
    if (typeof item === "number") {
      if (!Number.isFinite(item)) throw new ContractViolation(`${field}_number`);
      continue;
    }
    if (typeof item === "string") {
      if (item.includes("\0") || Buffer.byteLength(item, "utf8") > 1024 * 1024) {
        throw new ContractViolation(`${field}_string`);
      }
      continue;
    }
    if (typeof item !== "object" || seen.has(item)) {
      throw new ContractViolation(`${field}_json`);
    }
    seen.add(item);
    if (Array.isArray(item)) {
      for (let index = 0; index < item.length; index += 1) {
        if (!Object.hasOwn(item, index)) throw new ContractViolation(`${field}_sparse_array`);
        stack.push({ value: item[index], depth: current.depth + 1 });
      }
      continue;
    }
    if (!isPlainObject(item)) throw new ContractViolation(`${field}_object`);
    for (const [key, child] of Object.entries(item)) {
      if (key.includes("\0") || Buffer.byteLength(key, "utf8") > 128) {
        throw new ContractViolation(`${field}_key`);
      }
      stack.push({ value: child, depth: current.depth + 1 });
    }
  }
  if (!isJsonValue(value)) throw new ContractViolation(`${field}_json`);
  if (Buffer.byteLength(canonicalJson(value), "utf8") > maxBytes) {
    throw new ContractViolation(`${field}_bytes`);
  }
}

function isPlainObject(value: unknown): value is JsonObject {
  if (value === null || typeof value !== "object" || Array.isArray(value)) return false;
  const prototype = Object.getPrototypeOf(value);
  return prototype === Object.prototype || prototype === null;
}

const RUN_CONTEXT_KINDS = new Set<RunContextV1["kind"]>([
  "company_ticker_set",
  "covered_universe",
  "selected_feed_items",
  "source_filing",
  "research_notebook",
  "existing_answer",
  "routing_request",
  "question_only",
]);

export function isJsonValue(value: unknown, ancestors = new Set<object>()): value is JsonValue {
  if (value === null || typeof value === "string" || typeof value === "boolean") return true;
  if (typeof value === "number") return Number.isFinite(value);
  if (typeof value !== "object" || ancestors.has(value)) return false;
  ancestors.add(value);
  try {
    if (Array.isArray(value)) {
      return value.every((item, index) => Object.hasOwn(value, index) && isJsonValue(item, ancestors));
    }
    if (!isPlainObject(value)) return false;
    return Object.values(value).every((item) => isJsonValue(item, ancestors));
  } finally {
    ancestors.delete(value);
  }
}
