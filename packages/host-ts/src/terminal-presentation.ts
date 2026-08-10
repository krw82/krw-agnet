/**
 * Small, public projections for terminal agent runs.
 *
 * The kernel owns the durable execution record.  This module deliberately
 * exposes only the counters a product needs for credit settlement and a
 * human-readable retry message for a terminal failure.  It never forwards
 * provider text, prompts, tool payloads, or arbitrary database diagnostics.
 */
import type { JsonObject, JsonValue } from "./json.js";

export interface PublicRunUsage extends JsonObject {
  readonly provider_turns: number;
  readonly capability_calls: number;
  readonly repairs: number;
  readonly input_tokens: number;
  readonly output_tokens: number;
  readonly total_tokens: number;
  readonly provider_total_ms: number;
  readonly capability_total_ms: number;
}

export type RetryCategory =
  | "model_response"
  | "data_connection"
  | "processing_limit"
  | "service_setup"
  | "temporary_processing";

export interface RetryMessage extends JsonObject {
  readonly category: RetryCategory;
  readonly retry_recommended: true;
  readonly markdown: string;
}

const MAX_COUNTER = 0xffff_ffff;

/**
 * Project kernel-recorded usage into the stable, product-facing counters.
 * `total_tokens` is derived here so billing consumers never need to assume a
 * provider-specific usage shape.
 */
export function projectPublicRunUsage(value: JsonValue): PublicRunUsage {
  if (!isPlainObject(value)) throw new TypeError("terminal_usage_not_object");
  const providerTurns = nonnegativeCounter(value.provider_turns, "provider_turns");
  const capabilityCalls = nonnegativeCounter(value.capability_calls, "capability_calls");
  const repairs = nonnegativeCounter(value.repairs, "repairs");
  const inputTokens = nonnegativeCounter(value.input_tokens, "input_tokens");
  const outputTokens = nonnegativeCounter(value.output_tokens, "output_tokens");
  const providerTotalMs = nonnegativeCounter(value.provider_total_ms, "provider_total_ms");
  const capabilityTotalMs = nonnegativeCounter(value.capability_total_ms, "capability_total_ms");
  const totalTokens = inputTokens + outputTokens;
  if (!Number.isSafeInteger(totalTokens) || totalTokens > MAX_COUNTER) {
    throw new TypeError("terminal_usage_total_tokens");
  }
  return {
    provider_turns: providerTurns,
    capability_calls: capabilityCalls,
    repairs,
    input_tokens: inputTokens,
    output_tokens: outputTokens,
    total_tokens: totalTokens,
    provider_total_ms: providerTotalMs,
    capability_total_ms: capabilityTotalMs,
  };
}

/**
 * Convert a terminal reason code into a safe Markdown retry instruction.
 * Reason codes are deliberately bucketed; unrecognised database text never
 * crosses this boundary.
 */
export function retryMessageForTerminalFailure(value: JsonValue | null): RetryMessage {
  const category = retryCategory(value);
  const explanation = {
    model_response: "분석 과정에서 모델 응답을 해석하거나 복구하는 단계가 끝까지 완료되지 않았습니다.",
    data_connection: "분석에 필요한 자료 조회 또는 모델 연결이 일시적으로 완료되지 않았습니다.",
    processing_limit: "이번 질문이 현재 실행 시간이나 분석 한도를 초과했습니다.",
    service_setup: "분석 실행 환경을 준비하는 단계에서 문제가 발생했습니다.",
    temporary_processing: "분석 처리 과정이 예기치 않게 중단됐습니다.",
  }[category];
  const nextStep = category === "processing_limit"
    ? "질문 범위를 조금 좁혀 다시 요청해 주세요."
    : "같은 질문을 다시 요청하면 새 실행으로 재시도합니다.";
  return {
    category,
    retry_recommended: true,
    markdown: `## 분석을 완료하지 못했습니다\n\n${explanation}\n\n${nextStep}`,
  };
}

function retryCategory(value: JsonValue | null): RetryCategory {
  const reason = terminalReasonCode(value);
  if (reason === "budget_exhausted" || reason === "engine_size_limit") {
    return "processing_limit";
  }
  if (
    reason === "provider_protocol_failure"
    || reason === "model_proposal_rejected"
    || reason === "provider_model_not_loaded"
  ) {
    return "model_response";
  }
  if (
    reason === "deferred_attempts_exhausted"
    || reason.startsWith("capability_")
    || reason.startsWith("provider_")
  ) {
    return "data_connection";
  }
  if (
    reason.startsWith("engine_input_")
    || reason === "canonical_contract_failure"
    || reason === "recovery_integrity_failure"
  ) {
    return "service_setup";
  }
  return "temporary_processing";
}

function terminalReasonCode(value: JsonValue | null): string {
  if (!isPlainObject(value)) return "";
  const reason = value.reason_code;
  return typeof reason === "string" && /^[a-z0-9_]{1,128}$/.test(reason) ? reason : "";
}

function nonnegativeCounter(value: JsonValue | undefined, name: string): number {
  if (
    typeof value !== "number"
    || !Number.isSafeInteger(value)
    || value < 0
    || value > MAX_COUNTER
  ) {
    throw new TypeError(`terminal_usage_${name}`);
  }
  return value;
}

function isPlainObject(value: JsonValue): value is JsonObject {
  return value !== null && !Array.isArray(value) && typeof value === "object";
}
