import assert from "node:assert/strict";
import test from "node:test";

import { readFileSync } from "node:fs";

import {
  buildSkillDirective,
  buildWidgetDigest,
  chartArtifact,
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
  widgetDataFunctionCall,
  type Widget,
} from "../src/contract.js";

const priceWidget: Widget = {
  uuid: "68ab6973-ed1a-45aa-ab20-efd3e016dd48",
  origin: "OpenBB API",
  widget_id: "eod_price",
  name: "Historical Stock Price",
  params: [
    { name: "symbol", type: "ticker", current_value: "MSFT" },
    { name: "start_date", type: "date", current_value: "2026-05-02" },
    { name: "tabs", type: "tabs", current_value: "prices" },
  ],
};

function queryRequest(overrides: Record<string, unknown> = {}): unknown {
  return {
    messages: [{ role: "human", content: "MSFT 최근 실적과 주가 흐름을 정리해줘" }],
    widgets: { primary: [priceWidget], secondary: [], extra: [] },
    ...overrides,
  };
}

test("sse builders mirror the openbb-ai wire format", () => {
  const encoded = encodeSse(messageChunk("안녕"));
  assert.equal(encoded, 'event: copilotMessageChunk\ndata: {"delta":"안녕"}\n\n');
  const status = statusUpdate("진행 중");
  assert.equal(status.event, "copilotStatusUpdate");
  assert.deepEqual(JSON.parse(status.data), { eventType: "INFO", message: "진행 중", group: "reasoning" });
  const call = widgetDataFunctionCall([
    { widget_uuid: "u", origin: "o", id: "w", input_args: { symbol: "MSFT" } },
  ]);
  assert.equal(call.event, "copilotFunctionCall");
  const payload = JSON.parse(call.data) as { function: string; input_arguments: { data_sources: unknown[] } };
  assert.equal(payload.function, "get_widget_data");
  assert.equal(payload.input_arguments.data_sources.length, 1);
});

test("chart artifact uses camelCase chart_params like the SDK models", () => {
  const event = chartArtifact({
    chartType: "line",
    data: [{ period: "FY2024", 매출: 123 }],
    xKey: "period",
    yKeys: ["매출"],
    name: "차트",
    description: "설명",
  });
  const payload = JSON.parse(event.data) as { chart_params: Record<string, unknown> };
  assert.deepEqual(payload.chart_params, { chartType: "line", xKey: "period", yKey: ["매출"] });
});

test("extractQuestion finds the human text and ignores tool noise", () => {
  const messages = [
    { role: "human", content: "MSFT 전망 알려줘" },
    { role: "ai", content: JSON.stringify({ function: "get_widget_data", input_arguments: {} }) },
    { role: "tool", function: "get_widget_data", input_arguments: {}, data: [] },
  ];
  assert.equal(extractQuestion(messages), "MSFT 전망 알려줘");
  assert.equal(extractQuestion([{ role: "ai", content: "hi" }]), null);
});

test("hasWidgetDataResult detects the workspace re-post", () => {
  assert.equal(hasWidgetDataResult([{ role: "human", content: "q" }]), false);
  assert.equal(
    hasWidgetDataResult([{ role: "tool", function: "get_widget_data", data: [{ items: [] }] }]),
    true,
  );
});

test("pickTicker prefers widget ticker params and falls back to the question", () => {
  const request = parseQueryRequest(queryRequest());
  assert.deepEqual(pickTicker(request, "전망 알려줘"), { ticker: "MSFT", source: "Historical Stock Price" });
  const noWidgets = parseQueryRequest({ messages: [{ role: "human", content: "NVDA 어때?" }] });
  assert.deepEqual(pickTicker(noWidgets, "NVDA 어때?"), { ticker: "NVDA", source: "질문" });
  const hopeless = parseQueryRequest({ messages: [{ role: "human", content: "시장 전망은?" }] });
  assert.equal(pickTicker(hopeless, "시장 전망은?"), null);
});

test("pickTicker also matches the official string-param-named-ticker shape", () => {
  // agents-for-openbb test_payloads carry {name: "ticker", type: "string"}.
  const request = parseQueryRequest({
    messages: [{ role: "human", content: "실적 분석해줘" }],
    widgets: {
      primary: [
        {
          uuid: "w-string-ticker",
          origin: "https://pro.openbb.co",
          widget_id: "company_profile",
          name: "Company Profile",
          params: [{ name: "ticker", type: "string", current_value: "aapl" }],
        },
      ],
      secondary: [],
      extra: [],
    },
  });
  assert.deepEqual(pickTicker(request, "실적 분석해줘"), { ticker: "AAPL", source: "Company Profile" });
});

test("primaryWidgetDataSources uses current values and skips tab params", () => {
  const sources = primaryWidgetDataSources(parseQueryRequest(queryRequest()));
  assert.equal(sources.length, 1);
  assert.deepEqual(sources[0]?.input_args, { symbol: "MSFT", start_date: "2026-05-02" });
});

test("buildWidgetDigest summarizes widget rows with caps and a citation list", () => {
  const rows = Array.from({ length: 20 }, (_, index) => ({
    date: `2026-08-${String(index + 1).padStart(2, "0")}`,
    close: 400 + index,
    volume: 1_000_000 + index,
  }));
  const request = parseQueryRequest(
    queryRequest({
      messages: [
        { role: "human", content: "MSFT 흐름?" },
        { role: "ai", content: JSON.stringify({ function: "get_widget_data", input_arguments: {} }) },
        {
          role: "tool",
          function: "get_widget_data",
          input_arguments: { data_sources: [{ widget_uuid: priceWidget.uuid, origin: priceWidget.origin, id: priceWidget.widget_id, input_args: { symbol: "MSFT" } }] },
          data: [{ items: [{ content: JSON.stringify(rows), data_format: { data_type: "object" } }] }],
        },
      ],
    }),
  );
  const digest = buildWidgetDigest(request, []);
  const block = digest.block ?? "";
  assert.ok(block.includes("[화면 위젯 관측 — OpenBB Workspace 컨텍스트]"));
  assert.ok(block.includes("Historical Stock Price"));
  assert.ok(block.includes("열: date, close, volume"));
  assert.ok(block.includes("총 20행 중 12행 표시"));
  assert.ok(block.length <= 4_100);
  assert.ok(block.length > 0);
  assert.equal(digest.citedWidgets.length, 1);
});

test("buildWidgetDigest reports widget fetch errors as absent data", () => {
  const request = parseQueryRequest(
    queryRequest({
      messages: [
        { role: "human", content: "q" },
        {
          role: "tool",
          function: "get_widget_data",
          input_arguments: { data_sources: [{ widget_uuid: priceWidget.uuid }] },
          data: [{ error_type: "widget_failed", content: "boom" }],
        },
      ],
    }),
  );
  assert.equal(buildWidgetDigest(request, []).block, null);
});

test("composeQuestion appends the digest block", () => {
  const composed = composeQuestion("질문", null, "블록");
  assert.equal(composed, "질문\n\n블록");
  assert.equal(composeQuestion("질문", null, null), "질문");
});

test("forced slash selected_skills are parsed and turned into a directive", () => {
  const request = parseQueryRequest({
    messages: [{ role: "human", content: "AAPL 실적 분석해줘" }],
    selected_skills: [
      {
        slug: "conservative-investor",
        description: "보수적 투자자 관점",
        contentMarkdown: "## 관점\n- 안전성 우선\n- 배당 중심으로 평가",
        source: "forced_slash",
      },
    ],
  });
  const directive = buildSkillDirective(request);
  assert.ok(directive !== null);
  assert.ok(directive.includes("[사용자 지정 스킬 지시 — OpenBB Workspace /스킬]"));
  assert.ok(directive.includes("conservative-investor"));
  assert.ok(directive.includes("안전성 우선"));
});

test("the shipped dcf-valuation skill flows through the directive path intact", () => {
  // /dcf (P4): the workspace owns slash skills, the bridge owns the shipped
  // content. This pins that the artifact composes cleanly end to end.
  const skillMarkdown = readFileSync(
    new URL("../skills/dcf-valuation.md", import.meta.url),
    "utf8",
  );
  assert.ok(skillMarkdown.includes("quant.dcf"));
  assert.ok(skillMarkdown.includes("민감도"));
  const request = parseQueryRequest({
    messages: [{ role: "human", content: "AAPL 적정가 분석해줘" }],
    selected_skills: [
      {
        slug: "dcf",
        description: "DCF 밸류에이션 렌즈",
        contentMarkdown: skillMarkdown,
        source: "forced_slash",
      },
    ],
  });
  const directive = buildSkillDirective(request);
  assert.ok(directive !== null);
  assert.ok(directive.includes("(dcf · DCF 밸류에이션 렌즈)"));
  assert.ok(directive.includes("quant.dcf"));
});

test("skill directive accepts snake_case content and caps oversized skills", () => {
  const long = "지시 항목입니다. ".repeat(600);
  const request = parseQueryRequest({
    messages: [{ role: "human", content: "q" }],
    selected_skills: [
      { slug: "s", description: "d", content_markdown: long, source: "forced_slash" },
    ],
  });
  const directive = buildSkillDirective(request) ?? "";
  assert.ok(directive.includes("지시 항목입니다"));
  assert.ok(directive.length <= 4_200);
  assert.ok(directive.includes("…(스킬 지시 초과분 생략)"));
});

test("skill-less requests and catalog-only requests yield no directive", () => {
  assert.equal(buildSkillDirective(parseQueryRequest({ messages: [{ role: "human", content: "q" }] })), null);
  const catalogOnly = parseQueryRequest({
    messages: [{ role: "human", content: "q" }],
    skills_catalog: [{ slug: "s", description: "d", updatedAt: "2026-09-02" }],
  });
  assert.equal(buildSkillDirective(catalogOnly), null);
});

test("composeQuestion orders question, skill directive, then widget digest", () => {
  const composed = composeQuestion("본 질문", "[사용자 지정 스킬 지시 — X]\n지시", "[화면 위젯 관측 — Y]\n관측");
  const questionAt = composed.indexOf("본 질문");
  const skillAt = composed.indexOf("[사용자 지정 스킬 지시");
  const digestAt = composed.indexOf("[화면 위젯 관측");
  assert.ok(questionAt < skillAt && skillAt < digestAt);
});

test("gateway run body never carries the disabled guru lens", () => {
  const body = gatewayRunBody("MSFT", "질문");
  assert.deepEqual(Object.keys(body).sort(), ["question", "schema_version", "ticker"]);
  assert.ok(!("advisor_lens" in body));
});

test("parseFollowUps reads the Korean numbered section and stops at the next header", () => {
  const markdown = [
    "본문입니다.",
    "",
    "### 이어서 볼 질문",
    "",
    "1. 매출 성장률은?",
    "2. 마진 전망은?",
    "3. 리스크는?",
    "4. 초과 항목은?",
    "",
    "### 출처",
    "",
    "[^1]: 10-K",
  ].join("\n");
  assert.deepEqual(parseFollowUps(markdown), ["매출 성장률은?", "마진 전망은?", "리스크는?"]);
  assert.deepEqual(parseFollowUps("### Suggested Follow-up Questions\n\n- Growth?\n"), ["Growth?"]);
  assert.deepEqual(parseFollowUps("후속 질문 없음"), []);
});

test("visualizationArtifacts merges krw series onto shared period rows", () => {
  const visualization = {
    title: "매출 추이",
    views: [
      {
        view_id: "v1",
        chart_type: "bar",
        title: "연간 매출",
        series: [
          {
            series_key: "s1",
            label: "매출",
            points: [
              { period: "FY2024", value: 100, period_sort_key: 2024 },
              { period: "FY2025", value: 120, period_sort_key: 2025 },
            ],
          },
          {
            series_key: "s2",
            label: "영업이익",
            points: [
              { period: "FY2024", value: 30, period_sort_key: 2024 },
              { period: "FY2025", value: 41, period_sort_key: 2025 },
            ],
          },
        ],
      },
    ],
  };
  const events = visualizationArtifacts([visualization]);
  assert.equal(events.length, 1);
  const payload = JSON.parse(events[0]?.data ?? "{}") as {
    type: string;
    content: Record<string, unknown>[];
    chart_params: { chartType: string; xKey: string; yKey: string[] };
  };
  assert.equal(payload.type, "chart");
  assert.equal(payload.chart_params.chartType, "bar");
  assert.deepEqual(payload.chart_params.yKey, ["매출", "영업이익"]);
  assert.deepEqual(payload.content, [
    { period: "FY2024", 매출: 100, 영업이익: 30 },
    { period: "FY2025", 매출: 120, 영업이익: 41 },
  ]);
});

test("visualizationArtifacts caps points and skips malformed views", () => {
  const many = Array.from({ length: 100 }, (_, index) => ({
    period: `P${index}`,
    value: index,
    period_sort_key: index,
  }));
  const events = visualizationArtifacts([
    { title: "t", views: [{ chart_type: "line", series: [{ label: "L", points: many }] }] },
    { title: "broken", views: [{ chart_type: "line", series: [] }] },
    "not-an-object",
  ]);
  assert.equal(events.length, 1);
  const payload = JSON.parse(events[0]?.data ?? "{}") as { content: unknown[] };
  assert.equal(payload.content.length, 60);
});

test("promptSuggestions caps at three entries", () => {
  const payload = JSON.parse(promptSuggestions(["a?", "b?", "c?", "d?"]).data) as { suggestions: string[] };
  assert.deepEqual(payload.suggestions, ["a?", "b?", "c?"]);
});
