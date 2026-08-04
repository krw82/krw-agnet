import type { CommittedAnswerSourceV1 } from "./contracts.js";
import {
  canonicalHash,
  canonicalJson,
  isContentHash,
  type ContentHash,
  type JsonObject,
} from "./json.js";
import {
  ContractViolation,
  validateBoundedIdentifier,
} from "./validation.js";

const MAX_CANONICAL_SOURCE_BYTES = 1024 * 1024;

export type CanonicalUnitType =
  | "heading"
  | "paragraph"
  | "metric"
  | "period_delta"
  | "business_segment"
  | "premise"
  | "impact_channel"
  | "risk_item"
  | "driver_item"
  | "headwind_item"
  | "watch_item"
  | "timeline_event"
  | "comparison_row"
  | "caveat"
  | "glossary_term"
  | "evidence_note";

export interface CanonicalDisplayUnitV1 extends JsonObject {
  readonly unit_id: string;
  readonly unit_type: CanonicalUnitType;
  readonly importance: "required" | "supporting" | "optional";
  readonly confidence: "direct" | "indirect" | "inferred" | "unsupported";
  readonly summary: string;
  readonly content_hash: ContentHash;
  readonly claim_ids: readonly string[];
  readonly evidence_ids: readonly string[];
}

export interface CanonicalDisplaySourceV1 extends JsonObject {
  readonly schema_version: 1;
  readonly final_receipt_hash: ContentHash;
  readonly answer_bundle_hash: ContentHash;
  readonly answer_ir_hash: ContentHash;
  readonly locale: "ko-KR" | "en-US";
  readonly default_order: readonly string[];
  readonly units: readonly CanonicalDisplayUnitV1[];
}

export interface ExpectedCommittedHashes {
  readonly finalReceiptHash: ContentHash;
  readonly answerBundleHash: ContentHash;
  readonly answerIrHash: ContentHash;
}

/** Converts DB materialized canonical JCS into the exact protocol carrier. */
export function committedSourceFromCanonicalJcs(
  canonicalSourceJcs: string,
  expected: ExpectedCommittedHashes,
): CommittedAnswerSourceV1 {
  if (
    canonicalSourceJcs.length === 0 ||
    canonicalSourceJcs.includes("\0") ||
    Buffer.byteLength(canonicalSourceJcs, "utf8") > MAX_CANONICAL_SOURCE_BYTES
  ) {
    throw new ContractViolation("canonical_source_jcs_bounds");
  }
  let parsed: unknown;
  try {
    parsed = JSON.parse(canonicalSourceJcs);
  } catch {
    throw new ContractViolation("canonical_source_invalid_json");
  }
  if (!isPlainObject(parsed) || canonicalJson(parsed) !== canonicalSourceJcs) {
    throw new ContractViolation("canonical_source_not_jcs");
  }
  const source = validateCanonicalDisplaySource(parsed);
  if (
    source.final_receipt_hash !== expected.finalReceiptHash ||
    source.answer_bundle_hash !== expected.answerBundleHash ||
    source.answer_ir_hash !== expected.answerIrHash
  ) {
    throw new ContractViolation("canonical_source_final_hash_mismatch");
  }
  const sourceUnitHashes = Object.fromEntries(
    source.units.map((unit) => [unit.unit_id, unit.content_hash]),
  );
  return {
    schema_version: 1,
    final_receipt_hash: source.final_receipt_hash,
    answer_bundle_hash: source.answer_bundle_hash,
    answer_ir_hash: source.answer_ir_hash,
    canonical_source_hash: canonicalHash(source),
    source_unit_hashes: sourceUnitHashes,
    canonical_source: source,
  };
}

export function validateCommittedAnswerSource(
  value: unknown,
): asserts value is CommittedAnswerSourceV1 {
  exactKeys(value, [
    "schema_version",
    "final_receipt_hash",
    "answer_bundle_hash",
    "answer_ir_hash",
    "canonical_source_hash",
    "source_unit_hashes",
    "canonical_source",
  ]);
  if (!isPlainObject(value)) throw new ContractViolation("committed_source_object");
  if (value.schema_version !== 1) throw new ContractViolation("committed_source_version");
  requireHash(value.final_receipt_hash, "final_receipt_hash");
  requireHash(value.answer_bundle_hash, "answer_bundle_hash");
  requireHash(value.answer_ir_hash, "answer_ir_hash");
  requireHash(value.canonical_source_hash, "canonical_source_hash");
  if (!isPlainObject(value.source_unit_hashes)) {
    throw new ContractViolation("source_unit_hashes_object");
  }
  const source = validateCanonicalDisplaySource(value.canonical_source);
  const expectedUnitHashes = Object.fromEntries(
    source.units.map((unit) => [unit.unit_id, unit.content_hash]),
  );
  if (
    canonicalHash(source) !== value.canonical_source_hash ||
    canonicalJson(value.source_unit_hashes) !== canonicalJson(expectedUnitHashes) ||
    source.final_receipt_hash !== value.final_receipt_hash ||
    source.answer_bundle_hash !== value.answer_bundle_hash ||
    source.answer_ir_hash !== value.answer_ir_hash
  ) {
    throw new ContractViolation("committed_source_commitment_mismatch");
  }
}

export function validateCanonicalDisplaySource(
  value: unknown,
): CanonicalDisplaySourceV1 {
  exactKeys(value, [
    "schema_version",
    "final_receipt_hash",
    "answer_bundle_hash",
    "answer_ir_hash",
    "locale",
    "default_order",
    "units",
  ]);
  if (!isPlainObject(value)) throw new ContractViolation("canonical_source_object");
  if (value.schema_version !== 1) throw new ContractViolation("canonical_source_version");
  requireHash(value.final_receipt_hash, "final_receipt_hash");
  requireHash(value.answer_bundle_hash, "answer_bundle_hash");
  requireHash(value.answer_ir_hash, "answer_ir_hash");
  if (value.locale !== "ko-KR" && value.locale !== "en-US") {
    throw new ContractViolation("canonical_source_locale");
  }
  if (!Array.isArray(value.units) || value.units.length < 1 || value.units.length > 64) {
    throw new ContractViolation("canonical_source_unit_count");
  }
  if (!Array.isArray(value.default_order) || value.default_order.length !== value.units.length) {
    throw new ContractViolation("canonical_source_order_count");
  }

  const units = value.units.map(validateCanonicalUnit);
  const unitIds = units.map((unit) => unit.unit_id);
  if (
    new Set(unitIds).size !== unitIds.length ||
    new Set(value.default_order).size !== value.default_order.length ||
    value.default_order.some(
      (unitId) => typeof unitId !== "string" || !unitIds.includes(unitId),
    )
  ) {
    throw new ContractViolation("canonical_source_order_mismatch");
  }
  return value as unknown as CanonicalDisplaySourceV1;
}

function validateCanonicalUnit(value: unknown): CanonicalDisplayUnitV1 {
  exactKeys(value, [
    "unit_id",
    "unit_type",
    "importance",
    "confidence",
    "summary",
    "content_hash",
    "claim_ids",
    "evidence_ids",
  ]);
  if (!isPlainObject(value)) throw new ContractViolation("canonical_unit_object");
  if (typeof value.unit_id !== "string") throw new ContractViolation("canonical_unit_id");
  validateBoundedIdentifier(value.unit_id, "canonical_unit_id");
  if (!UNIT_TYPES.has(value.unit_type as CanonicalUnitType)) {
    throw new ContractViolation("canonical_unit_type");
  }
  if (value.importance !== "required" && value.importance !== "supporting" && value.importance !== "optional") {
    throw new ContractViolation("canonical_unit_importance");
  }
  if (
    value.confidence !== "direct" &&
    value.confidence !== "indirect" &&
    value.confidence !== "inferred" &&
    value.confidence !== "unsupported"
  ) {
    throw new ContractViolation("canonical_unit_confidence");
  }
  if (
    typeof value.summary !== "string" ||
    value.summary.length === 0 ||
    value.summary.includes("\0") ||
    Buffer.byteLength(value.summary, "utf8") > 1_000
  ) {
    throw new ContractViolation("canonical_unit_summary");
  }
  requireHash(value.content_hash, "canonical_unit_content_hash");
  validateIdArray(value.claim_ids, "canonical_unit_claim_ids");
  validateIdArray(value.evidence_ids, "canonical_unit_evidence_ids");
  return value as unknown as CanonicalDisplayUnitV1;
}

function validateIdArray(value: unknown, field: string): void {
  if (!Array.isArray(value) || value.length > 64 || new Set(value).size !== value.length) {
    throw new ContractViolation(field);
  }
  for (const item of value) {
    if (typeof item !== "string") throw new ContractViolation(field);
    validateBoundedIdentifier(item, field);
  }
}

function exactKeys(value: unknown, expected: readonly string[]): void {
  if (!isPlainObject(value)) throw new ContractViolation("expected_plain_object");
  const actual = Object.keys(value).sort();
  const wanted = [...expected].sort();
  if (actual.length !== wanted.length || actual.some((key, index) => key !== wanted[index])) {
    throw new ContractViolation("unknown_or_missing_committed_source_field");
  }
}

function requireHash(value: unknown, field: string): asserts value is ContentHash {
  if (!isContentHash(value)) throw new ContractViolation(`invalid_${field}`);
}

function isPlainObject(value: unknown): value is JsonObject {
  if (value === null || typeof value !== "object" || Array.isArray(value)) return false;
  const prototype = Object.getPrototypeOf(value);
  return prototype === Object.prototype || prototype === null;
}

const UNIT_TYPES = new Set<CanonicalUnitType>([
  "heading",
  "paragraph",
  "metric",
  "period_delta",
  "business_segment",
  "premise",
  "impact_channel",
  "risk_item",
  "driver_item",
  "headwind_item",
  "watch_item",
  "timeline_event",
  "comparison_row",
  "caveat",
  "glossary_term",
  "evidence_note",
]);
