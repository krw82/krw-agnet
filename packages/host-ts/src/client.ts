import {
  AGENT_V1_ABI_VERSION,
  type CancelRunResponse,
  type EnqueueRunResponse,
  type OutboxReceipt,
  type ReadCommittedOutcomeResponse,
  type ReadFinalOutputResponse,
  type ReadFinalProjectionResponse,
  type ReadTerminalTraceResponse,
  type SessionMemoryRetirementInput,
  type SessionMemoryRetirementResponse,
} from "./contracts.js";
import { canonicalHash, isContentHash, type JsonObject, type JsonValue } from "./json.js";
import {
  validatePreparedCancelRun,
  validatePreparedEnqueueRun,
  type PreparedCancelRunV1,
  type PreparedEnqueueRunV1,
} from "./ownership.js";
import {
  validateAuthenticatedRunOwnership,
  type AuthenticatedRunOwnershipV1,
} from "./materialization.js";
import {
  type HostProcedure,
  type OutboxProcedure,
} from "./procedure.js";
import {
  ContractViolation,
  isJsonValue,
  validateBoundedIdentifier,
} from "./validation.js";

export { encodeProcedureRequest, type HostProcedure, type OutboxProcedure } from "./procedure.js";

export interface JsonProcedureTransport<P extends string> {
  execute(procedure: P, request: JsonObject): Promise<unknown>;
}

export class HostAgentClient {
  constructor(private readonly transport: JsonProcedureTransport<HostProcedure>) {}

  async enqueue(prepared: PreparedEnqueueRunV1): Promise<EnqueueRunResponse> {
    validatePreparedEnqueueRun(prepared);
    const response = await this.transport.execute(
      "agent_v1.enqueue_run",
      prepared.agent_request,
    );
    return parseEnqueueResponse(response, prepared.ownership.run_id);
  }

  async cancel(prepared: PreparedCancelRunV1): Promise<CancelRunResponse> {
    validatePreparedCancelRun(prepared);
    const response = await this.transport.execute(
      "agent_v1.request_cancel",
      prepared.agent_request,
    );
    return parseCancelResponse(response, prepared.ownership.run_id);
  }

  /**
   * Permanently retires one room's agent context after the product has
   * authenticated a hard-purge lifecycle event. This server-side method does
   * not accept browser history or soft-delete signals.
   */
  async retireSessionMemory(
    input: SessionMemoryRetirementInput,
  ): Promise<SessionMemoryRetirementResponse> {
    validateBoundedIdentifier(input.mutationId, "mutation_id");
    validateBoundedIdentifier(input.tenantId, "tenant_id");
    validateBoundedIdentifier(input.principalId, "principal_id");
    validateBoundedIdentifier(input.sessionId, "session_id");
    if (!isContentHash(input.lifecycleReceiptHash)) {
      throw new ContractViolation("invalid_lifecycle_receipt_hash");
    }
    const request: JsonObject = {
      abi_version: AGENT_V1_ABI_VERSION,
      mutation_id: input.mutationId,
      tenant_id: input.tenantId,
      principal_id: input.principalId,
      session_id: input.sessionId,
      lifecycle_receipt_hash: input.lifecycleReceiptHash,
      reason_code: "product_hard_purge",
    };
    const response = await this.transport.execute(
      "agent_v1.retire_session_memory",
      { ...request, mutation_hash: canonicalHash({
        procedure: "agent_v1.retire_session_memory",
        request,
      }) },
    );
    const object = exactObject(response, [
      "outcome",
      "tenant_id",
      "principal_id",
      "session_id",
      "lifecycle_receipt_hash",
    ]);
    if (
      object.outcome !== "retired" ||
      object.tenant_id !== input.tenantId ||
      object.principal_id !== input.principalId ||
      object.session_id !== input.sessionId ||
      object.lifecycle_receipt_hash !== input.lifecycleReceiptHash
    ) {
      throw new ContractViolation("retirement_response_identity_mismatch");
    }
    return object as unknown as SessionMemoryRetirementResponse;
  }

  async readCommittedOutcome(
    ownership: AuthenticatedRunOwnershipV1,
  ): Promise<ReadCommittedOutcomeResponse> {
    validateAuthenticatedRunOwnership(ownership);
    const response = await this.transport.execute("agent_v1.read_committed_outcome", {
      abi_version: AGENT_V1_ABI_VERSION,
      run_id: ownership.run_id,
      tenant_id: ownership.tenant_id,
    });
    return parseOutcomeResponse(response, ownership.run_id);
  }

  /**
   * Reads only committed final Markdown through the fixed host ABI.
   * @deprecated use readFinalProjection
   */
  async readFinalOutput(
    ownership: AuthenticatedRunOwnershipV1,
  ): Promise<ReadFinalOutputResponse> {
    validateAuthenticatedRunOwnership(ownership);
    const response = await this.transport.execute("agent_v1.read_final_output", {
      abi_version: AGENT_V1_ABI_VERSION,
      run_id: ownership.run_id,
      tenant_id: ownership.tenant_id,
    });
    return parseFinalOutputResponse(response, ownership.run_id);
  }

  /**
   * Stronger final projection. Verifies full ownership (tenant+principal+
   * session+run) on the database side, matching `commit_final`'s checks, and
   * returns the rendered Markdown plus the public ledger hashes and usage
   * counters required for product projection.
   */
  async readFinalProjection(
    ownership: AuthenticatedRunOwnershipV1,
  ): Promise<ReadFinalProjectionResponse> {
    validateAuthenticatedRunOwnership(ownership);
    const response = await this.transport.execute("agent_v1.read_final_projection", {
      abi_version: AGENT_V1_ABI_VERSION,
      run_id: ownership.run_id,
      tenant_id: ownership.tenant_id,
      principal_id: ownership.principal_id,
      session_id: ownership.session_id,
    });
    return parseFinalProjectionResponse(response, ownership.run_id);
  }

  /**
   * Reads a bounded terminal action summary for operator quality review.
   * The database verifies full ownership and excludes prompts, arguments,
   * result bodies, artifact references, and provider reasoning.
   */
  async readTerminalTrace(
    ownership: AuthenticatedRunOwnershipV1,
  ): Promise<ReadTerminalTraceResponse> {
    validateAuthenticatedRunOwnership(ownership);
    const response = await this.transport.execute("agent_v1.read_terminal_trace", {
      abi_version: AGENT_V1_ABI_VERSION,
      run_id: ownership.run_id,
      tenant_id: ownership.tenant_id,
      principal_id: ownership.principal_id,
      session_id: ownership.session_id,
    });
    return parseTerminalTraceResponse(response, ownership.run_id);
  }
}

export class HostOutboxClient {
  constructor(private readonly transport: JsonProcedureTransport<OutboxProcedure>) {}

  async claim(workerId: string, limit: number, leaseMs: number): Promise<readonly OutboxReceipt[]> {
    validateBoundedIdentifier(workerId, "worker_id");
    boundedInteger(limit, 1, 100, "outbox_limit");
    boundedInteger(leaseMs, 1_000, 600_000, "outbox_lease_ms");
    const response = await this.transport.execute("agent_v1.claim_outbox", {
      abi_version: AGENT_V1_ABI_VERSION,
      worker_id: workerId,
      limit,
      lease_ms: leaseMs,
    });
    const object = exactObject(response, ["events"]);
    if (!Array.isArray(object.events) || object.events.length > 100) {
      throw new ContractViolation("invalid_outbox_events");
    }
    return object.events.map(parseOutboxReceipt);
  }

  async ack(
    workerId: string,
    outboxId: number,
    result: { readonly success: true } | { readonly success: false; readonly errorHash: string },
  ): Promise<"delivered" | "already_delivered" | "released" | "already_released"> {
    validateBoundedIdentifier(workerId, "worker_id");
    boundedInteger(outboxId, 1, Number.MAX_SAFE_INTEGER, "outbox_id");
    if (!result.success && !isContentHash(result.errorHash)) {
      throw new ContractViolation("invalid_outbox_error_hash");
    }
    const request: JsonObject = result.success
      ? { abi_version: AGENT_V1_ABI_VERSION, worker_id: workerId, outbox_id: outboxId, success: true }
      : {
          abi_version: AGENT_V1_ABI_VERSION,
          worker_id: workerId,
          outbox_id: outboxId,
          success: false,
          error_hash: result.errorHash,
        };
    const response = exactObject(
      await this.transport.execute("agent_v1.ack_outbox", request),
      ["outcome", "outbox_id"],
    );
    if (response.outbox_id !== outboxId) throw new ContractViolation("outbox_ack_id_mismatch");
    if (!isOneOf(response.outcome, ["delivered", "already_delivered", "released", "already_released"])) {
      throw new ContractViolation("invalid_outbox_ack_outcome");
    }
    return response.outcome;
  }
}

function parseEnqueueResponse(value: unknown, runId: string): EnqueueRunResponse {
  const object = exactObject(value, ["outcome", "run_id", "run_version"]);
  if (!isOneOf(object.outcome, ["enqueued", "already_enqueued"])) {
    throw new ContractViolation("invalid_enqueue_outcome");
  }
  if (object.run_id !== runId) throw new ContractViolation("enqueue_run_id_mismatch");
  positiveInteger(object.run_version, "run_version");
  return object as unknown as EnqueueRunResponse;
}

function parseCancelResponse(value: unknown, runId: string): CancelRunResponse {
  const object = exactObject(value, [
    "outcome",
    "run_id",
    "fencing_token",
    "run_version",
    "cancel_generation",
  ]);
  if (!isOneOf(object.outcome, ["cancelled", "already_final", "already_cancelled", "already_failed"])) {
    throw new ContractViolation("invalid_cancel_outcome");
  }
  if (object.run_id !== runId) throw new ContractViolation("cancel_run_id_mismatch");
  nonnegativeInteger(object.fencing_token, "fencing_token");
  positiveInteger(object.run_version, "run_version");
  nonnegativeInteger(object.cancel_generation, "cancel_generation");
  return object as unknown as CancelRunResponse;
}

function parseOutcomeResponse(value: unknown, runId: string): ReadCommittedOutcomeResponse {
  const object = exactObject(value, [
    "run_id",
    "state",
    "fencing_token",
    "run_version",
    "cancel_generation",
    "terminal_outcome",
  ]);
  if (object.run_id !== runId) throw new ContractViolation("outcome_run_id_mismatch");
  if (!isOneOf(object.state, ["queued", "deferred", "active", "final", "cancelled", "failed"])) {
    throw new ContractViolation("invalid_run_state");
  }
  nonnegativeInteger(object.fencing_token, "fencing_token");
  positiveInteger(object.run_version, "run_version");
  nonnegativeInteger(object.cancel_generation, "cancel_generation");
  if (!isJsonValue(object.terminal_outcome)) throw new ContractViolation("invalid_terminal_outcome");
  return object as unknown as ReadCommittedOutcomeResponse;
}

function parseFinalOutputResponse(value: unknown, runId: string): ReadFinalOutputResponse {
  const object = exactObject(value, [
    "run_id",
    "final_output_hash",
    "markdown",
    "visualizations",
  ]);
  if (object.run_id !== runId || !isContentHash(object.final_output_hash)) {
    throw new ContractViolation("invalid_final_output_identity");
  }
  if (
    typeof object.markdown !== "string" ||
    object.markdown.length === 0 ||
    object.markdown.includes("\0") ||
    Buffer.byteLength(object.markdown, "utf8") > 64 * 1024
  ) {
    throw new ContractViolation("invalid_final_output_markdown");
  }
  assertVisualizations(object.visualizations);
  return object as unknown as ReadFinalOutputResponse;
}

function assertVisualizations(value: unknown): asserts value is readonly JsonValue[] {
  if (
    !Array.isArray(value) ||
    value.length > 16 ||
    !value.every((item) => isJsonValue(item) && typeof item === "object" && item !== null)
  ) {
    throw new ContractViolation("invalid_final_output_visualizations");
  }
}

function parseFinalProjectionResponse(
  value: unknown,
  runId: string,
): ReadFinalProjectionResponse {
  const object = exactObject(value, [
    "run_id",
    "answer_bundle_hash",
    "final_output_hash",
    "markdown",
    "visualizations",
    "usage",
    "evidence_ledger_hash",
    "memory_revision",
    "memory_frontier_hash",
  ]);
  if (
    object.run_id !== runId ||
    !isContentHash(object.answer_bundle_hash) ||
    !isContentHash(object.final_output_hash)
  ) {
    throw new ContractViolation("invalid_final_projection_identity");
  }
  if (
    typeof object.markdown !== "string" ||
    object.markdown.length === 0 ||
    object.markdown.includes("\0") ||
    Buffer.byteLength(object.markdown, "utf8") > 64 * 1024
  ) {
    throw new ContractViolation("invalid_final_projection_markdown");
  }
  assertVisualizations(object.visualizations);
  if (!isJsonValue(object.usage)) {
    throw new ContractViolation("invalid_final_projection_usage");
  }
  if (!isNullableContentHash(object.evidence_ledger_hash)) {
    throw new ContractViolation("invalid_final_projection_evidence_ledger_hash");
  }
  if (
    object.memory_revision !== null &&
    (!Number.isSafeInteger(object.memory_revision) || (object.memory_revision as number) < 0)
  ) {
    throw new ContractViolation("invalid_final_projection_memory_revision");
  }
  if (!isNullableContentHash(object.memory_frontier_hash)) {
    throw new ContractViolation("invalid_final_projection_memory_frontier_hash");
  }
  return object as unknown as ReadFinalProjectionResponse;
}

function parseTerminalTraceResponse(value: unknown, runId: string): ReadTerminalTraceResponse {
  const object = exactObject(value, ["run_id", "state", "actions"]);
  if (object.run_id !== runId || !isOneOf(object.state, ["final", "cancelled", "failed"])) {
    throw new ContractViolation("invalid_terminal_trace_identity");
  }
  if (!Array.isArray(object.actions) || object.actions.length > 512) {
    throw new ContractViolation("invalid_terminal_trace_actions");
  }
  for (const action of object.actions) {
    const entry = exactObject(action, ["capability_id", "stage", "result_hash"]);
    if (typeof entry.capability_id !== "string") {
      throw new ContractViolation("invalid_terminal_trace_capability");
    }
    validateBoundedIdentifier(entry.capability_id, "terminal_trace_capability");
    if (!isOneOf(entry.stage, ["begun", "observed", "accepted", "rejected", "ambiguous"])) {
      throw new ContractViolation("invalid_terminal_trace_stage");
    }
    if (!isNullableContentHash(entry.result_hash)) {
      throw new ContractViolation("invalid_terminal_trace_result_hash");
    }
  }
  return object as unknown as ReadTerminalTraceResponse;
}

function isNullableContentHash(value: unknown): value is string | null {
  return value === null || (typeof value === "string" && isContentHash(value));
}

function parseOutboxReceipt(value: unknown): OutboxReceipt {
  const object = exactObject(value, [
    "outbox_id",
    "run_id",
    "event_kind",
    "dedupe_key",
    "payload",
    "delivery_attempts",
    "delivery_deadline",
  ]);
  positiveInteger(object.outbox_id, "outbox_id");
  validateBoundedIdentifier(asString(object.run_id, "run_id"), "run_id");
  validateBoundedIdentifier(asString(object.event_kind, "event_kind"), "event_kind");
  validateBoundedIdentifier(asString(object.dedupe_key, "dedupe_key"), "dedupe_key", 256);
  positiveInteger(object.delivery_attempts, "delivery_attempts");
  if (typeof object.delivery_deadline !== "string" || Number.isNaN(Date.parse(object.delivery_deadline))) {
    throw new ContractViolation("invalid_delivery_deadline");
  }
  if (!isJsonValue(object.payload)) throw new ContractViolation("invalid_outbox_payload");
  return object as unknown as OutboxReceipt;
}

function exactObject(value: unknown, keys: readonly string[]): Record<string, unknown> {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    throw new ContractViolation("response_not_object");
  }
  const actual = Object.keys(value).sort();
  const expected = [...keys].sort();
  if (actual.length !== expected.length || actual.some((key, index) => key !== expected[index])) {
    throw new ContractViolation("unknown_or_missing_response_field");
  }
  return value as Record<string, unknown>;
}

function isOneOf<const T extends string>(value: unknown, allowed: readonly T[]): value is T {
  return typeof value === "string" && allowed.includes(value as T);
}

function asString(value: unknown, field: string): string {
  if (typeof value !== "string") throw new ContractViolation(`invalid_${field}`);
  return value;
}

function boundedInteger(value: unknown, min: number, max: number, field: string): void {
  if (!Number.isSafeInteger(value) || (value as number) < min || (value as number) > max) {
    throw new ContractViolation(`invalid_${field}`);
  }
}

function positiveInteger(value: unknown, field: string): void {
  boundedInteger(value, 1, Number.MAX_SAFE_INTEGER, field);
}

function nonnegativeInteger(value: unknown, field: string): void {
  boundedInteger(value, 0, Number.MAX_SAFE_INTEGER, field);
}
