import {
  AGENT_V1_ABI_VERSION,
  type EncodedEnqueueRunRequest,
  type HostRunInput,
} from "./contracts.js";
import { buildEnqueueRunRequest } from "./claim.js";
import { encodeProcedureRequest } from "./procedure.js";
import {
  canonicalHash,
  isContentHash,
  type ContentHash,
  type JsonObject,
  type JsonValue,
} from "./json.js";
import {
  materializeRunInputs,
  validateAuthenticatedRunOwnership,
  type AuthenticatedRunOwnershipV1,
  type EnqueueRunIntentV1,
  type MaterializationPorts,
} from "./materialization.js";
import {
  validatePinnedReleaseArtifact,
  type PinnedReleaseArtifact,
} from "./release-artifact.js";
import { ContractViolation, isJsonValue, validateBoundedIdentifier } from "./validation.js";

export interface ReleasePinV1 extends JsonObject {
  readonly schema_version: 1;
  readonly descriptor_artifact_hash: ContentHash;
  readonly release_set_hash: ContentHash;
}

export interface MaterializationAuditV1 extends JsonObject {
  readonly schema_version: 1;
  readonly committed_source_claim_hash: ContentHash | null;
}

/**
 * Persist this value before the first enqueue attempt and reuse it byte-for-
 * byte for every retry. Rematerializing memory under the same run id is a
 * divergent mutation, and the database correctly rejects it.
 */
export interface PreparedEnqueueRunV1 extends JsonObject {
  readonly schema_version: 1;
  readonly ownership: AuthenticatedRunOwnershipV1;
  readonly release_pin: ReleasePinV1;
  readonly materialization: MaterializationAuditV1;
  readonly agent_request: EncodedEnqueueRunRequest;
  readonly preparation_hash: ContentHash;
}

export interface PrepareEnqueueRunInput {
  readonly artifact: PinnedReleaseArtifact;
  readonly ownership: AuthenticatedRunOwnershipV1;
  readonly intent: EnqueueRunIntentV1;
  readonly materializers: MaterializationPorts;
}

export async function prepareEnqueueRun(
  input: PrepareEnqueueRunInput,
): Promise<PreparedEnqueueRunV1> {
  validatePinnedReleaseArtifact(input.artifact);
  validateAuthenticatedRunOwnership(input.ownership);
  const materialized = await materializeRunInputs(
    input.ownership,
    input.intent,
    input.materializers,
  );
  const hostInput: HostRunInput = {
    mutationId: input.intent.mutation_id,
    runId: input.ownership.run_id,
    tenantId: input.ownership.tenant_id,
    principalId: input.ownership.principal_id,
    sessionId: input.ownership.session_id,
    runKind: input.intent.run_kind,
    locale: input.intent.locale,
    question: input.intent.question,
    context: materialized.context,
    // Session memory is resolved only by the worker after acquiring a DB fence.
    sessionMemory: null,
  };
  const agentRequest = encodeProcedureRequest(
    "agent_v1.enqueue_run",
    buildEnqueueRunRequest(input.artifact.descriptor, hostInput),
  ) as EncodedEnqueueRunRequest;
  const releasePin: ReleasePinV1 = {
    schema_version: 1,
    descriptor_artifact_hash: input.artifact.artifact_hash,
    release_set_hash: input.artifact.release_set_hash,
  };
  const materialization: MaterializationAuditV1 = {
    schema_version: 1,
    committed_source_claim_hash: materialized.committedSourceClaimHash,
  };
  const unsigned = {
    schema_version: 1,
    ownership: input.ownership,
    release_pin: releasePin,
    materialization,
    agent_request: agentRequest,
  } as const;
  return deepFreeze({
    ...unsigned,
    preparation_hash: canonicalHash(unsigned),
  });
}

export function validatePreparedEnqueueRun(
  value: PreparedEnqueueRunV1,
): void {
  exactKeys(value, [
    "schema_version",
    "ownership",
    "release_pin",
    "materialization",
    "agent_request",
    "preparation_hash",
  ]);
  if (value.schema_version !== 1) throw new ContractViolation("prepared_enqueue_version");
  validateAuthenticatedRunOwnership(value.ownership);
  validateReleasePin(value.release_pin);
  validateMaterializationAudit(value.materialization, value.agent_request);
  validateEncodedEnqueueIdentity(value.agent_request, value.ownership);
  if (!isContentHash(value.preparation_hash)) {
    throw new ContractViolation("invalid_preparation_hash");
  }
  const { preparation_hash: _preparationHash, ...unsigned } = value;
  if (canonicalHash(unsigned) !== value.preparation_hash) {
    throw new ContractViolation("preparation_hash_mismatch");
  }
}

export interface PrepareCancelRunInput {
  readonly mutationId: string;
  readonly ownership: AuthenticatedRunOwnershipV1;
  readonly reasonCode: string;
  /** Server-created credit/reservation release receipt; never browser JSON. */
  readonly release: JsonValue;
}

export interface PreparedCancelRunV1 extends JsonObject {
  readonly schema_version: 1;
  readonly ownership: AuthenticatedRunOwnershipV1;
  readonly agent_request: JsonObject;
  readonly preparation_hash: ContentHash;
}

export function prepareCancelRun(input: PrepareCancelRunInput): PreparedCancelRunV1 {
  validateAuthenticatedRunOwnership(input.ownership);
  validateBoundedIdentifier(input.mutationId, "mutation_id");
  validateBoundedIdentifier(input.reasonCode, "reason_code", 64);
  if (!isJsonValue(input.release)) throw new ContractViolation("invalid_release_payload");
  const agentRequest = encodeProcedureRequest("agent_v1.request_cancel", {
    mutation_id: input.mutationId,
    run_id: input.ownership.run_id,
    tenant_id: input.ownership.tenant_id,
    reason_code: input.reasonCode,
    release: input.release,
  });
  const unsigned = {
    schema_version: 1,
    ownership: input.ownership,
    agent_request: agentRequest,
  } as const;
  return deepFreeze({
    ...unsigned,
    preparation_hash: canonicalHash(unsigned),
  });
}

export function validatePreparedCancelRun(value: PreparedCancelRunV1): void {
  exactKeys(value, ["schema_version", "ownership", "agent_request", "preparation_hash"]);
  if (value.schema_version !== 1) throw new ContractViolation("prepared_cancel_version");
  validateAuthenticatedRunOwnership(value.ownership);
  if (
    value.agent_request.abi_version !== AGENT_V1_ABI_VERSION ||
    value.agent_request.run_id !== value.ownership.run_id ||
    value.agent_request.tenant_id !== value.ownership.tenant_id
  ) {
    throw new ContractViolation("prepared_cancel_identity_mismatch");
  }
  validateMutationHash("agent_v1.request_cancel", value.agent_request);
  if (!isContentHash(value.preparation_hash)) {
    throw new ContractViolation("invalid_preparation_hash");
  }
  const { preparation_hash: _preparationHash, ...unsigned } = value;
  if (canonicalHash(unsigned) !== value.preparation_hash) {
    throw new ContractViolation("preparation_hash_mismatch");
  }
}

function validateEncodedEnqueueIdentity(
  value: EncodedEnqueueRunRequest,
  ownership: AuthenticatedRunOwnershipV1,
): void {
  if (
    value.abi_version !== AGENT_V1_ABI_VERSION ||
    value.run_id !== ownership.run_id ||
    value.tenant_id !== ownership.tenant_id ||
    value.principal_id !== ownership.principal_id ||
    value.session_id !== ownership.session_id
  ) {
    throw new ContractViolation("encoded_enqueue_identity_mismatch");
  }
  if (!isContentHash(value.immutable_snapshot_hash)) {
    throw new ContractViolation("invalid_immutable_snapshot_hash");
  }
  if (canonicalHash(value.immutable_snapshot) !== value.immutable_snapshot_hash) {
    throw new ContractViolation("immutable_snapshot_hash_mismatch");
  }
  const request = value.immutable_snapshot.request;
  if (
    request.run_id !== ownership.run_id ||
    request.tenant_id !== ownership.tenant_id ||
    request.principal_id !== ownership.principal_id ||
    request.session_id !== ownership.session_id
  ) {
    throw new ContractViolation("immutable_snapshot_identity_mismatch");
  }
  validateMutationHash("agent_v1.enqueue_run", value);
}

function validateMutationHash(procedure: string, value: JsonObject): void {
  if (!isContentHash(value.mutation_hash)) {
    throw new ContractViolation("invalid_mutation_hash");
  }
  const { mutation_hash: _mutationHash, ...request } = value;
  if (canonicalHash({ procedure, request }) !== value.mutation_hash) {
    throw new ContractViolation("mutation_hash_mismatch");
  }
}

function validateReleasePin(value: ReleasePinV1): void {
  exactKeys(value, ["schema_version", "descriptor_artifact_hash", "release_set_hash"]);
  if (
    value.schema_version !== 1 ||
    !isContentHash(value.descriptor_artifact_hash) ||
    !isContentHash(value.release_set_hash)
  ) {
    throw new ContractViolation("invalid_release_pin");
  }
}

function validateMaterializationAudit(
  value: MaterializationAuditV1,
  request: EncodedEnqueueRunRequest,
): void {
  exactKeys(value, [
    "schema_version",
    "committed_source_claim_hash",
  ]);
  if (value.schema_version !== 1) {
    throw new ContractViolation("materialization_audit_version");
  }
  const existingAnswer = request.immutable_snapshot.request.context.kind === "existing_answer";
  if (
    (existingAnswer !== (value.committed_source_claim_hash !== null)) ||
    (value.committed_source_claim_hash !== null &&
      !isContentHash(value.committed_source_claim_hash)) ||
    request.immutable_snapshot.request.session_memory !== null
  ) {
    throw new ContractViolation("materialization_audit_mismatch");
  }
}

function exactKeys(value: unknown, expected: readonly string[]): void {
  if (!isPlainObject(value)) throw new ContractViolation("expected_plain_object");
  const actual = Object.keys(value).sort();
  const wanted = [...expected].sort();
  if (actual.length !== wanted.length || actual.some((key, index) => key !== wanted[index])) {
    throw new ContractViolation("unknown_or_missing_prepared_request_field");
  }
}

function isPlainObject(value: unknown): value is JsonObject {
  if (value === null || typeof value !== "object" || Array.isArray(value)) return false;
  const prototype = Object.getPrototypeOf(value);
  return prototype === Object.prototype || prototype === null;
}

function deepFreeze<T>(value: T): T {
  if (value !== null && typeof value === "object" && !Object.isFrozen(value)) {
    Object.freeze(value);
    for (const nested of Object.values(value)) deepFreeze(nested);
  }
  return value;
}
