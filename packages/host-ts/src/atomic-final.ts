import {
  ALLOWED_MODEL_IDS,
  GLM_MODEL_ID,
  type AllowedModelId,
  type OutboxReceipt,
} from "./contracts.js";
import type { HostAgentClient } from "./client.js";
import {
  canonicalHash,
  isContentHash,
  type ContentHash,
  type JsonObject,
  type JsonValue,
} from "./json.js";
import type { AuthenticatedRunOwnershipV1 } from "./materialization.js";
import type { ProductOutboxHandlers } from "./outbox.js";
import {
  ContractViolation,
  isJsonValue,
  validateBoundedIdentifier,
} from "./validation.js";

export interface ProjectionAuthorityRequestV1 extends JsonObject {
  readonly schema_version: 1;
  readonly outbox_id: number;
  readonly run_id: string;
  readonly event_kind: string;
  readonly dedupe_key: string;
}

/**
 * DB-authoritative readback used to fence every product projection. The
 * resolver must authenticate the product run row before reading agent_v1.
 */
export interface ProjectionAuthorityClaimV1 extends JsonObject {
  readonly schema_version: 1;
  readonly request_hash: ContentHash;
  readonly ownership: AuthenticatedRunOwnershipV1;
  readonly state: "queued" | "deferred" | "active" | "final" | "cancelled" | "failed";
  readonly fencing_token: number;
  readonly run_version: number;
  readonly cancel_generation: number;
  readonly terminal_answer_bundle_hash: ContentHash | null;
}

export interface ProjectionAuthorityResolver {
  resolveProjectionAuthority(request: ProjectionAuthorityRequestV1): Promise<unknown>;
}

export interface ProductRunOwnershipReader {
  /** Must read server-owned tenant/principal/session/run columns, not request JSON. */
  readRunOwnership(runId: string): Promise<unknown>;
}

/** Builds the standard authority resolver from product ownership + agent DB readback. */
export function createProjectionAuthorityResolver(
  ownershipReader: ProductRunOwnershipReader,
  agent: HostAgentClient,
): ProjectionAuthorityResolver {
  return {
    async resolveProjectionAuthority(request) {
      validateProjectionAuthorityRequest(request);
      const rawOwnership = await ownershipReader.readRunOwnership(request.run_id);
      validateOwnership(rawOwnership, request.run_id);
      const ownership = rawOwnership as AuthenticatedRunOwnershipV1;
      const outcome = await agent.readCommittedOutcome(ownership);
      let terminalAnswerBundleHash: ContentHash | null = null;
      if (outcome.state === "final") {
        const terminal = exactObject(outcome.terminal_outcome, ["kind", "answer_bundle_hash"]);
        if (terminal.kind !== "final") {
          throw new ContractViolation("projection_terminal_outcome_mismatch");
        }
        terminalAnswerBundleHash = requireHash(
          terminal.answer_bundle_hash,
          "terminal_answer_bundle_hash",
        );
      }
      return {
        schema_version: 1,
        request_hash: canonicalHash(request),
        ownership,
        state: outcome.state,
        fencing_token: outcome.fencing_token,
        run_version: outcome.run_version,
        cancel_generation: outcome.cancel_generation,
        terminal_answer_bundle_hash: terminalAnswerBundleHash,
      } satisfies ProjectionAuthorityClaimV1;
    },
  };
}

export interface AnswerCommittedMetadataV1 extends JsonObject {
  readonly run_id: string;
  readonly answer_bundle_hash: ContentHash;
  readonly rendered_message_hash: ContentHash;
  readonly memory_revision: number | null;
  readonly memory_frontier_hash: ContentHash | null;
}

export interface AnswerPresentationMetadataV1 extends JsonObject {
  readonly schema_version: 1;
  readonly run_id: string;
  readonly answer_bundle_hash: ContentHash;
  readonly final_output_hash: ContentHash;
  readonly rendered_message_hash: ContentHash;
}

/** Exact serialized `BudgetUsage`; it is not a monetary settlement receipt. */
export interface BillingUsageV1 extends JsonObject {
  readonly provider_turns: number;
  readonly capability_calls: number;
  readonly replans: number;
  readonly repairs: number;
  readonly input_tokens: number;
  readonly output_tokens: number;
  readonly evidence_bytes: number;
  /**
   * Cumulative wall-clock time in milliseconds. Optional because runs
   * persisted before these fields were introduced have `0`/missing values;
   * readers must treat absence as zero.
   */
  readonly provider_total_ms?: number;
  readonly capability_total_ms?: number;
  readonly compact_total_ms?: number;
  readonly provider_queue_wait_ms?: number;
  readonly session_memory_total_ms?: number;
  readonly market_preflight_ms?: number;
  readonly prompt_build_total_ms?: number;
  readonly checkpoint_total_ms?: number;
}

export interface AnswerBillingMetadataV1 extends JsonObject {
  readonly schema_version: 1;
  readonly run_id: string;
  readonly answer_bundle_hash: ContentHash;
  readonly model_id: AllowedModelId;
  readonly usage: BillingUsageV1;
}

export interface SafeTerminalSseEventV1 extends JsonObject {
  readonly schema_version: 1;
  readonly event_id: string;
  readonly event_type: "done";
  readonly data: {
    readonly schema_version: 1;
    readonly run_id: string;
    readonly answer_bundle_hash: ContentHash;
    readonly rendered_message_hash: ContentHash;
    readonly memory_revision: number | null;
    readonly memory_frontier_hash: ContentHash | null;
  };
}

export interface ProjectionCommandBaseV1 extends JsonObject {
  readonly schema_version: 1;
  readonly outbox_id: number;
  readonly idempotency_key: string;
  readonly event_payload_hash: ContentHash;
  readonly ownership: AuthenticatedRunOwnershipV1;
  readonly fencing_token: number;
  readonly run_version: number;
  readonly cancel_generation: number;
}

export interface AnswerCommittedProjectionV1 extends ProjectionCommandBaseV1 {
  readonly kind: "answer_committed";
  readonly answer: AnswerCommittedMetadataV1;
  /** Publish only in the same transaction as the durable final product row. */
  readonly sse: SafeTerminalSseEventV1;
}

export interface AnswerPresentationProjectionV1 extends ProjectionCommandBaseV1 {
  readonly kind: "answer_presentation";
  readonly presentation: AnswerPresentationMetadataV1;
}

export interface AnswerBillingProjectionV1 extends ProjectionCommandBaseV1 {
  readonly kind: "answer_billing";
  readonly model_id: AllowedModelId;
  readonly billing: AnswerBillingMetadataV1;
}

export type TerminalProjectionV1 =
  | (ProjectionCommandBaseV1 & {
      readonly kind: "run_cancelled";
      readonly reason_code: string;
    })
  | (ProjectionCommandBaseV1 & {
      readonly kind: "run_failed";
      readonly reason_code: string;
    })
  | (ProjectionCommandBaseV1 & {
      readonly kind: "run_deferred";
      readonly reason_code: string;
      readonly retry_delay_ms: number;
    });

export interface ProjectionApplyReceiptV1 extends JsonObject {
  readonly schema_version: 1;
  readonly outbox_id: number;
  readonly idempotency_key: string;
  readonly event_payload_hash: ContentHash;
  readonly run_id: string;
  readonly fencing_token: number;
  readonly run_version: number;
  readonly outcome: "applied" | "already_applied" | "stale_ignored";
}

export interface ProductProjectionConsumers {
  /** Must atomically persist final state, the idempotency receipt, and SSE row. */
  applyAnswerCommitted(command: AnswerCommittedProjectionV1): Promise<unknown>;
  applyAnswerPresentation(command: AnswerPresentationProjectionV1): Promise<unknown>;
  /** Usage only. Monetary pricing/settlement must come from an authoritative billing store. */
  applyAnswerBilling(command: AnswerBillingProjectionV1): Promise<unknown>;
  applyTerminal(command: TerminalProjectionV1): Promise<unknown>;
}

/**
 * Exact current daemon outbox adapter. It ACKs only after a validated consumer
 * receipt proves the dedupe key and fence were durably handled.
 */
export function createProductProjectionHandlers(
  authority: ProjectionAuthorityResolver,
  consumers: ProductProjectionConsumers,
): ProductOutboxHandlers {
  return {
    "answer.committed": async (event) => {
      const claim = await resolveAuthority(authority, event);
      const answer = parseAnswerCommitted(event);
      requireFinalAuthority(claim, answer.answer_bundle_hash);
      const command: AnswerCommittedProjectionV1 = {
        ...commandBase(event, claim),
        kind: "answer_committed",
        answer,
        sse: {
          schema_version: 1,
          event_id: `${event.run_id}:done:${answer.answer_bundle_hash}`,
          event_type: "done",
          data: {
            schema_version: 1,
            run_id: event.run_id,
            answer_bundle_hash: answer.answer_bundle_hash,
            rendered_message_hash: answer.rendered_message_hash,
            memory_revision: answer.memory_revision,
            memory_frontier_hash: answer.memory_frontier_hash,
          },
        },
      };
      validateApplyReceipt(await consumers.applyAnswerCommitted(command), command);
    },
    "answer.presentation": async (event) => {
      const claim = await resolveAuthority(authority, event);
      const presentation = parseAnswerPresentation(event);
      requireFinalAuthority(claim, presentation.answer_bundle_hash);
      const command: AnswerPresentationProjectionV1 = {
        ...commandBase(event, claim),
        kind: "answer_presentation",
        presentation,
      };
      validateApplyReceipt(await consumers.applyAnswerPresentation(command), command);
    },
    "answer.billing": async (event) => {
      const claim = await resolveAuthority(authority, event);
      const billing = parseAnswerBilling(event);
      requireFinalAuthority(claim, billing.answer_bundle_hash);
      const command: AnswerBillingProjectionV1 = {
        ...commandBase(event, claim),
        kind: "answer_billing",
        model_id: billing.model_id,
        billing,
      };
      validateApplyReceipt(await consumers.applyAnswerBilling(command), command);
    },
    "run.cancelled": async (event) => {
      const claim = await resolveAuthority(authority, event);
      const payload = exactObject(event.payload, ["run_id", "cancel_generation", "reason_code"]);
      assertRunId(payload.run_id, event.run_id);
      if (claim.state !== "cancelled") throw new ContractViolation("projection_state_mismatch");
      const generation = boundedInteger(payload.cancel_generation, 1, Number.MAX_SAFE_INTEGER, "cancel_generation");
      if (generation !== claim.cancel_generation) {
        throw new ContractViolation("projection_cancel_generation_mismatch");
      }
      const command: TerminalProjectionV1 = {
        ...commandBase(event, claim),
        kind: "run_cancelled",
        reason_code: reasonCode(payload.reason_code),
      };
      validateApplyReceipt(await consumers.applyTerminal(command), command);
    },
    "run.failed": async (event) => {
      const claim = await resolveAuthority(authority, event);
      const payload = exactObject(event.payload, ["run_id", "run_version", "reason_code"]);
      assertRunId(payload.run_id, event.run_id);
      const version = boundedInteger(payload.run_version, 1, Number.MAX_SAFE_INTEGER, "run_version");
      if (claim.state !== "failed" || version !== claim.run_version) {
        throw new ContractViolation("projection_state_mismatch");
      }
      const command: TerminalProjectionV1 = {
        ...commandBase(event, claim),
        kind: "run_failed",
        reason_code: reasonCode(payload.reason_code),
      };
      validateApplyReceipt(await consumers.applyTerminal(command), command);
    },
    "run.deferred": async (event) => {
      const claim = await resolveAuthority(authority, event);
      const payload = exactObject(event.payload, [
        "run_id",
        "run_version",
        "reason_code",
        "retry_delay_ms",
      ]);
      assertRunId(payload.run_id, event.run_id);
      const version = boundedInteger(payload.run_version, 1, Number.MAX_SAFE_INTEGER, "run_version");
      if (claim.state !== "deferred" || version !== claim.run_version) {
        throw new ContractViolation("projection_state_mismatch");
      }
      const command: TerminalProjectionV1 = {
        ...commandBase(event, claim),
        kind: "run_deferred",
        reason_code: reasonCode(payload.reason_code),
        retry_delay_ms: boundedInteger(
          payload.retry_delay_ms,
          0,
          Number.MAX_SAFE_INTEGER,
          "retry_delay_ms",
        ),
      };
      validateApplyReceipt(await consumers.applyTerminal(command), command);
    },
  };
}

export function parseAnswerCommitted(event: OutboxReceipt): AnswerCommittedMetadataV1 {
  if (event.event_kind !== "answer.committed") {
    throw new ContractViolation("answer_committed_wrong_event_kind");
  }
  const payload = exactObject(event.payload, [
    "run_id",
    "answer_bundle_hash",
    "rendered_message_hash",
    "memory_revision",
    "memory_frontier_hash",
  ]);
  assertRunId(payload.run_id, event.run_id);
  const memoryRevision =
    payload.memory_revision === null
      ? null
      : boundedInteger(
          payload.memory_revision,
          1,
          Number.MAX_SAFE_INTEGER,
          "memory_revision",
        );
  const memoryFrontierHash =
    payload.memory_frontier_hash === null
      ? null
      : requireHash(payload.memory_frontier_hash, "memory_frontier_hash");
  if ((memoryRevision === null) !== (memoryFrontierHash === null)) {
    throw new ContractViolation("answer_committed_memory_metadata_partial");
  }
  return {
    run_id: event.run_id,
    answer_bundle_hash: requireHash(payload.answer_bundle_hash, "answer_bundle_hash"),
    rendered_message_hash: requireHash(payload.rendered_message_hash, "rendered_message_hash"),
    memory_revision: memoryRevision,
    memory_frontier_hash: memoryFrontierHash,
  };
}

export function parseAnswerPresentation(event: OutboxReceipt): AnswerPresentationMetadataV1 {
  if (event.event_kind !== "answer.presentation") {
    throw new ContractViolation("answer_presentation_wrong_event_kind");
  }
  const payload = exactObject(event.payload, [
    "schema_version",
    "run_id",
    "answer_bundle_hash",
    "final_output_hash",
    "rendered_message_hash",
  ]);
  if (payload.schema_version !== 1) {
    throw new ContractViolation("answer_presentation_version");
  }
  assertRunId(payload.run_id, event.run_id);
  return {
    schema_version: 1,
    run_id: event.run_id,
    answer_bundle_hash: requireHash(payload.answer_bundle_hash, "answer_bundle_hash"),
    final_output_hash: requireHash(payload.final_output_hash, "final_output_hash"),
    rendered_message_hash: requireHash(payload.rendered_message_hash, "rendered_message_hash"),
  };
}

export function parseAnswerBilling(event: OutboxReceipt): AnswerBillingMetadataV1 {
  if (event.event_kind !== "answer.billing") {
    throw new ContractViolation("answer_billing_wrong_event_kind");
  }
  const payload = exactObjectWithOptional(event.payload, [
    "schema_version",
    "run_id",
    "answer_bundle_hash",
    "usage",
  ], ["model_id"]);
  if (payload.schema_version !== 1) throw new ContractViolation("answer_billing_version");
  assertRunId(payload.run_id, event.run_id);
  const modelId = payload.model_id === undefined ? GLM_MODEL_ID : payload.model_id;
  if (!ALLOWED_MODEL_IDS.includes(modelId as string)) {
    throw new ContractViolation("billing_model_not_allowlisted");
  }
  return {
    schema_version: 1,
    run_id: event.run_id,
    answer_bundle_hash: requireHash(payload.answer_bundle_hash, "answer_bundle_hash"),
    model_id: modelId as AllowedModelId,
    usage: parseBillingUsage(payload.usage),
  };
}

function parseBillingUsage(value: unknown): BillingUsageV1 {
  const usage = exactObjectWithOptional(value, [
    "provider_turns",
    "capability_calls",
    "replans",
    "repairs",
    "input_tokens",
    "output_tokens",
    "evidence_bytes",
  ], [
    "provider_total_ms",
    "capability_total_ms",
    "compact_total_ms",
    "provider_queue_wait_ms",
    "session_memory_total_ms",
    "market_preflight_ms",
    "prompt_build_total_ms",
    "checkpoint_total_ms",
  ]);
  const parsed = {
    provider_turns: boundedInteger(usage.provider_turns, 0, 65_535, "provider_turns"),
    capability_calls: boundedInteger(usage.capability_calls, 0, 65_535, "capability_calls"),
    replans: boundedInteger(usage.replans, 0, 255, "replans"),
    repairs: boundedInteger(usage.repairs, 0, 255, "repairs"),
    input_tokens: boundedInteger(usage.input_tokens, 0, 0xffff_ffff, "input_tokens"),
    output_tokens: boundedInteger(usage.output_tokens, 0, 0xffff_ffff, "output_tokens"),
    evidence_bytes: boundedInteger(
      usage.evidence_bytes,
      0,
      Number.MAX_SAFE_INTEGER,
      "evidence_bytes",
    ),
  };
  const diagnostics: Record<string, number> = {};
  for (const field of [
    "provider_total_ms",
    "capability_total_ms",
    "compact_total_ms",
    "provider_queue_wait_ms",
    "session_memory_total_ms",
    "market_preflight_ms",
    "prompt_build_total_ms",
    "checkpoint_total_ms",
  ] as const) {
    if (field in usage) {
      diagnostics[field] = boundedInteger(
        usage[field],
        0,
        Number.MAX_SAFE_INTEGER,
        field,
      );
    }
  }
  return { ...parsed, ...diagnostics } as BillingUsageV1;
}

async function resolveAuthority(
  resolver: ProjectionAuthorityResolver,
  event: OutboxReceipt,
): Promise<ProjectionAuthorityClaimV1> {
  const request: ProjectionAuthorityRequestV1 = Object.freeze({
    schema_version: 1,
    outbox_id: event.outbox_id,
    run_id: event.run_id,
    event_kind: event.event_kind,
    dedupe_key: event.dedupe_key,
  });
  const value = await resolver.resolveProjectionAuthority(request);
  const claim = exactObject(value, [
    "schema_version",
    "request_hash",
    "ownership",
    "state",
    "fencing_token",
    "run_version",
    "cancel_generation",
    "terminal_answer_bundle_hash",
  ]) as unknown as ProjectionAuthorityClaimV1;
  if (claim.schema_version !== 1 || claim.request_hash !== canonicalHash(request)) {
    throw new ContractViolation("projection_authority_request_mismatch");
  }
  validateOwnership(claim.ownership, event.run_id);
  if (!RUN_STATES.has(claim.state)) throw new ContractViolation("projection_authority_state");
  boundedInteger(claim.fencing_token, 0, Number.MAX_SAFE_INTEGER, "fencing_token");
  boundedInteger(claim.run_version, 1, Number.MAX_SAFE_INTEGER, "run_version");
  boundedInteger(claim.cancel_generation, 0, Number.MAX_SAFE_INTEGER, "cancel_generation");
  if (
    claim.terminal_answer_bundle_hash !== null &&
    !isContentHash(claim.terminal_answer_bundle_hash)
  ) {
    throw new ContractViolation("projection_authority_terminal_hash");
  }
  return claim;
}

function validateProjectionAuthorityRequest(request: ProjectionAuthorityRequestV1): void {
  exactObject(request, ["schema_version", "outbox_id", "run_id", "event_kind", "dedupe_key"]);
  if (request.schema_version !== 1) {
    throw new ContractViolation("projection_authority_request_version");
  }
  boundedInteger(request.outbox_id, 1, Number.MAX_SAFE_INTEGER, "outbox_id");
  validateBoundedIdentifier(request.run_id, "run_id");
  validateBoundedIdentifier(request.event_kind, "event_kind");
  validateBoundedIdentifier(request.dedupe_key, "dedupe_key", 256);
}

function commandBase(
  event: OutboxReceipt,
  claim: ProjectionAuthorityClaimV1,
): ProjectionCommandBaseV1 {
  return {
    schema_version: 1,
    outbox_id: event.outbox_id,
    idempotency_key: event.dedupe_key,
    event_payload_hash: canonicalHash(event.payload),
    ownership: claim.ownership,
    fencing_token: claim.fencing_token,
    run_version: claim.run_version,
    cancel_generation: claim.cancel_generation,
  };
}

function validateApplyReceipt(value: unknown, command: ProjectionCommandBaseV1): void {
  const receipt = exactObject(value, [
    "schema_version",
    "outbox_id",
    "idempotency_key",
    "event_payload_hash",
    "run_id",
    "fencing_token",
    "run_version",
    "outcome",
  ]);
  if (
    receipt.schema_version !== 1 ||
    receipt.outbox_id !== command.outbox_id ||
    receipt.idempotency_key !== command.idempotency_key ||
    receipt.event_payload_hash !== command.event_payload_hash ||
    receipt.run_id !== command.ownership.run_id ||
    receipt.fencing_token !== command.fencing_token ||
    receipt.run_version !== command.run_version ||
    !isOneOf(receipt.outcome, ["applied", "already_applied", "stale_ignored"])
  ) {
    throw new ContractViolation("projection_apply_receipt_mismatch");
  }
}

function requireFinalAuthority(
  claim: ProjectionAuthorityClaimV1,
  answerBundleHash: ContentHash,
): void {
  if (
    claim.state !== "final" ||
    claim.terminal_answer_bundle_hash !== answerBundleHash
  ) {
    throw new ContractViolation("projection_final_authority_mismatch");
  }
}

function validateOwnership(value: unknown, runId: string): void {
  const ownership = exactObject(value, [
    "schema_version",
    "tenant_id",
    "principal_id",
    "session_id",
    "run_id",
  ]);
  if (ownership.schema_version !== 1 || ownership.run_id !== runId) {
    throw new ContractViolation("projection_ownership_mismatch");
  }
  for (const field of ["tenant_id", "principal_id", "session_id", "run_id"] as const) {
    if (typeof ownership[field] !== "string") throw new ContractViolation(`invalid_${field}`);
    validateBoundedIdentifier(ownership[field], field);
  }
}

function reasonCode(value: unknown): string {
  if (typeof value !== "string") throw new ContractViolation("reason_code_string");
  validateBoundedIdentifier(value, "reason_code", 64);
  return value;
}

function assertRunId(value: unknown, expected: string): void {
  if (value !== expected) throw new ContractViolation("outbox_run_id_mismatch");
  validateBoundedIdentifier(expected, "run_id");
}

function boundedInteger(value: unknown, min: number, max: number, field: string): number {
  if (!Number.isSafeInteger(value) || (value as number) < min || (value as number) > max) {
    throw new ContractViolation(`invalid_${field}`);
  }
  return value as number;
}

function requireHash(value: unknown, field: string): ContentHash {
  if (!isContentHash(value)) throw new ContractViolation(`invalid_${field}`);
  return value;
}

function exactObject(value: unknown, keys: readonly string[]): Record<string, unknown> {
  if (value === null || typeof value !== "object" || Array.isArray(value) || !isJsonValue(value)) {
    throw new ContractViolation("expected_exact_json_object");
  }
  const actual = Object.keys(value).sort();
  const expected = [...keys].sort();
  if (actual.length !== expected.length || actual.some((key, index) => key !== expected[index])) {
    throw new ContractViolation("unknown_or_missing_projection_field");
  }
  return value as Record<string, unknown>;
}

function exactObjectWithOptional(
  value: unknown,
  required: readonly string[],
  optional: readonly string[],
): Record<string, unknown> {
  if (value === null || typeof value !== "object" || Array.isArray(value) || !isJsonValue(value)) {
    throw new ContractViolation("expected_exact_json_object");
  }
  const allowed = new Set([...required, ...optional]);
  const actual = Object.keys(value);
  if (
    required.some((key) => !actual.includes(key)) ||
    actual.some((key) => !allowed.has(key))
  ) {
    throw new ContractViolation("unknown_or_missing_projection_field");
  }
  return value as Record<string, unknown>;
}

function isOneOf<const T extends string>(value: unknown, allowed: readonly T[]): value is T {
  return typeof value === "string" && allowed.includes(value as T);
}

const RUN_STATES = new Set<JsonValue>([
  "queued",
  "deferred",
  "active",
  "final",
  "cancelled",
  "failed",
]);
