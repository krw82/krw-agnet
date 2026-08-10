import { AGENT_V1_ABI_VERSION } from "./contracts.js";
import { canonicalHash, type JsonObject } from "./json.js";
import { ContractViolation } from "./validation.js";

export type HostProcedure =
  | "agent_v1.enqueue_run"
  | "agent_v1.request_cancel"
  | "agent_v1.read_committed_outcome"
  | "agent_v1.read_final_output"
  | "agent_v1.read_final_projection"
  | "agent_v1.read_terminal_trace";

export type OutboxProcedure = "agent_v1.claim_outbox" | "agent_v1.ack_outbox";

export function encodeProcedureRequest(
  procedure: HostProcedure,
  request: JsonObject,
): JsonObject {
  if (Object.hasOwn(request, "abi_version") || Object.hasOwn(request, "mutation_hash")) {
    throw new ContractViolation("reserved_abi_field");
  }
  const withVersion: JsonObject = { ...request, abi_version: AGENT_V1_ABI_VERSION };
  if (!Object.hasOwn(withVersion, "mutation_id")) return withVersion;
  const mutationHash = canonicalHash({ procedure, request: withVersion });
  return { ...withVersion, mutation_hash: mutationHash };
}
