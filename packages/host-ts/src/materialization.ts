import {
  SESSION_MEMORY_CARRIER_SCHEMA_VERSION,
  type CommittedAnswerSourceV1,
  type RunContextV1,
  type SessionMemoryCarrierV3,
} from "./contracts.js";
import { validateCommittedAnswerSource } from "./committed-source.js";
import {
  canonicalHash,
  bytesContentHash,
  isContentHash,
  utf8ContentHash,
  type ContentHash,
  type JsonObject,
  type JsonValue,
} from "./json.js";
import {
  ContractViolation,
  validateBoundedIdentifier,
  validateRunContext,
  validateSessionMemoryCarrier,
} from "./validation.js";

const MAX_QUESTION_BYTES = 64 * 1024;

/** Identity supplied by authenticated server state, never request JSON. */
export interface AuthenticatedRunOwnershipV1 extends JsonObject {
  readonly schema_version: 1;
  readonly tenant_id: string;
  readonly principal_id: string;
  readonly session_id: string;
  readonly run_id: string;
}

export type EnqueueContextIntentV1 =
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
      /** The only client-selectable existing-answer value is an opaque row id. */
      readonly kind: "existing_answer";
      readonly source_run_id: string;
    }
  | {
      readonly kind: "routing_request";
      readonly input_hash: ContentHash;
      readonly typed_input: JsonValue;
    }
  | { readonly kind: "question_only" };

/** Safe host-facing enqueue intent. It has no canonical-source or memory carrier field. */
export interface EnqueueRunIntentV1 {
  readonly mutation_id: string;
  readonly run_kind: string;
  readonly locale: string;
  readonly question: string;
  readonly context: EnqueueContextIntentV1;
}

export interface CommittedAnswerSourceMaterializationRequestV1 extends JsonObject {
  readonly schema_version: 1;
  readonly tenant_id: string;
  readonly principal_id: string;
  readonly session_id: string;
  readonly requesting_run_id: string;
  readonly source_run_id: string;
}

/**
 * Receipt returned by trusted product storage after checking row ownership and
 * reading the canonical source from the committed final row.
 */
export interface CommittedAnswerSourceClaimV1 extends JsonObject {
  readonly schema_version: 1;
  readonly request_hash: ContentHash;
  readonly tenant_id: string;
  readonly principal_id: string;
  readonly session_id: string;
  readonly requesting_run_id: string;
  readonly source_run_id: string;
  readonly committed_source: CommittedAnswerSourceV1;
}

export interface CommittedAnswerSourceMaterializer {
  /** Must use a server DB connection and reject non-final or non-owned rows. */
  materializeCommittedAnswerSource(
    request: CommittedAnswerSourceMaterializationRequestV1,
  ): Promise<unknown>;
}

/**
 * Differential/audit claim emitted after the Rust worker obtains a run fence,
 * reads the DB delta chain, and builds its question-conditioned effective view.
 * This claim is never part of the public enqueue request.
 */
export interface SessionMemoryViewClaimV3 extends JsonObject {
  readonly schema_version: 3;
  readonly run_id_hash: ContentHash;
  readonly session_id_hash: ContentHash;
  readonly source_revision: number;
  readonly source_frontier_hash: ContentHash;
  readonly view_hash: ContentHash | null;
  readonly page_count: number;
  readonly delta_count: number;
  readonly carrier: SessionMemoryCarrierV3 | null;
}

export interface MaterializationPorts {
  readonly committedAnswers: CommittedAnswerSourceMaterializer;
}

export interface MaterializedRunInputs {
  readonly context: RunContextV1;
  readonly committedSourceClaimHash: ContentHash | null;
}

/** Exact `krw.session-memory/frontier-v2` transition used by the Rust ABI. */
export function sessionMemoryFrontierHash(
  parentHash: ContentHash,
  revision: number,
  deltaHash: ContentHash,
): ContentHash {
  if (!isContentHash(parentHash) || !isContentHash(deltaHash)) {
    throw new ContractViolation("invalid_session_memory_frontier_input_hash");
  }
  boundedInteger(revision, 1, Number.MAX_SAFE_INTEGER, "session_memory_revision");
  return bytesContentHash(
    Buffer.from(
      `krw.session-memory/frontier-v2\0${parentHash}\0${revision}\0${deltaHash}`,
      "utf8",
    ),
  );
}

export async function materializeRunInputs(
  ownership: AuthenticatedRunOwnershipV1,
  intent: EnqueueRunIntentV1,
  ports: MaterializationPorts,
): Promise<MaterializedRunInputs> {
  validateAuthenticatedRunOwnership(ownership);
  validateEnqueueRunIntent(intent);

  let context: RunContextV1;
  let committedSourceClaimHash: ContentHash | null = null;
  if (intent.context.kind === "existing_answer") {
    const request: CommittedAnswerSourceMaterializationRequestV1 = Object.freeze({
      schema_version: 1,
      tenant_id: ownership.tenant_id,
      principal_id: ownership.principal_id,
      session_id: ownership.session_id,
      requesting_run_id: ownership.run_id,
      source_run_id: intent.context.source_run_id,
    });
    const claim = parseCommittedAnswerSourceClaim(
      await ports.committedAnswers.materializeCommittedAnswerSource(request),
      request,
    );
    context = { kind: "existing_answer", committed_source: claim.committed_source };
    committedSourceClaimHash = canonicalHash(claim);
  } else {
    context = intent.context;
    validateRunContext(context);
  }

  return {
    context,
    committedSourceClaimHash,
  };
}

export function validateAuthenticatedRunOwnership(
  value: AuthenticatedRunOwnershipV1,
): void {
  exactKeys(value, ["schema_version", "tenant_id", "principal_id", "session_id", "run_id"]);
  if (value.schema_version !== 1) throw new ContractViolation("ownership_schema_version");
  validateBoundedIdentifier(value.tenant_id, "tenant_id");
  validateBoundedIdentifier(value.principal_id, "principal_id");
  validateBoundedIdentifier(value.session_id, "session_id");
  validateBoundedIdentifier(value.run_id, "run_id");
}

export function validateEnqueueRunIntent(value: EnqueueRunIntentV1): void {
  exactKeys(value, ["mutation_id", "run_kind", "locale", "question", "context"]);
  validateBoundedIdentifier(value.mutation_id, "mutation_id");
  validateBoundedIdentifier(value.run_kind, "run_kind");
  validateBoundedIdentifier(value.locale, "locale", 32);
  if (
    value.question.length === 0 ||
    value.question.includes("\0") ||
    Buffer.byteLength(value.question, "utf8") > MAX_QUESTION_BYTES
  ) {
    throw new ContractViolation("invalid_question");
  }
  validateContextIntent(value.context);
}

function validateContextIntent(value: EnqueueContextIntentV1): void {
  if (!isPlainObject(value) || typeof value.kind !== "string") {
    throw new ContractViolation("invalid_context_intent");
  }
  if (value.kind === "existing_answer") {
    exactKeys(value, ["kind", "source_run_id"]);
    validateBoundedIdentifier(value.source_run_id, "source_run_id");
    return;
  }
  validateRunContext(value as RunContextV1);
}

function parseCommittedAnswerSourceClaim(
  value: unknown,
  request: CommittedAnswerSourceMaterializationRequestV1,
): CommittedAnswerSourceClaimV1 {
  exactKeys(value, [
    "schema_version",
    "request_hash",
    "tenant_id",
    "principal_id",
    "session_id",
    "requesting_run_id",
    "source_run_id",
    "committed_source",
  ]);
  const claim = value as CommittedAnswerSourceClaimV1;
  if (claim.schema_version !== 1 || claim.request_hash !== canonicalHash(request)) {
    throw new ContractViolation("committed_source_claim_request_mismatch");
  }
  if (
    claim.tenant_id !== request.tenant_id ||
    claim.principal_id !== request.principal_id ||
    claim.session_id !== request.session_id ||
    claim.requesting_run_id !== request.requesting_run_id ||
    claim.source_run_id !== request.source_run_id
  ) {
    throw new ContractViolation("committed_source_claim_ownership_mismatch");
  }
  validateCommittedAnswerSource(claim.committed_source);
  return claim;
}

export function validateSessionMemoryViewClaim(
  value: SessionMemoryViewClaimV3,
  ownership: AuthenticatedRunOwnershipV1,
): void {
  exactKeys(value, [
    "schema_version",
    "run_id_hash",
    "session_id_hash",
    "source_revision",
    "source_frontier_hash",
    "view_hash",
    "page_count",
    "delta_count",
    "carrier",
  ]);
  validateAuthenticatedRunOwnership(ownership);
  if (
    value.schema_version !== 3 ||
    value.run_id_hash !== utf8ContentHash(ownership.run_id) ||
    value.session_id_hash !== utf8ContentHash(ownership.session_id)
  ) {
    throw new ContractViolation("session_memory_claim_ownership_mismatch");
  }
  boundedInteger(
    value.source_revision,
    0,
    Number.MAX_SAFE_INTEGER,
    "session_memory_claim_revision",
  );
  boundedInteger(value.page_count, 1, 64, "session_memory_page_count");
  boundedInteger(value.delta_count, 0, 65_535, "session_memory_delta_count");
  if (!isContentHash(value.source_frontier_hash)) {
    throw new ContractViolation("invalid_session_memory_claim_frontier");
  }
  if (value.source_revision === 0) {
    if (value.view_hash !== null || value.carrier !== null || value.delta_count !== 0) {
      throw new ContractViolation("session_memory_empty_claim_mismatch");
    }
    return;
  }
  if (!isContentHash(value.view_hash) || value.carrier === null) {
    throw new ContractViolation("session_memory_claim_view_missing");
  }
  validateSessionMemoryCarrier(value.carrier);
  if (
    value.carrier.schema_version !== SESSION_MEMORY_CARRIER_SCHEMA_VERSION ||
    value.carrier.source_revision !== value.source_revision ||
    value.carrier.source_frontier_hash !== value.source_frontier_hash ||
    value.carrier.view_hash !== value.view_hash
  ) {
    throw new ContractViolation("session_memory_claim_carrier_mismatch");
  }
  validateCanonicalSessionMemoryView(value.carrier, ownership.session_id);
}

function validateCanonicalSessionMemoryView(
  carrier: SessionMemoryCarrierV3,
  sessionId: string,
): void {
  const view = carrier.canonical_view;
  exactKeys(view, [
    "schema_version",
    "authority",
    "session_id_hash",
    "source_frontier_hash",
    "source_revision",
    "view_hash",
    "tickers",
    "constraints",
    "sources",
    "claims",
    "unresolved_goals",
    "recent_turns",
  ]);
  if (
    view.schema_version !== 3 ||
    view.authority !== "context_only" ||
    view.session_id_hash !== utf8ContentHash(sessionId) ||
    view.source_frontier_hash !== carrier.source_frontier_hash ||
    view.source_revision !== carrier.source_revision ||
    !isContentHash(view.view_hash)
  ) {
    throw new ContractViolation("session_memory_view_identity_mismatch");
  }
  const { view_hash: _semanticViewHash, ...semanticInput } = view;
  if (canonicalHash(semanticInput) !== view.view_hash) {
    throw new ContractViolation("session_memory_semantic_view_hash_mismatch");
  }
}

function exactKeys(value: unknown, expected: readonly string[]): asserts value is JsonObject {
  if (!isPlainObject(value)) throw new ContractViolation("expected_plain_object");
  const actual = Object.keys(value).sort();
  const wanted = [...expected].sort();
  if (actual.length !== wanted.length || actual.some((key, index) => key !== wanted[index])) {
    throw new ContractViolation("unknown_or_missing_materialization_field");
  }
}

function boundedInteger(value: unknown, min: number, max: number, field: string): void {
  if (!Number.isSafeInteger(value) || (value as number) < min || (value as number) > max) {
    throw new ContractViolation(`invalid_${field}`);
  }
}

function isPlainObject(value: unknown): value is JsonObject {
  if (value === null || typeof value !== "object" || Array.isArray(value)) return false;
  const prototype = Object.getPrototypeOf(value);
  return prototype === Object.prototype || prototype === null;
}
