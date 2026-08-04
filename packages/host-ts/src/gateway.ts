/**
 * Framework-neutral preparation for the narrow public Agent Gateway surface.
 *
 * A web route authenticates the caller and creates the server-owned IDs first.
 * This helper then converts only `{ question, ticker }` into the existing
 * immutable enqueue contract. It deliberately exposes no model, capability,
 * release, budget, ownership, or session-memory input to the caller.
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

/** Browser/CLI body for the company-research-only v1 Gateway route. */
export interface GatewayCompanyResearchRequestV1 extends JsonObject {
  readonly schema_version: typeof GATEWAY_SCHEMA_VERSION;
  readonly question: string;
  readonly ticker: string;
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
  const intent: EnqueueRunIntentV1 = {
    mutation_id: input.mutation_id,
    run_kind: "company_research",
    locale: "ko-KR",
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

/** Parse unknown HTTP JSON without accepting any future fields by accident. */
export function parseGatewayCompanyResearchRequest(
  value: unknown,
): GatewayCompanyResearchRequestV1 {
  if (!isPlainObject(value)) throw new ContractViolation("gateway_request_object");
  exactKeys(value, ["schema_version", "question", "ticker"]);
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
  const context = { kind: "company_ticker_set" as const, tickers: [value.ticker] };
  validateRunContext(context);
  return Object.freeze({
    schema_version: GATEWAY_SCHEMA_VERSION,
    question: value.question,
    ticker: value.ticker,
  });
}

function exactKeys(value: JsonObject, expected: readonly string[]): void {
  const actual = Object.keys(value).sort();
  const wanted = [...expected].sort();
  if (actual.length !== wanted.length || actual.some((key, index) => key !== wanted[index])) {
    throw new ContractViolation("gateway_request_unknown_or_missing_field");
  }
}

function isPlainObject(value: unknown): value is JsonObject {
  if (value === null || typeof value !== "object" || Array.isArray(value)) return false;
  const prototype = Object.getPrototypeOf(value);
  return prototype === Object.prototype || prototype === null;
}
