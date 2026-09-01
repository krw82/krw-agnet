/**
 * Framework-neutral preparation for the narrow public Agent Gateway surface.
 *
 * A web route authenticates the caller and creates the server-owned IDs first.
 * This helper then converts only `{ question, ticker, advisor_lens? }` into the
 * existing immutable enqueue contract. It deliberately exposes no model,
 * capability, release, budget, ownership, or session-memory input to the caller.
 */
import {
  prepareEnqueueRun,
  type PrepareEnqueueRunInput,
  type PreparedEnqueueRunV1,
} from "./ownership.js";
import type { EnqueueRunIntentV1 } from "./materialization.js";
import type { JsonObject } from "./json.js";
import { ContractViolation, validateRunContext } from "./validation.js";

const GATEWAY_SCHEMA_VERSION = 1 as const;
const MAX_QUESTION_BYTES = 64 * 1024;

/** Public Guru choices. The host maps these to sealed AgentSpec run kinds. */
export type GatewayAdvisorLens = "ackman" | "buffett" | "flatt" | "marks" | "terry_smith";

const GURU_RUN_KIND_BY_LENS: Readonly<Record<GatewayAdvisorLens, string>> = {
  ackman: "guru_ackman",
  buffett: "guru_buffett",
  flatt: "guru_flatt",
  marks: "guru_marks",
  terry_smith: "guru_terry_smith",
};

/** Browser/CLI body for the company-research-only v1 Gateway route. */
export interface GatewayCompanyResearchRequestV1 extends JsonObject {
  readonly schema_version: typeof GATEWAY_SCHEMA_VERSION;
  readonly question: string;
  readonly ticker: string;
  readonly advisor_lens?: GatewayAdvisorLens;
}

/**
 * Server-only input. `ownership` and `mutation_id` must come from
 * authentication/conversation storage and a server ID generator, never from
 * `GatewayCompanyResearchRequestV1`.
 */
export interface PrepareGatewayCompanyResearchInput
  extends Omit<PrepareEnqueueRunInput, "intent"> {
  readonly mutation_id: string;
  readonly request: GatewayCompanyResearchRequestV1;
}

/**
 * Convert the deliberately small Gateway request to the exact normal host
 * enqueue route. A future HTTP handler calls this and then
 * `HostAgentClient.enqueue`; it must not construct its own claim.
 */
export async function prepareGatewayCompanyResearch(
  input: PrepareGatewayCompanyResearchInput,
): Promise<PreparedEnqueueRunV1> {
  const request = parseGatewayCompanyResearchRequest(input.request);
  const locale = gatewayLocaleForQuestion(
    request.question,
    input.artifact.descriptor,
  );
  const intent: EnqueueRunIntentV1 = {
    mutation_id: input.mutation_id,
    run_kind: request.advisor_lens
      ? GURU_RUN_KIND_BY_LENS[request.advisor_lens]
      : locale === "en-US"
        ? "company_research_en"
        : "company_research",
    locale,
    question: request.question,
    context: { kind: "company_ticker_set", tickers: [request.ticker] },
  };
  return prepareEnqueueRun({
    artifact: input.artifact,
    ownership: input.ownership,
    intent,
    materializers: input.materializers,
  });
}

/**
 * Deterministic locale routing for the company-research entrypoint: the
 * Korean and English agent images share one gateway, and the entrypoint
 * previously hardcoded ko-KR, so an English question could never reach the
 * English image (2026-09-02 quality loop). Rule: Hangul anywhere routes
 * ko-KR; otherwise Latin letters route en-US, and anything else keeps the
 * ko-KR default.
 */
export function gatewayLocaleForQuestion(
  question: string,
  descriptor: unknown,
): "ko-KR" | "en-US" {
  if (/\p{Script=Hangul}/u.test(question)) return "ko-KR";
  if (/[A-Za-z]/.test(question) && releaseHasEnglishEntrypoint(descriptor)) {
    return "en-US";
  }
  return "ko-KR";
}

/**
 * The pinned release artifact decides whether an English entrypoint exists
 * (the English image declares run_kind `company_research_en`, not a locale
 * variant of `company_research`). Older pins carry only the Korean image, so
 * an English question must keep routing to the Korean image instead of
 * failing the enqueue (2026-09-02 loop: a live English submit returned 400
 * against the month-old pin).
 */
function releaseHasEnglishEntrypoint(descriptor: unknown): boolean {
  if (!isPlainObject(descriptor) || !Array.isArray(descriptor.entries)) return false;
  return descriptor.entries.some(
    (entry) =>
      isPlainObject(entry) &&
      entry.run_kind === "company_research_en" &&
      entry.locale === "en-US",
  );
}

/** Parse unknown HTTP JSON without accepting any future fields by accident. */
export function parseGatewayCompanyResearchRequest(
  value: unknown,
): GatewayCompanyResearchRequestV1 {
  if (!isPlainObject(value)) throw new ContractViolation("gateway_request_object");
  allowedKeys(
    value,
    ["schema_version", "question", "ticker", "advisor_lens"],
    ["schema_version", "question", "ticker"],
  );
  if (value.schema_version !== GATEWAY_SCHEMA_VERSION) {
    throw new ContractViolation("gateway_request_schema_version");
  }
  if (
    typeof value.question !== "string" ||
    value.question.length === 0 ||
    value.question.includes("\0") ||
    Buffer.byteLength(value.question, "utf8") > MAX_QUESTION_BYTES
  ) {
    throw new ContractViolation("gateway_request_question");
  }
  if (typeof value.ticker !== "string") {
    throw new ContractViolation("gateway_request_ticker");
  }
  const advisorLens = value.advisor_lens;
  if (advisorLens !== undefined && !isGatewayAdvisorLens(advisorLens)) {
    throw new ContractViolation("gateway_request_advisor_lens");
  }
  const context = { kind: "company_ticker_set" as const, tickers: [value.ticker] };
  validateRunContext(context);
  const parsed: GatewayCompanyResearchRequestV1 = {
    schema_version: GATEWAY_SCHEMA_VERSION,
    question: value.question,
    ticker: value.ticker,
    ...(advisorLens === undefined ? {} : { advisor_lens: advisorLens }),
  };
  return Object.freeze(parsed);
}

function isGatewayAdvisorLens(value: unknown): value is GatewayAdvisorLens {
  return (
    typeof value === "string" &&
    Object.hasOwn(GURU_RUN_KIND_BY_LENS, value)
  );
}

function allowedKeys(
  value: JsonObject,
  allowed: readonly string[],
  required: readonly string[],
): void {
  const allowedSet = new Set(allowed);
  const actual = Object.keys(value);
  if (actual.some((key) => !allowedSet.has(key))) {
    throw new ContractViolation("gateway_request_unknown_or_missing_field");
  }
  if (required.some((key) => !Object.hasOwn(value, key))) {
    throw new ContractViolation("gateway_request_unknown_or_missing_field");
  }
}

function isPlainObject(value: unknown): value is JsonObject {
  if (value === null || typeof value !== "object" || Array.isArray(value)) return false;
  const prototype = Object.getPrototypeOf(value);
  return prototype === Object.prototype || prototype === null;
}
