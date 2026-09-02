/**
 * OpenBB Workspace copilot agent bridge.
 *
 * Exposes the two-endpoint custom-agent contract (GET /agents.json, POST /query
 * with named SSE responses) on top of the local KRW Agent gateway
 * (POST /runs + GET /runs/:id polling). See
 * docs/superpowers/plans/2026-08-31-workspace-copilot-bridge.md for the locked
 * design: Korean deep research for every question, real widget data via the
 * remote function-call loop, local Mac + cloudflared tunnel hosting.
 */
import { createServer, type IncomingMessage, type ServerResponse } from "node:http";
import { randomUUID } from "node:crypto";

import {
  buildSkillDirective,
  buildWidgetDigest,
  composeQuestion,
  encodeSse,
  extractQuestion,
  gatewayRunBody,
  hasWidgetDataResult,
  messageChunk,
  parseFollowUps,
  parseQueryRequest,
  pickTicker,
  primaryWidgetDataSources,
  promptSuggestions,
  statusUpdate,
  visualizationArtifacts,
  widgetCitation,
  widgetDataFunctionCall,
  widgetInputArgs,
  type SseEvent,
} from "./contract.js";

interface BridgeConfig {
  readonly host: string;
  readonly port: number;
  readonly pathSecret: string;
  readonly gatewayUrl: string;
  readonly gatewayToken: string;
  readonly pollIntervalMs: number;
  readonly heartbeatMs: number;
  readonly maxRunMs: number;
  readonly rateLimitWindowMs: number;
  readonly rateLimitMax: number;
}

function requiredEnv(name: string): string {
  const value = process.env[name];
  if (!value || value.length === 0 || value.includes("\0")) {
    throw new Error(`missing_or_invalid_${name.toLowerCase()}`);
  }
  return value;
}

function optionalIntEnv(name: string, fallback: number): number {
  const raw = process.env[name];
  if (raw === undefined || raw.length === 0) return fallback;
  const parsed = Number(raw);
  if (!Number.isInteger(parsed) || parsed <= 0) throw new Error(`invalid_${name.toLowerCase()}`);
  return parsed;
}

function config(): BridgeConfig {
  const plain = process.env.KRW_WORKSPACE_BRIDGE_ALLOW_PLAIN === "1";
  const pathSecret = process.env.KRW_WORKSPACE_BRIDGE_PATH_SECRET ?? (plain ? "" : requiredEnv("KRW_WORKSPACE_BRIDGE_PATH_SECRET"));
  if (pathSecret.length > 0 && !/^[A-Za-z0-9_-]{16,128}$/.test(pathSecret)) {
    throw new Error("invalid_krw_workspace_bridge_path_secret");
  }
  if (pathSecret.length === 0 && !plain) throw new Error("invalid_krw_workspace_bridge_path_secret");
  return {
    host: process.env.KRW_WORKSPACE_BRIDGE_HOST ?? "127.0.0.1",
    port: optionalIntEnv("KRW_WORKSPACE_BRIDGE_PORT", 14790),
    pathSecret,
    gatewayUrl: process.env.KRW_AGENT_GATEWAY_URL ?? "http://127.0.0.1:14318/v1/agent",
    gatewayToken: requiredEnv("KRW_AGENT_GATEWAY_TOKEN"),
    pollIntervalMs: optionalIntEnv("KRW_WORKSPACE_BRIDGE_POLL_MS", 4_000),
    heartbeatMs: optionalIntEnv("KRW_WORKSPACE_BRIDGE_HEARTBEAT_MS", 90_000),
    maxRunMs: optionalIntEnv("KRW_WORKSPACE_BRIDGE_MAX_RUN_MS", 45 * 60_000),
    rateLimitWindowMs: 60_000,
    rateLimitMax: optionalIntEnv("KRW_WORKSPACE_BRIDGE_RATE_LIMIT_MAX", 120),
  };
}

class RateLimiter {
  private readonly hits = new Map<string, number>();

  check(key: string): boolean {
    const bucket = Math.floor(Date.now() / 60_000);
    const mapKey = `${key}:${bucket}`;
    const count = (this.hits.get(mapKey) ?? 0) + 1;
    this.hits.set(mapKey, count);
    if (this.hits.size > 10_000) {
      for (const existing of this.hits.keys()) {
        if (Number(existing.slice(existing.lastIndexOf(":") + 1)) < bucket) this.hits.delete(existing);
      }
    }
    return count <= this.max;
  }

  constructor(private readonly max: number) {}
}

const ALLOWED_CORS_HOST_SUFFIXES = [".openbb.co", ".openbb.com", "openbb.co", "openbb.com"];

function corsOriginAllowed(origin: string): boolean {
  let host: string;
  try {
    host = new URL(origin).hostname.toLowerCase();
  } catch {
    return false;
  }
  if (host === "localhost" || host === "127.0.0.1" || host === "[::1]") return true;
  if (ALLOWED_CORS_HOST_SUFFIXES.some((suffix) => host === suffix || host.endsWith(suffix))) return true;
  const extras = (process.env.KRW_WORKSPACE_BRIDGE_EXTRA_ORIGINS ?? "")
    .split(",")
    .map((value) => value.trim().toLowerCase())
    .filter((value) => value.length > 0);
  return extras.includes(host);
}

// The browser-side Workspace client is cross-origin by design (hosted OpenBB
// domains or a self-hosted deployment), so the manifest/query routes echo only
// allow-listed origins. Non-browser callers (curl, the tunnel health probes)
// carry no Origin and pass without CORS headers.
function applyCors(request: IncomingMessage, response: ServerResponse): void {
  const origin = request.headers.origin;
  if (typeof origin === "string" && corsOriginAllowed(origin)) {
    response.setHeader("Access-Control-Allow-Origin", origin);
    response.setHeader("Access-Control-Allow-Methods", "GET, POST, OPTIONS");
    response.setHeader("Access-Control-Allow-Headers", "Content-Type");
    response.setHeader("Vary", "Origin");
  }
}

interface GatewayRun {
  readonly run_id: string;
}

interface GatewayStatus {
  readonly state: string;
  readonly final_output: { readonly markdown: string; readonly visualizations?: readonly unknown[] } | null;
  readonly retry_message: string | null;
}

async function gatewayFetch(
  settings: BridgeConfig,
  path: string,
  init?: RequestInit,
): Promise<unknown> {
  const response = await fetch(`${settings.gatewayUrl}${path}`, {
    ...init,
    headers: {
      authorization: `Bearer ${settings.gatewayToken}`,
      "content-type": "application/json",
      ...(init?.headers ?? {}),
    },
  });
  const body: unknown = await response.json().catch(() => null);
  if (!response.ok) {
    const code = body !== null && typeof body === "object" && "error" in body
      ? String((body as { error: unknown }).error)
      : `http_${response.status}`;
    throw new Error(`gateway_request_failed:${code}`);
  }
  return body;
}

function sleep(ms: number, aborted: () => boolean): Promise<void> {
  return new Promise((resolve) => {
    const timer = setTimeout(resolve, ms);
    if (timer.unref === undefined) return;
    void timer;
    const check = () => {
      if (aborted()) {
        clearTimeout(timer);
        resolve();
      }
    };
    const interval = setInterval(check, 500);
    if (interval.unref !== undefined) interval.unref();
  });
}

function streamMarkdown(markdown: string): readonly SseEvent[] {
  const events: SseEvent[] = [];
  const lines = markdown.split("\n");
  for (let index = 0; index < lines.length; index += 1) {
    const line = lines[index];
    if (line === undefined) break;
    events.push(messageChunk(index === lines.length - 1 ? line : `${line}\n`));
  }
  return events;
}

async function handleQuery(
  settings: BridgeConfig,
  request: IncomingMessage,
  response: ServerResponse,
): Promise<void> {
  response.writeHead(200, {
    "content-type": "text/event-stream; charset=utf-8",
    "cache-control": "no-store",
    connection: "keep-alive",
    "x-accel-buffering": "no",
  });
  const aborted = () => response.writableEnded || response.destroyed;
  const write = (event: SseEvent) => {
    if (!aborted()) response.write(encodeSse(event));
  };

  let body = "";
  request.setEncoding("utf8");
  for await (const chunk of request) {
    body += chunk;
    if (body.length > 4 * 1024 * 1024) {
      write(statusUpdate("요청 본문이 너무 큽니다.", "ERROR"));
      response.end();
      return;
    }
  }
  let parsed: ReturnType<typeof parseQueryRequest>;
  try {
    parsed = parseQueryRequest(JSON.parse(body));
  } catch {
    write(statusUpdate("요청 형식을 읽을 수 없습니다.", "ERROR"));
    response.end();
    return;
  }

  const question = extractQuestion(parsed.messages);
  if (question === null) {
    write(statusUpdate("질문 텍스트를 찾을 수 없습니다.", "WARNING"));
    response.end();
    return;
  }

  // Phase 1: pull the clicked (primary) widgets' actual data with the remote
  // function-call loop. The connection closes here; the Workspace re-POSTs
  // with the tool result appended.
  const dataSources = primaryWidgetDataSources(parsed);
  if (dataSources.length > 0 && !hasWidgetDataResult(parsed.messages)) {
    write(statusUpdate("클릭한 위젯의 데이터를 가져오는 중…"));
    write(widgetDataFunctionCall(dataSources));
    response.end();
    return;
  }

  const ticker = pickTicker(parsed, question);
  if (ticker === null) {
    write(statusUpdate("종목(ticker)을 특정할 수 없습니다. 위젯을 추가하거나 티커를 질문에 포함해 주세요.", "WARNING"));
    write(messageChunk("종목을 특정할 수 없어 조사를 시작할 수 없습니다. 차트·표 위젯을 'Add to context'로 붙이거나, 질문에 티커(예: MSFT)를 함께 적어 주세요."));
    response.end();
    return;
  }

  const digest = buildWidgetDigest(parsed, dataSources);
  // "/"-pinned skill first frames the research, then the on-screen widget
  // observation grounds it; the guru lens stays disabled (never sent).
  const skillDirective = buildSkillDirective(parsed);
  const composedQuestion = composeQuestion(question, skillDirective, digest.block);

  let run: GatewayRun;
  try {
    run = (await gatewayFetch(settings, "/runs", {
      method: "POST",
      body: JSON.stringify(gatewayRunBody(ticker.ticker, composedQuestion)),
    })) as GatewayRun;
  } catch (error) {
    write(statusUpdate(`런 등록 실패: ${error instanceof Error ? error.message : "알 수 없음"}`, "ERROR"));
    write(messageChunk("조사 런을 등록하지 못했습니다. 잠시 후 다시 시도해 주세요."));
    response.end();
    return;
  }

  write(statusUpdate(`${ticker.ticker} 심층 리서치를 시작했습니다 (출처: ${ticker.source}). 수 분이 걸릴 수 있습니다.`));

  const startedAt = Date.now();
  let lastHeartbeat = 0;
  let lastState = "queued";
  for (;;) {
    if (aborted()) return;
    if (Date.now() - startedAt > settings.maxRunMs) {
      write(statusUpdate(`제한 시간(${Math.round(settings.maxRunMs / 60000)}분)을 초과했습니다.`, "WARNING"));
      write(messageChunk("조사가 제한 시간 안에 마무리되지 않았습니다. 질문을 조금 더 좁혀서 다시 시도해 주세요."));
      response.end();
      return;
    }
    let status: GatewayStatus;
    try {
      status = (await gatewayFetch(settings, `/runs/${run.run_id}`)) as GatewayStatus;
    } catch (error) {
      write(statusUpdate(`런 상태 조회 실패: ${error instanceof Error ? error.message : "알 수 없음"}`, "ERROR"));
      write(messageChunk("조사 상태를 확인하지 못했습니다. 잠시 후 다시 시도해 주세요."));
      response.end();
      return;
    }
    if (status.state !== lastState) {
      lastState = status.state;
      if (status.state === "active") write(statusUpdate("온톨로지·시장 데이터 심층 조사 진행 중…"));
    }
    if (Date.now() - lastHeartbeat >= settings.heartbeatMs) {
      lastHeartbeat = Date.now();
      const minutes = Math.floor((Date.now() - startedAt) / 60000);
      write(statusUpdate(`심층 조사 진행 중… (${minutes}분 경과)`));
    }
    if (status.state === "failed") {
      write(statusUpdate("조사 런이 실패했습니다.", "ERROR"));
      write(messageChunk(status.retry_message ?? "조사가 실패했습니다. 잠시 후 다시 시도해 주세요."));
      response.end();
      return;
    }
    if (status.state === "cancelled") {
      write(statusUpdate("조사 런이 취소되었습니다.", "WARNING"));
      response.end();
      return;
    }
    if (status.state === "final" && status.final_output !== null) {
      for (const event of streamMarkdown(status.final_output.markdown)) write(event);
      const visualizations = status.final_output.visualizations ?? [];
      for (const event of visualizationArtifacts(visualizations)) write(event);
      for (const widget of digest.citedWidgets) {
        write(widgetCitation({ origin: widget.origin, widgetId: widget.widget_id, inputArgs: widgetInputArgs(widget) }));
      }
      const followUps = parseFollowUps(status.final_output.markdown);
      if (followUps.length > 0) write(promptSuggestions(followUps));
      response.end();
      return;
    }
    await sleep(settings.pollIntervalMs, aborted);
  }
}

function agentsJsonBody(settings: BridgeConfig, request: IncomingMessage): Record<string, unknown> {
  // Behind the cloudflared tunnel the Host header is the public hostname, so
  // the manifest advertises the same public URL the Workspace already reached.
  const host = request.headers.host ?? `127.0.0.1:${settings.port}`;
  const forwardedProto = request.headers["x-forwarded-proto"];
  const proto = typeof forwardedProto === "string" ? forwardedProto : "http";
  const base = `${proto}://${host}${settings.pathSecret.length > 0 ? `/${settings.pathSecret}` : ""}`;
  return {
    schema_version: 1,
    krw_research: {
      name: "KRW 심층 리서치",
      description: "클릭한 위젯의 종목·데이터를 배경으로 공시 온톨로지·거시·시세를 삼각 조사해 국문 심층 보고서를 제공합니다. 수 분이 걸립니다.",
      endpoints: { query: `${base}/query` },
      features: {
        streaming: true,
        "widget-dashboard-select": true,
        "widget-dashboard-search": true,
      },
    },
  };
}

function main(): void {
  const settings = config();
  const limiter = new RateLimiter(settings.rateLimitMax);
  const prefix = settings.pathSecret.length > 0 ? `/${settings.pathSecret}` : "";
  const server = createServer((request, response) => {
    const path = new URL(request.url ?? "/", `http://${settings.host}`).pathname;
    const agentsJsonPath = `${prefix}/agents.json`;
    const queryPath = `${prefix}/query`;
    const onRoute = path === agentsJsonPath || path === queryPath;
    if (onRoute) applyCors(request, response);
    if (request.method === "OPTIONS" && onRoute) {
      response.writeHead(204, { "content-length": "0" });
      response.end();
      return;
    }
    if (!onRoute) {
      response.writeHead(404, { "content-type": "application/json; charset=utf-8" });
      response.end(Buffer.from(JSON.stringify({ schema_version: 1, error: "not_found" })));
      return;
    }
    const ip = request.socket.remoteAddress ?? "unknown";
    if (!limiter.check(ip)) {
      response.writeHead(429, { "content-type": "application/json; charset=utf-8" });
      response.end(Buffer.from(JSON.stringify({ schema_version: 1, error: "rate_limited" })));
      return;
    }
    if (request.method === "GET" && path === agentsJsonPath) {
      response.writeHead(200, { "content-type": "application/json; charset=utf-8" });
      response.end(Buffer.from(JSON.stringify(agentsJsonBody(settings, request))));
      return;
    }
    if (request.method === "POST" && path === queryPath) {
      void handleQuery(settings, request, response).catch(() => {
        if (!response.writableEnded) response.end();
      });
      return;
    }
    response.writeHead(405, { "content-type": "application/json; charset=utf-8" });
    response.end(Buffer.from(JSON.stringify({ schema_version: 1, error: "method_not_allowed" })));
  });
  server.listen(settings.port, settings.host, () => {
    process.stdout.write(
      `KRW Workspace Bridge listening on http://${settings.host}:${settings.port}${prefix} (gateway ${settings.gatewayUrl})\n`,
    );
  });
  const shutdown = () => server.close(() => process.exit(0));
  process.once("SIGINT", shutdown);
  process.once("SIGTERM", shutdown);
}

void main();
