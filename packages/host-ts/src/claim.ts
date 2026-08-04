import {
  CLAIM_SCHEMA_VERSION,
  KRW_PROTOCOL_VERSION,
  type EnqueueRunRequest,
  type HostRunInput,
  type ImmutableRunClaimV1,
  type PublicReleaseDescriptor,
  type PublicReleaseEntrypoint,
  type RunContextV1,
  type RunRequest,
  type RunResourceProfileV1,
} from "./contracts.js";
import { canonicalHash } from "./json.js";
import {
  ContractViolation,
  validateBoundedIdentifier,
  validateClaimShape,
  validateReleaseDescriptor,
  validateRunContext,
} from "./validation.js";

const RESOURCE_PROFILE: RunResourceProfileV1 = Object.freeze({
  schema_version: CLAIM_SCHEMA_VERSION,
  workload_class: "read_only_interactive",
});

export function buildEnqueueRunRequest(
  descriptor: PublicReleaseDescriptor,
  input: HostRunInput,
): EnqueueRunRequest {
  validateReleaseDescriptor(descriptor);
  validateHostRunInput(input);
  const entry = exactEntrypoint(descriptor, input.runKind, input.locale);
  validateScope(entry, input.context);

  const request: RunRequest = {
    run_id: input.runId,
    session_id: input.sessionId,
    tenant_id: input.tenantId,
    principal_id: input.principalId,
    run_kind: entry.run_kind,
    locale: entry.locale,
    question: input.question,
    requested_model: entry.execution.requested_model,
    model_profile: entry.execution.model_profile,
    budget: entry.execution.budget,
    context: input.context,
    session_memory: input.sessionMemory,
  };
  validateClaimShape(request, entry.execution);

  const immutableSnapshot: ImmutableRunClaimV1 = {
    schema_version: CLAIM_SCHEMA_VERSION,
    request,
    execution: entry.execution,
    resource_profile: RESOURCE_PROFILE,
  };
  return {
    mutation_id: input.mutationId,
    run_id: input.runId,
    tenant_id: input.tenantId,
    principal_id: input.principalId,
    session_id: input.sessionId,
    agent_image_hash: entry.agent_image_hash,
    runtime_version: descriptor.runtime_version,
    priority: 0,
    immutable_snapshot_hash: canonicalHash(immutableSnapshot),
    immutable_snapshot: immutableSnapshot,
    resource_profile: RESOURCE_PROFILE,
    budgets: entry.execution.budget,
  };
}

export function exactEntrypoint(
  descriptor: PublicReleaseDescriptor,
  runKind: string,
  locale: string,
): PublicReleaseEntrypoint {
  const matches = descriptor.entries.filter(
    (entry) => entry.run_kind === runKind && entry.locale === locale,
  );
  if (matches.length !== 1) throw new ContractViolation("release_route_not_exact");
  return matches[0] as PublicReleaseEntrypoint;
}

function validateHostRunInput(input: HostRunInput): void {
  validateBoundedIdentifier(input.mutationId, "mutation_id");
  validateBoundedIdentifier(input.runId, "run_id");
  validateBoundedIdentifier(input.tenantId, "tenant_id");
  validateBoundedIdentifier(input.principalId, "principal_id");
  validateBoundedIdentifier(input.sessionId, "session_id");
  validateBoundedIdentifier(input.runKind, "run_kind");
  validateBoundedIdentifier(input.locale, "locale", 32);
  if (
    input.question.length === 0 ||
    input.question.includes("\0") ||
    Buffer.byteLength(input.question, "utf8") > 64 * 1024
  ) {
    throw new ContractViolation("invalid_question");
  }
  validateRunContext(input.context);
  if (input.sessionMemory !== null) {
    throw new ContractViolation("host_session_memory_forbidden");
  }
}

function validateScope(entry: PublicReleaseEntrypoint, context: RunContextV1): void {
  if (entry.scope.context_kind !== context.kind) {
    throw new ContractViolation("run_context_kind_mismatch");
  }
  const count = scopedValueCount(context);
  const allowed =
    entry.scope.cardinality === "exact" ? count === entry.scope.value : count <= entry.scope.value;
  if (!allowed) throw new ContractViolation("run_context_cardinality_mismatch");
}

function scopedValueCount(context: RunContextV1): number {
  switch (context.kind) {
    case "company_ticker_set":
      return context.tickers.length;
    case "selected_feed_items":
      return context.feed_item_ids.length;
    case "source_filing":
    case "research_notebook":
      return 1;
    case "existing_answer":
      return Object.keys(context.committed_source.source_unit_hashes).length;
    case "covered_universe":
    case "routing_request":
    case "question_only":
      return 0;
  }
}

export function assertCurrentProtocol(entry: PublicReleaseEntrypoint): void {
  if (entry.execution.protocol_version !== KRW_PROTOCOL_VERSION) {
    throw new ContractViolation("unsupported_protocol");
  }
}
