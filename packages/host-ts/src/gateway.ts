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
 * Browser/CLI body for the ticker-less open-research route (the free door).
 * The question text is the only input: no ticker, no advisor lens — an
 * advisor lens without a company is a category error and must fail closed.
 */
export interface GatewayOpenResearchRequestV1 extends JsonObject {
  readonly schema_version: typeof GATEWAY_SCHEMA_VERSION;
  readonly question: string;
}

/**
 * The single POST /runs body, discriminated by door: `ticker` key presence
 * selects the company door, its absence the open-research free door. The
 * explicit tag keeps narrowing reliable even though both bodies extend the
 * index-signature `JsonObject`.
 */
export type GatewayRunRequestV1 =
  | { readonly door: "company"; readonly request: GatewayCompanyResearchRequestV1 }
  | { readonly door: "open_research"; readonly request: GatewayOpenResearchRequestV1 };

/** Parse the discriminated public body (ticker key presence = company door). */
export function parseGatewayRunRequest(value: unknown): GatewayRunRequestV1 {
  if (!isPlainObject(value)) throw new ContractViolation("gateway_request_object");
  return Object.hasOwn(value, "ticker")
    ? {
        door: "company" as const,
        request: parseGatewayCompanyResearchRequest(value),
      }
    : {
        door: "open_research" as const,
        request: parseGatewayOpenResearchRequest(value),
      };
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
 * Server-only input for the free door. Same ownership rules as the company
 * preparation; the run kind is always `open_research`.
 */
export interface PrepareGatewayOpenResearchInput
  extends Omit<PrepareEnqueueRunInput, "intent"> {
  readonly mutation_id: string;
  readonly request: GatewayOpenResearchRequestV1;
}

/**
 * Convert a ticker-less question into the open-research enqueue intent.
 * Locale routing degrades gracefully: Latin-script questions only reach an
 * English entrypoint when the pinned release actually publishes one — until
 * `open_research_en` ships, they stay on the Korean image (the same
 * degradation the company route applies to old pins).
 */
export async function prepareGatewayOpenResearch(
  input: PrepareGatewayOpenResearchInput,
): Promise<PreparedEnqueueRunV1> {
  const request = parseGatewayOpenResearchRequest(input.request);
  const intent: EnqueueRunIntentV1 = {
    mutation_id: input.mutation_id,
    run_kind: "open_research",
    locale: openResearchLocaleForQuestion(
      request.question,
      input.artifact.descriptor,
    ),
    question: request.question,
    context: { kind: "question_only" },
  };
  return prepareEnqueueRun({
    artifact: input.artifact,
    ownership: input.ownership,
    intent,
    materializers: input.materializers,
  });
}

/** Deterministic locale routing for the free door (see note above). */
export function openResearchLocaleForQuestion(
  question: string,
  descriptor: unknown,
): "ko-KR" | "en-US" {
  if (/\p{Script=Hangul}/u.test(question)) return "ko-KR";
  if (/[A-Za-z]/.test(question) && releaseHasOpenResearchEntrypoint(descriptor, "en-US")) {
    return "en-US";
  }
  return "ko-KR";
}

function releaseHasOpenResearchEntrypoint(
  descriptor: unknown,
  locale: string,
): boolean {
  if (!isPlainObject(descriptor) || !Array.isArray(descriptor.entries)) return false;
  return descriptor.entries.some(
    (entry) =>
      isPlainObject(entry) &&
      entry.run_kind === "open_research" &&
      entry.locale === locale,
  );
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

/** Parse the ticker-less open-research body. No future fields by accident. */
export function parseGatewayOpenResearchRequest(
  value: unknown,
): GatewayOpenResearchRequestV1 {
  if (!isPlainObject(value)) throw new ContractViolation("gateway_request_object");
  allowedKeys(value, ["schema_version", "question"], ["schema_version", "question"]);
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
  const context = { kind: "question_only" as const };
  validateRunContext(context);
  const parsed: GatewayOpenResearchRequestV1 = {
    schema_version: GATEWAY_SCHEMA_VERSION,
    question: value.question,
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
