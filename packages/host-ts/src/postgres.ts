import type { JsonProcedureTransport } from "./client.js";
import type { HostProcedure, OutboxProcedure } from "./procedure.js";
import { canonicalHash, type JsonObject } from "./json.js";
import { ContractViolation, isJsonValue } from "./validation.js";

export interface PgQueryResult<Row> {
  readonly rowCount: number | null;
  readonly rows: readonly Row[];
}

export interface PgQueryClient {
  query<Row>(config: {
    readonly name: string;
    readonly text: string;
    readonly values: readonly unknown[];
  }): Promise<PgQueryResult<Row>>;
}

export class RedactedDatabaseError extends Error {
  constructor(
    readonly procedure: HostProcedure | OutboxProcedure,
    readonly sqlstate: string | null,
    readonly diagnosticHash: string,
  ) {
    super(`database procedure failed: ${procedure}`);
    this.name = "RedactedDatabaseError";
  }
}

const HOST_STATEMENTS = Object.freeze({
  "agent_v1.enqueue_run": {
    name: "krw_host_agent_v1_enqueue_run",
    text: "SELECT agent_v1.enqueue_run($1::jsonb) AS result",
  },
  "agent_v1.request_cancel": {
    name: "krw_host_agent_v1_request_cancel",
    text: "SELECT agent_v1.request_cancel($1::jsonb) AS result",
  },
  "agent_v1.read_committed_outcome": {
    name: "krw_host_agent_v1_read_committed_outcome",
    text: "SELECT agent_v1.read_committed_outcome($1::jsonb) AS result",
  },
  "agent_v1.read_final_output": {
    name: "krw_host_agent_v1_read_final_output",
    text: "SELECT agent_v1.read_final_output($1::jsonb) AS result",
  },
  "agent_v1.read_final_projection": {
    name: "krw_host_agent_v1_read_final_projection",
    text: "SELECT agent_v1.read_final_projection($1::jsonb) AS result",
  },
} satisfies Record<HostProcedure, Statement>);

const OUTBOX_STATEMENTS = Object.freeze({
  "agent_v1.claim_outbox": {
    name: "krw_host_agent_v1_claim_outbox",
    text: "SELECT agent_v1.claim_outbox($1::jsonb) AS result",
  },
  "agent_v1.ack_outbox": {
    name: "krw_host_agent_v1_ack_outbox",
    text: "SELECT agent_v1.ack_outbox($1::jsonb) AS result",
  },
} satisfies Record<OutboxProcedure, Statement>);

interface Statement {
  readonly name: string;
  readonly text: string;
}

export class HostPostgresTransport implements JsonProcedureTransport<HostProcedure> {
  constructor(private readonly client: PgQueryClient) {}

  execute(procedure: HostProcedure, request: JsonObject): Promise<unknown> {
    return executeFixed(this.client, procedure, HOST_STATEMENTS[procedure], request);
  }
}

export class OutboxPostgresTransport implements JsonProcedureTransport<OutboxProcedure> {
  constructor(private readonly client: PgQueryClient) {}

  execute(procedure: OutboxProcedure, request: JsonObject): Promise<unknown> {
    return executeFixed(this.client, procedure, OUTBOX_STATEMENTS[procedure], request);
  }
}

async function executeFixed<P extends HostProcedure | OutboxProcedure>(
  client: PgQueryClient,
  procedure: P,
  statement: Statement,
  request: JsonObject,
): Promise<unknown> {
  try {
    const result = await client.query<{ readonly result: unknown }>({
      name: statement.name,
      text: statement.text,
      values: [request],
    });
    if (result.rowCount !== 1 || result.rows.length !== 1) {
      throw new ContractViolation("database_response_cardinality");
    }
    const row = result.rows[0];
    if (!row || Object.keys(row).length !== 1 || !Object.hasOwn(row, "result")) {
      throw new ContractViolation("database_response_shape");
    }
    if (!isJsonValue(row.result)) throw new ContractViolation("database_result_not_json");
    return row.result;
  } catch (error) {
    if (error instanceof ContractViolation) throw error;
    const sqlstate = databaseCode(error);
    throw new RedactedDatabaseError(
      procedure,
      sqlstate,
      canonicalHash(redactedDiagnostic(error, sqlstate)),
    );
  }
}

function databaseCode(error: unknown): string | null {
  if (error === null || typeof error !== "object") return null;
  const code = Reflect.get(error, "code");
  return typeof code === "string" && /^[A-Z0-9]{5}$/.test(code) ? code : null;
}

function redactedDiagnostic(error: unknown, sqlstate: string | null): JsonObject {
  return {
    class:
      error instanceof Error && error.name.length > 0 && error.name.length <= 128
        ? error.name
        : "UnknownDatabaseFailure",
    sqlstate,
  };
}
