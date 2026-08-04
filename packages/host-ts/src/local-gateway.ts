/**
 * Loopback-only operator Gateway for the standalone KRW Agent runtime.
 *
 * It is deliberately separate from the web app: the browser is not mounted
 * here.  The service accepts the public v1 CLI contract, owns a tiny local
 * session/run registry, calls only fixed `agent_v1` procedures through the
 * host adapter, and exposes committed final Markdown only after atomic final.
 */
import { createServer, type IncomingMessage, type ServerResponse } from "node:http";
import { randomUUID, timingSafeEqual } from "node:crypto";
import { readFile } from "node:fs/promises";
import { isAbsolute, normalize } from "node:path";

import { Pool, type PoolClient } from "pg";

import {
  ContractViolation,
  HostAgentClient,
  HostPostgresTransport,
  loadPinnedReleaseArtifact,
  parseGatewayCompanyResearchRequest,
  prepareGatewayCompanyResearch,
  type JsonObject,
  type MaterializationPorts,
  type PgQueryClient,
} from "./index.js";

const API_PREFIX = "/v1/agent";
const MAX_BODY_BYTES = 64 * 1024;
const DEFAULT_HOST = "127.0.0.1";
const DEFAULT_PORT = 4318;

interface Config {
  readonly host: string;
  readonly port: number;
  readonly bearerToken: Buffer;
  readonly tenantId: string;
  readonly principalId: string;
  readonly databaseUrl: string;
  readonly databaseCaPem: string | null;
  readonly descriptorPath: string;
  readonly descriptorHash: string;
  readonly releaseSetHash: string;
}

interface StoredRun {
  readonly session_id: string;
}

function required(name: string): string {
  const value = process.env[name];
  if (!value || value.includes("\0")) throw new Error(`missing_or_invalid_${name.toLowerCase()}`);
  return value;
}

function identifier(value: string, name: string): string {
  if (!/^[A-Za-z0-9_-]{1,128}$/.test(value)) throw new Error(`invalid_${name}`);
  return value;
}

function hash(value: string, name: string): string {
  if (!/^sha256:[0-9a-f]{64}$/.test(value)) throw new Error(`invalid_${name}`);
  return value;
}

function port(value: string | undefined): number {
  if (value === undefined) return DEFAULT_PORT;
  const parsed = Number(value);
  if (!Number.isInteger(parsed) || parsed < 1 || parsed > 65_535) throw new Error("invalid_gateway_port");
  return parsed;
}

async function config(): Promise<Config> {
  const host = process.env.KRW_AGENT_GATEWAY_HOST ?? DEFAULT_HOST;
  if (host !== "127.0.0.1" && host !== "::1") throw new Error("gateway_must_bind_loopback");
  const descriptorPath = required("KRW_AGENT_RELEASE_DESCRIPTOR_PATH");
  if (!isAbsolute(descriptorPath) || normalize(descriptorPath) !== descriptorPath) {
    throw new Error("invalid_release_descriptor_path");
  }
  const token = Buffer.from(required("KRW_AGENT_GATEWAY_TOKEN"), "utf8");
  if (token.length < 24 || token.length > 16 * 1024) throw new Error("invalid_gateway_token");
  const caFile = process.env.KRW_AGENT_GATEWAY_DATABASE_CA_PEM_FILE;
  const inlineCa = process.env.KRW_AGENT_GATEWAY_DATABASE_CA_PEM;
  if (caFile && inlineCa) throw new Error("gateway_database_ca_source_ambiguous");
  const databaseCaPem = caFile ? await readFile(caFile, "utf8") : inlineCa ?? null;
  if (databaseCaPem !== null && databaseCaPem.length === 0) throw new Error("invalid_gateway_database_ca");
  return {
    host,
    port: port(process.env.KRW_AGENT_GATEWAY_PORT),
    bearerToken: token,
    tenantId: identifier(required("KRW_AGENT_GATEWAY_TENANT_ID"), "gateway_tenant_id"),
    principalId: identifier(required("KRW_AGENT_GATEWAY_PRINCIPAL_ID"), "gateway_principal_id"),
    databaseUrl: required("KRW_AGENT_GATEWAY_DATABASE_URL"),
    databaseCaPem,
    descriptorPath,
    descriptorHash: hash(required("KRW_AGENT_RELEASE_DESCRIPTOR_HASH"), "release_descriptor_hash"),
    releaseSetHash: hash(required("KRW_AGENT_RELEASE_SET_HASH"), "release_set_hash"),
  };
}

function pgClient(client: Pool | PoolClient): PgQueryClient {
  return {
    async query<Row>(query: { readonly name: string; readonly text: string; readonly values: readonly unknown[] }) {
      const result = await client.query({
        name: query.name,
        text: query.text,
        // `pg` deliberately types parameter values as `any`; this adapter
        // preserves the host package's `unknown` boundary before that driver
        // boundary, while keeping each statement name fixed by the adapter.
        values: [...query.values] as never[],
      });
      return { rowCount: result.rowCount, rows: result.rows as readonly Row[] };
    },
  };
}

async function initializeGatewayStore(pool: Pool): Promise<void> {
  // This schema is owned by the local Gateway, not by the agent runtime. It
  // persists only authenticated session/run ownership; it contains no model,
  // evidence, provider, or tool payload.
  await pool.query("CREATE SCHEMA IF NOT EXISTS krw_gateway_local");
  await pool.query(`
    CREATE TABLE IF NOT EXISTS krw_gateway_local.sessions (
      session_id text PRIMARY KEY CHECK (session_id ~ '^[A-Za-z0-9_-]{1,128}$'),
      tenant_id text NOT NULL CHECK (tenant_id ~ '^[A-Za-z0-9_-]{1,128}$'),
      principal_id text NOT NULL CHECK (principal_id ~ '^[A-Za-z0-9_-]{1,128}$'),
      created_at timestamptz NOT NULL DEFAULT clock_timestamp()
    )
  `);
  await pool.query(`
    CREATE TABLE IF NOT EXISTS krw_gateway_local.runs (
      run_id text PRIMARY KEY CHECK (run_id ~ '^[A-Za-z0-9_-]{1,128}$'),
      session_id text NOT NULL REFERENCES krw_gateway_local.sessions(session_id) ON DELETE RESTRICT,
      tenant_id text NOT NULL CHECK (tenant_id ~ '^[A-Za-z0-9_-]{1,128}$'),
      principal_id text NOT NULL CHECK (principal_id ~ '^[A-Za-z0-9_-]{1,128}$'),
      created_at timestamptz NOT NULL DEFAULT clock_timestamp()
    )
  `);
}

function sameToken(value: string | undefined, expected: Buffer): boolean {
  if (!value?.startsWith("Bearer ")) return false;
  const actual = Buffer.from(value.slice("Bearer ".length), "utf8");
  return actual.length === expected.length && timingSafeEqual(actual, expected);
}

function writeJson(response: ServerResponse, status: number, value: JsonObject): void {
  const bytes = Buffer.from(JSON.stringify(value));
  response.writeHead(status, {
    "content-type": "application/json; charset=utf-8",
    "content-length": String(bytes.length),
    "cache-control": "no-store",
  });
  response.end(bytes);
}

async function readJson(request: IncomingMessage): Promise<unknown> {
  const contentLength = request.headers["content-length"];
  if (contentLength && (!/^\d+$/.test(contentLength) || Number(contentLength) > MAX_BODY_BYTES)) {
    throw new ContractViolation("gateway_body_too_large");
  }
  const chunks: Buffer[] = [];
  let size = 0;
  for await (const chunk of request) {
    const bytes = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk);
    size += bytes.length;
    if (size > MAX_BODY_BYTES) throw new ContractViolation("gateway_body_too_large");
    chunks.push(bytes);
  }
  try {
    return JSON.parse(Buffer.concat(chunks).toString("utf8"));
  } catch {
    throw new ContractViolation("gateway_body_json");
  }
}

function generatedId(prefix: "ses" | "run" | "mut"): string {
  return `${prefix}_${randomUUID().replaceAll("-", "")}`;
}

const noCommittedAnswerMaterializer: MaterializationPorts = {
  committedAnswers: {
    async materializeCommittedAnswerSource() {
      throw new ContractViolation("gateway_company_research_has_no_committed_source");
    },
  },
};

async function createRun(
  pool: Pool,
  artifact: Awaited<ReturnType<typeof loadPinnedReleaseArtifact>>,
  config: Config,
  sessionId: string | null,
  body: unknown,
): Promise<JsonObject> {
  const client = await pool.connect();
  try {
    await client.query("BEGIN");
    const resolvedSessionId = sessionId ?? generatedId("ses");
    if (sessionId === null) {
      await client.query({
        name: "krw_gateway_create_session_v1",
        text: "INSERT INTO krw_gateway_local.sessions(session_id, tenant_id, principal_id) VALUES ($1, $2, $3)",
        values: [resolvedSessionId, config.tenantId, config.principalId],
      });
    } else {
      const owned = await client.query<{ readonly session_id: string }>({
        name: "krw_gateway_read_session_owner_v1",
        text: "SELECT session_id FROM krw_gateway_local.sessions WHERE session_id=$1 AND tenant_id=$2 AND principal_id=$3",
        values: [resolvedSessionId, config.tenantId, config.principalId],
      });
      if (owned.rowCount !== 1) throw new ContractViolation("gateway_session_not_owned");
    }

    const runId = generatedId("run");
    const ownership = {
      schema_version: 1 as const,
      tenant_id: config.tenantId,
      principal_id: config.principalId,
      session_id: resolvedSessionId,
      run_id: runId,
    };
    const prepared = await prepareGatewayCompanyResearch({
      artifact,
      ownership,
      mutation_id: generatedId("mut"),
      request: parseGatewayCompanyResearchRequest(body),
      materializers: noCommittedAnswerMaterializer,
    });
    await client.query({
      name: "krw_gateway_create_run_v1",
      text: "INSERT INTO krw_gateway_local.runs(run_id, session_id, tenant_id, principal_id) VALUES ($1, $2, $3, $4)",
      values: [runId, resolvedSessionId, config.tenantId, config.principalId],
    });
    // The fixed agent ABI call occurs before committing local ownership so a
    // visible Gateway run can never exist without a matching immutable claim.
    const transactionAgent = new HostAgentClient(new HostPostgresTransport(pgClient(client)));
    const enqueued = await transactionAgent.enqueue(prepared);
    await client.query("COMMIT");
    return { schema_version: 1, session_id: resolvedSessionId, run_id: enqueued.run_id, state: "queued" };
  } catch (error) {
    await client.query("ROLLBACK").catch(() => undefined);
    throw error;
  } finally {
    client.release();
  }
}

async function readRun(pool: Pool, agent: HostAgentClient, config: Config, runId: string): Promise<JsonObject> {
  const owned = await pool.query<StoredRun>({
    name: "krw_gateway_read_run_owner_v1",
    text: "SELECT session_id FROM krw_gateway_local.runs WHERE run_id=$1 AND tenant_id=$2 AND principal_id=$3",
    values: [runId, config.tenantId, config.principalId],
  });
  if (owned.rowCount !== 1 || !owned.rows[0]) throw new ContractViolation("gateway_run_not_owned");
  const ownership = {
    schema_version: 1 as const,
    tenant_id: config.tenantId,
    principal_id: config.principalId,
    session_id: owned.rows[0].session_id,
    run_id: runId,
  };
  const outcome = await agent.readCommittedOutcome(ownership);
  if (outcome.state !== "final") {
    return { schema_version: 1, session_id: ownership.session_id, run_id: runId, state: outcome.state, final_output: null };
  }
  const finalOutput = await agent.readFinalOutput(ownership);
  return {
    schema_version: 1,
    session_id: ownership.session_id,
    run_id: runId,
    state: "final",
    final_output: { markdown: finalOutput.markdown, final_output_hash: finalOutput.final_output_hash },
  };
}

async function main(): Promise<void> {
  const settings = await config();
  // Read the descriptor exactly once before accepting traffic. The loaded
  // artifact is immutable for this process lifetime.
  const artifact = await loadPinnedReleaseArtifact({
    path: settings.descriptorPath,
    expectedArtifactHash: settings.descriptorHash as `sha256:${string}`,
    expectedReleaseSetHash: settings.releaseSetHash as `sha256:${string}`,
  });
  const pool = new Pool({
    connectionString: settings.databaseUrl,
    ssl: settings.databaseCaPem === null ? undefined : { ca: settings.databaseCaPem, rejectUnauthorized: true },
    max: 8,
    idleTimeoutMillis: 30_000,
    connectionTimeoutMillis: 5_000,
  });
  await initializeGatewayStore(pool);
  const agent = new HostAgentClient(new HostPostgresTransport(pgClient(pool)));
  const server = createServer(async (request, response) => {
    try {
      const path = new URL(request.url ?? "/", `http://${settings.host}`).pathname;
      if (request.method === "GET" && path === "/healthz") {
        writeJson(response, 200, { schema_version: 1, status: "ok" });
        return;
      }
      if (!sameToken(request.headers.authorization, settings.bearerToken)) {
        writeJson(response, 401, { schema_version: 1, error: "unauthorized" });
        return;
      }
      if (request.method === "POST" && path === `${API_PREFIX}/runs`) {
        writeJson(response, 202, await createRun(pool, artifact, settings, null, await readJson(request)));
        return;
      }
      const continuation = new RegExp(`^${API_PREFIX}/sessions/([A-Za-z0-9_-]{1,128})/runs$`).exec(path);
      if (request.method === "POST" && continuation) {
        writeJson(response, 202, await createRun(pool, artifact, settings, continuation[1] ?? null, await readJson(request)));
        return;
      }
      const status = new RegExp(`^${API_PREFIX}/runs/([A-Za-z0-9_-]{1,128})$`).exec(path);
      if (request.method === "GET" && status) {
        writeJson(response, 200, await readRun(pool, agent, settings, status[1] ?? ""));
        return;
      }
      writeJson(response, 404, { schema_version: 1, error: "not_found" });
    } catch (error) {
      const status = error instanceof ContractViolation ? 400 : 500;
      // Never send provider/database/details to a CLI client or log them here.
      writeJson(response, status, { schema_version: 1, error: status === 400 ? "invalid_request" : "gateway_failure" });
    }
  });
  server.listen(settings.port, settings.host, () => {
    process.stdout.write(`KRW Agent Gateway listening on http://${settings.host}:${settings.port}${API_PREFIX}\n`);
  });
  const shutdown = () => server.close(() => pool.end().finally(() => process.exit(0)));
  process.once("SIGINT", shutdown);
  process.once("SIGTERM", shutdown);
}

void main().catch((error: unknown) => {
  const code = error instanceof Error && /^[a-z0-9_]{1,128}$/.test(error.message)
    ? error.message
    : "startup_failed";
  process.stderr.write(`local_gateway_startup_failed=${code}\n`);
  process.exitCode = 1;
});
