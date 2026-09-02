/**
 * OpenBB Workspace copilot wire contract, mirrored from
 * OpenBB-finance/openbb-ai (MIT) `openbb_ai/models.py`.
 *
 * The SDK serialises every SSE as `BaseSSE.model_dump()`:
 *   {"event": <name>, "data": <compact JSON string>}
 * which sse_starlette writes as a named SSE event. These builders produce the
 * same wire bytes from Node without the Python SDK.
 */

export interface SseEvent {
  readonly event: string;
  readonly data: string;
}

function sse(event: string, payload: unknown): SseEvent {
  return { event, data: JSON.stringify(payload) };
}

export function messageChunk(delta: string): SseEvent {
  return sse("copilotMessageChunk", { delta });
}

export function statusUpdate(
  message: string,
  eventType: "INFO" | "WARNING" | "ERROR" = "INFO",
): SseEvent {
  return sse("copilotStatusUpdate", { eventType, message, group: "reasoning" });
}

export function promptSuggestions(suggestions: readonly string[]): SseEvent {
  return sse("copilotPromptSuggestions", { suggestions: suggestions.slice(0, 3) });
}

export interface WidgetDataSource {
  readonly widget_uuid: string;
  readonly origin: string;
  readonly id: string;
  readonly input_args: Record<string, unknown>;
}

export function widgetDataFunctionCall(dataSources: readonly WidgetDataSource[]): SseEvent {
  return sse("copilotFunctionCall", {
    function: "get_widget_data",
    input_arguments: { data_sources: dataSources },
  });
}

export function chartArtifact(input: {
  readonly chartType: "line" | "bar";
  readonly data: readonly Record<string, unknown>[];
  readonly xKey: string;
  readonly yKeys: readonly string[];
  readonly name: string;
  readonly description: string;
}): SseEvent {
  return sse("copilotMessageArtifact", {
    type: "chart",
    name: input.name,
    description: input.description,
    content: input.data,
    chart_params: {
      chartType: input.chartType,
      xKey: input.xKey,
      yKey: input.yKeys,
    },
  });
}

export function tableArtifact(input: {
  readonly data: readonly Record<string, unknown>[];
  readonly name: string;
  readonly description: string;
}): SseEvent {
  return sse("copilotMessageArtifact", {
    type: "table",
    name: input.name,
    description: input.description,
    content: input.data,
  });
}

export function widgetCitation(input: {
  readonly origin: string;
  readonly widgetId: string;
  readonly inputArgs: Record<string, unknown>;
}): SseEvent {
  return sse("copilotCitationCollection", {
    citations: [
      {
        source_info: {
          type: "widget",
          origin: input.origin,
          widget_id: input.widgetId,
          metadata: { input_args: input.inputArgs },
        },
      },
    ],
  });
}

/**
 * Minimal structural readers over the Workspace QueryRequest. The bridge only
 * consumes the subset the krw run needs; unknown fields are ignored on purpose
 * (the contract adds fields without notice).
 */

export interface WidgetParam {
  readonly name: string;
  readonly type: string;
  readonly current_value?: unknown;
  readonly executed_value?: unknown;
  readonly default_value?: unknown;
}

export interface Widget {
  readonly uuid: string;
  readonly origin: string;
  readonly widget_id: string;
  readonly name: string;
  readonly params: readonly WidgetParam[];
}

export interface WidgetCollection {
  readonly primary: readonly Widget[];
  readonly secondary: readonly Widget[];
}

export interface QueryRequest {
  readonly messages: readonly unknown[];
  readonly widgets: WidgetCollection | null;
  readonly selectedSkills: readonly SelectedSkill[];
}

/** A skill the user pinned with the copilot's "/" picker (forced_slash). */
export interface SelectedSkill {
  readonly slug: string;
  readonly description: string;
  readonly contentMarkdown: string;
  readonly source: string;
}

function readSelectedSkills(value: unknown): readonly SelectedSkill[] {
  if (!Array.isArray(value)) return [];
  const skills: SelectedSkill[] = [];
  for (const item of value) {
    if (typeof item !== "object" || item === null) continue;
    const object = item as Record<string, unknown>;
    const content = object.contentMarkdown ?? object.content_markdown;
    if (typeof object.slug !== "string" || typeof content !== "string") continue;
    skills.push({
      slug: object.slug,
      description: typeof object.description === "string" ? object.description : "",
      contentMarkdown: content,
      source: typeof object.source === "string" ? object.source : "forced_slash",
    });
  }
  return skills;
}

export function parseQueryRequest(value: unknown): QueryRequest {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new Error("query_request_object");
  }
  const messages = (value as { messages?: unknown }).messages;
  if (!Array.isArray(messages) || messages.length === 0) {
    throw new Error("query_request_messages");
  }
  const rawWidgets = (value as { widgets?: unknown }).widgets;
  let widgets: WidgetCollection | null = null;
  if (typeof rawWidgets === "object" && rawWidgets !== null) {
    widgets = {
      primary: readWidgetList((rawWidgets as { primary?: unknown }).primary),
      secondary: readWidgetList((rawWidgets as { secondary?: unknown }).secondary),
    };
  }
  return {
    messages,
    widgets,
    selectedSkills: readSelectedSkills((value as { selected_skills?: unknown }).selected_skills),
  };
}

function readWidgetList(value: unknown): readonly Widget[] {
  if (!Array.isArray(value)) return [];
  const widgets: Widget[] = [];
  for (const item of value) {
    if (typeof item !== "object" || item === null) continue;
    const object = item as Record<string, unknown>;
    if (
      typeof object.uuid !== "string"
      || typeof object.origin !== "string"
      || typeof object.widget_id !== "string"
      || typeof object.name !== "string"
    ) {
      continue;
    }
    const params: WidgetParam[] = [];
    if (Array.isArray(object.params)) {
      for (const rawParam of object.params) {
        if (typeof rawParam !== "object" || rawParam === null) continue;
        const param = rawParam as Record<string, unknown>;
        if (typeof param.name !== "string" || typeof param.type !== "string") continue;
        params.push({
          name: param.name,
          type: param.type,
          ...(param.current_value === undefined ? {} : { current_value: param.current_value }),
          ...(param.executed_value === undefined ? {} : { executed_value: param.executed_value }),
          ...(param.default_value === undefined ? {} : { default_value: param.default_value }),
        });
      }
    }
    widgets.push({ uuid: object.uuid, origin: object.origin, widget_id: object.widget_id, name: object.name, params });
  }
  return widgets;
}

/** The human question is the last human message with plain string content. */
export function extractQuestion(messages: readonly unknown[]): string | null {
  for (let index = messages.length - 1; index >= 0; index -= 1) {
    const message = messages[index];
    if (typeof message !== "object" || message === null) continue;
    const object = message as Record<string, unknown>;
    if (object.role !== "human") continue;
    if (typeof object.content === "string" && object.content.trim().length > 0) {
      return object.content;
    }
  }
  return null;
}

/** True once the Workspace re-POSTed with a get_widget_data tool result. */
export function hasWidgetDataResult(messages: readonly unknown[]): boolean {
  return messages.some((message) => {
    if (typeof message !== "object" || message === null) return false;
    const object = message as Record<string, unknown>;
    return object.role === "tool" && object.function === "get_widget_data";
  });
}

function paramValue(param: WidgetParam): unknown {
  if (param.current_value !== undefined && param.current_value !== null) return param.current_value;
  if (param.executed_value !== undefined && param.executed_value !== null) return param.executed_value;
  if (param.default_value !== undefined && param.default_value !== null) return param.default_value;
  return undefined;
}

export function widgetInputArgs(widget: Widget): Record<string, unknown> {
  const args: Record<string, unknown> = {};
  for (const param of widget.params) {
    if (param.type === "tabs") continue;
    const value = paramValue(param);
    if (value === undefined) continue;
    args[param.name] = value;
  }
  return args;
}

const TICKER_PATTERN = /^[A-Z][A-Z0-9.\-]{0,9}$/;
const QUESTION_TICKER_PATTERN = /\b([A-Z]{2,6})\b/;

export interface TickerChoice {
  readonly ticker: string;
  readonly source: string;
}

export function pickTicker(request: QueryRequest, question: string): TickerChoice | null {
  const groups = request.widgets === null ? [] : [request.widgets.primary, request.widgets.secondary];
  for (const group of groups) {
    for (const widget of group) {
      for (const param of widget.params) {
        if (param.type !== "ticker") continue;
        const value = paramValue(param);
        if (typeof value !== "string") continue;
        const first = value.split(",")[0]?.trim().toUpperCase() ?? "";
        if (TICKER_PATTERN.test(first)) return { ticker: first, source: widget.name };
      }
    }
  }
  const match = question.match(QUESTION_TICKER_PATTERN);
  const candidate = match?.[1];
  if (candidate !== undefined && TICKER_PATTERN.test(candidate)) {
    return { ticker: candidate, source: "질문" };
  }
  return null;
}

const MAX_PRIMARY_WIDGET_FETCH = 4;

export function primaryWidgetDataSources(request: QueryRequest): readonly WidgetDataSource[] {
  if (request.widgets === null) return [];
  return request.widgets.primary.slice(0, MAX_PRIMARY_WIDGET_FETCH).map((widget) => ({
    widget_uuid: widget.uuid,
    origin: widget.origin,
    id: widget.widget_id,
    input_args: widgetInputArgs(widget),
  }));
}

export interface WidgetDigest {
  readonly block: string | null;
  readonly citedWidgets: readonly Widget[];
}

const MAX_DIGEST_CHARS = 4_000;
const MAX_DIGEST_ROWS = 12;
const MAX_DIGEST_COLUMNS = 10;
const MAX_CELL_CHARS = 32;

/**
 * Turn the re-POSTed get_widget_data tool results into a compact observation
 * block appended to the krw question. Rows are abbreviated, not invented: the
 * krw engine still runs its own ontology/openbb research on top of this.
 */
export function buildWidgetDigest(
  request: QueryRequest,
  dataSources: readonly WidgetDataSource[],
): WidgetDigest {
  if (request.widgets === null) return { block: null, citedWidgets: [] };
  void dataSources;
  const widgetsByUuid = new Map<string, Widget>();
  for (const widget of [...request.widgets.primary, ...request.widgets.secondary]) {
    widgetsByUuid.set(widget.uuid, widget);
  }
  const lines: string[] = [];
  const cited: Widget[] = [];
  for (const result of readWidgetDataResults(request.messages)) {
    const widget = widgetsByUuid.get(result.widgetUuid);
    if (widget === undefined || result.error) continue;
    cited.push(widget);
    const paramSummary = Object.entries(result.inputArgs)
      .slice(0, 4)
      .map(([key, value]) => `${key}=${String(value).slice(0, MAX_CELL_CHARS)}`)
      .join(", ");
    lines.push(`${widget.name} (${widget.origin}${paramSummary ? `, ${paramSummary}` : ""})`);
    const table = result.table;
    if (table === null) {
      lines.push(`- (데이터 비어 있음)`);
      continue;
    }
    if (table.columns === null) {
      lines.push(`- 텍스트 관측: ${table.text}`);
      continue;
    }
    lines.push(`- 열: ${table.columns.join(", ")}`);
    for (const row of table.rows) {
      lines.push(`- 행: ${row}`);
    }
  }
  if (lines.length === 0) return { block: null, citedWidgets: [] };
  let block = `[화면 위젯 관측 — OpenBB Workspace 컨텍스트]\n${lines.join("\n")}`;
  if (block.length > MAX_DIGEST_CHARS) {
    block = `${block.slice(0, MAX_DIGEST_CHARS)}\n…(위젯 관측 초과분 생략)`;
  }
  return { block, citedWidgets: cited };
}

interface WidgetDataResult {
  readonly widgetUuid: string;
  readonly inputArgs: Record<string, unknown>;
  readonly error: boolean;
  readonly table: { readonly columns: string[] | null; readonly rows: readonly string[]; readonly text: string } | null;
}

function readWidgetDataResults(messages: readonly unknown[]): readonly WidgetDataResult[] {
  const results: WidgetDataResult[] = [];
  for (const message of messages) {
    if (typeof message !== "object" || message === null) continue;
    const object = message as Record<string, unknown>;
    if (object.role !== "tool" || object.function !== "get_widget_data") continue;
    const inputArguments = (object.input_arguments ?? {}) as { data_sources?: unknown };
    const dataSources = Array.isArray(inputArguments.data_sources) ? inputArguments.data_sources : [];
    const dataItems = Array.isArray(object.data) ? object.data : [];
    dataItems.forEach((dataItem, index) => {
      const source = dataSources[index] as Record<string, unknown> | undefined;
      const widgetUuid = typeof source?.widget_uuid === "string" ? source.widget_uuid : `#${index}`;
      const inputArgs = (source?.input_args ?? {}) as Record<string, unknown>;
      if (typeof dataItem !== "object" || dataItem === null) return;
      const data = dataItem as Record<string, unknown>;
      if (typeof data.error_type === "string") {
        results.push({ widgetUuid, inputArgs, error: true, table: null });
        return;
      }
      results.push({ widgetUuid, inputArgs, error: false, table: digestDataContent(data) });
    });
  }
  return results;
}

function digestDataContent(
  data: Record<string, unknown>,
): { readonly columns: string[] | null; readonly rows: readonly string[]; readonly text: string } | null {
  const items = Array.isArray(data.items) ? data.items : [];
  const contents: string[] = [];
  for (const item of items) {
    if (typeof item !== "object" || item === null) continue;
    const content = (item as Record<string, unknown>).content;
    if (typeof content === "string" && content.length > 0) contents.push(content);
  }
  if (contents.length === 0) return null;
  const parsed = tryParseRows(contents[0] ?? "");
  if (parsed === null) {
    const text = (contents[0] ?? "").replace(/\s+/g, " ").trim().slice(0, 240);
    return { columns: null, rows: [], text: text.length > 0 ? text : "(내용 없음)" };
  }
  const columns = Object.keys(parsed[0] ?? {}).slice(0, MAX_DIGEST_COLUMNS);
  const rows = parsed.slice(0, MAX_DIGEST_ROWS).map((row) => {
    return columns
      .map((column) => {
        const value = row[column];
        const cell = value === null || value === undefined ? "" : String(value);
        return `${column}=${cell.length > MAX_CELL_CHARS ? `${cell.slice(0, MAX_CELL_CHARS)}…` : cell}`;
      })
      .join(" | ");
  });
  if (parsed.length > MAX_DIGEST_ROWS) rows.push(`…(총 ${parsed.length}행 중 ${MAX_DIGEST_ROWS}행 표시)`);
  return { columns, rows, text: "" };
}

function tryParseRows(content: string): readonly Record<string, unknown>[] | null {
  const trimmed = content.trim();
  if (!(trimmed.startsWith("[") || trimmed.startsWith("{"))) return null;
  try {
    const parsed: unknown = JSON.parse(trimmed);
    if (Array.isArray(parsed) && parsed.every((row) => typeof row === "object" && row !== null && !Array.isArray(row))) {
      return parsed as Record<string, unknown>[];
    }
    return null;
  } catch {
    return null;
  }
}

export function composeQuestion(
  question: string,
  skillDirective: string | null,
  digest: string | null,
): string {
  return [question, skillDirective, digest].filter((block) => block !== null).join("\n\n");
}

const MAX_SKILL_DIRECTIVE_CHARS = 4_000;

/**
 * The "/" picker sends the pinned skill's full markdown in
 * `selected_skills`. Only the forced selection is honoured — a bare
 * `skills_catalog` (model-selected handshakes) is deliberately ignored so
 * the run always starts from an explicit user intent.
 */
export function buildSkillDirective(request: QueryRequest): string | null {
  const skill = request.selectedSkills[0];
  if (skill === undefined || skill.contentMarkdown.trim().length === 0) return null;
  const header = `[사용자 지정 스킬 지시 — OpenBB Workspace /스킬] (${skill.slug}${
    skill.description.length > 0 ? ` · ${skill.description}` : ""
  })`;
  let block = `${header}\n${skill.contentMarkdown.trim()}`;
  if (block.length > MAX_SKILL_DIRECTIVE_CHARS) {
    block = `${block.slice(0, MAX_SKILL_DIRECTIVE_CHARS)}\n…(스킬 지시 초과분 생략)`;
  }
  return block;
}

/**
 * Gateway run request for the bridge. The guru lens is DISABLED on this
 * agent by operator decision (2026-09-02): the gateway accepts an optional
 * `advisor_lens`, and this builder must never send one — the "/" skill
 * rides the question text instead.
 */
export function gatewayRunBody(ticker: string, question: string): Record<string, unknown> {
  return { schema_version: 1, question, ticker };
}

const FOLLOW_UP_HEADERS = ["### 이어서 볼 질문", "### Suggested Follow-up Questions"];

export function parseFollowUps(markdown: string): readonly string[] {
  const lines = markdown.split("\n");
  let started = false;
  const questions: string[] = [];
  for (const line of lines) {
    if (!started) {
      if (FOLLOW_UP_HEADERS.some((header) => line.trim() === header)) started = true;
      continue;
    }
    const trimmed = line.trim();
    if (trimmed.startsWith("### ") || trimmed.startsWith("## ")) break;
    const question = trimmed.replace(/^\d+\.\s*/, "").replace(/^[-*]\s*/, "").trim();
    if (question.length > 0) questions.push(question);
  }
  return questions.slice(0, 3);
}

const MAX_CHART_POINTS = 60;
const MAX_CHART_SERIES = 4;

interface VizPoint {
  readonly period: string;
  readonly value: number;
  readonly sortKey: number;
}

/**
 * krw-presentation artifacts carry `views[].chart_type` (line|bar) with
 * `series[].points[]{period, value, period_sort_key}`. Each view becomes one
 * Workspace chart artifact; series are merged onto shared period rows.
 */
export function visualizationArtifacts(
  visualizations: readonly unknown[],
): readonly SseEvent[] {
  const events: SseEvent[] = [];
  for (const visualization of visualizations) {
    if (typeof visualization !== "object" || visualization === null) continue;
    const viz = visualization as Record<string, unknown>;
    const vizTitle = typeof viz.title === "string" ? viz.title : "krw 차트";
    const views = Array.isArray(viz.views) ? viz.views : [];
    for (const rawView of views.slice(0, 2)) {
      if (typeof rawView !== "object" || rawView === null) continue;
      const view = rawView as Record<string, unknown>;
      const chartType = view.chart_type === "bar" ? "bar" : "line";
      const viewTitle = typeof view.title === "string" && view.title.length > 0 ? view.title : vizTitle;
      const series = Array.isArray(view.series) ? view.series.slice(0, MAX_CHART_SERIES) : [];
      if (series.length === 0) continue;
      const rowsByPeriod = new Map<string, Record<string, unknown>>();
      const yKeys: string[] = [];
      let pointCount = 0;
      for (const rawSeries of series) {
        if (typeof rawSeries !== "object" || rawSeries === null) continue;
        const serie = rawSeries as Record<string, unknown>;
        const label = typeof serie.label === "string" && serie.label.length > 0 ? serie.label : `계열 ${yKeys.length + 1}`;
        yKeys.push(label);
        const points = (Array.isArray(serie.points) ? serie.points : [])
          .map(toVizPoint)
          .filter((point): point is VizPoint => point !== null)
          .sort((a, b) => a.sortKey - b.sortKey)
          .slice(-MAX_CHART_POINTS);
        pointCount = Math.max(pointCount, points.length);
        for (const point of points) {
          const row = rowsByPeriod.get(point.period) ?? { period: point.period };
          row[label] = point.value;
          rowsByPeriod.set(point.period, row);
        }
      }
      if (rowsByPeriod.size === 0 || yKeys.length === 0) continue;
      const data = [...rowsByPeriod.values()].slice(-MAX_CHART_POINTS);
      events.push(
        chartArtifact({
          chartType,
          data,
          xKey: "period",
          yKeys,
          name: viewTitle,
          description: `${vizTitle} · krw 온톨로지 근거 차트 (${pointCount}포인트)`,
        }),
      );
    }
  }
  return events;
}

function toVizPoint(value: unknown): VizPoint | null {
  if (typeof value !== "object" || value === null) return null;
  const object = value as Record<string, unknown>;
  if (typeof object.period !== "string" && typeof object.period !== "number") return null;
  if (typeof object.value !== "number" || !Number.isFinite(object.value)) return null;
  const sortKey = typeof object.period_sort_key === "number" ? object.period_sort_key : Number(object.period);
  return {
    period: String(object.period),
    value: object.value,
    sortKey: Number.isFinite(sortKey) ? sortKey : 0,
  };
}

export function encodeSse(event: SseEvent): string {
  return `event: ${event.event}\ndata: ${event.data}\n\n`;
}
