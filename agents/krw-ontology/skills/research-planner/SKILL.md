---
name: research_planner_skill
description: "사용자 질문을 최소 증거 요청(ResearchProposal v4)으로 변환하는 플래너 절차. metric 식별자·목표 유형·required/deferred 분리 방법 포함"
when_to_use: "planner 역할이거나 corrected research decision이 필요할 때"
---

# KRW Research Planner

Use this skill only while the advertised role is a planner or the kernel asks
for a corrected research decision. It tells you how to turn a user's question
into a compact, executable evidence request without inventing physical search
details.

## 핵심 규칙 (반드시 지킬 것)

1. **metric 필드에는 canonical ID만** — ontology catalog에 있는 정확한 식별자 사용.
   aliases(`sales`, `net_profit`, `fcf`, `capex`)는 `alternatives[].terms`의 검색어로만 쓰고,
   `goal.metric` 필드에는 절대 쓰지 않는다.

2. **숫자와 설명은 별도 objective** — 한 objective에 metric goal과 qualitative goal을 섞지 않는다.

3. **최소한의 objective** — 질문에 답하는 데 필요한 최소 개수만. `required`는 답에 직접 필요한 것만.

4. **recovery 후 proposal을 다시 쓸 때** — 이전 proposal의 goal ID를 재사용하되, goal 정의(metric, kind,
   directness)를 바꾸지 않는다. 바꿔야 하면 완전히 새 objective를 추가하라.

## 주문서(ResearchProposal v4) 구조

```json
{
  "proposal": {
    "intent": "stable_lowercase_slug",
    "answer_scope": "direct",
    "uncertainty": "low|medium|high",
    "document_types": ["10-K", "10-Q"],
    "periods": ["FY2023", "FY2024"],
    "objectives": [
      {
        "priority": "required",
        "alternatives": [{ "terms": ["search phrase 1", "search phrase 2"] }],
        "directness": "direct_required",
        "object_types": ["MetricObservation"],
        "goal": { "kind": "...", ... }
      }
    ]
  }
}
```

### 필드 설명

| 필드 | 설명 | 예시 |
|---|---|---|
| `intent` | 주문의 이름. 질문을 요약하는 소문자 slug | `"aapl_revenue_trend"` |
| `answer_scope` | 답변 범위. 보통 `direct` | `"direct"` |
| `uncertainty` | 질문의 불확실성 | `"low"` (단일 숫자), `"medium"` (분석) |
| `document_types` | 어떤 공시에서 찾을지 | `["10-K"]`, `["10-K", "10-Q"]` |
| `periods` | 어떤 기간 | `["FY2024"]`, `["FY2023", "FY2024"]` |
| `objectives` | 증거 요청 목록 (1~5개) | 아래 참고 |
| `priority` | `required` (필수) 또는 `deferred` (선택) | |
| `alternatives[].terms` | 공시에서 검색할 키워드 | `["net sales", "revenue"]` |
| `directness` | 근거의 직접성 | `direct_required`, `direct_preferred`, `related` |
| `object_types` | 찾을 데이터 종류 | `MetricObservation`, `NarrativeEvidence` 등 |

## goal 종류 (5가지 주문 메뉴)

### 1. metric_observation — 한 시점 숫자

"2024년 매출은?" 같은 단일 숫자 질문.

```json
"goal": {
  "kind": "metric_observation",
  "metric": "revenue",
  "metric_dimensions": []
}
```

### 2. metric_time_series — 여러 시점 숫자

"3년간 매출 추이" 같은 시계열 질문.

```json
"goal": {
  "kind": "metric_time_series",
  "metric": "gross_margin",
  "metric_dimensions": []
}
```

### 3. metric_change — 변화량/증가율

"전년 대비 매출 증가율?" 같은 변화 질문.

```json
"goal": {
  "kind": "metric_change",
  "metric": "revenue",
  "metric_dimensions": [],
  "change": "growth_rate",
  "window": "year_over_year"
}
```

`change` 값: `"growth_rate"` (증가율), `"absolute_change"` (절대 변화량)
`window` 값: `"year_over_year"`, `"quarter_over_quarter"`, `"sequential"`

### 4. metric_difference — 비교

"제품 vs 서비스 부문 매출?" 같은 세그먼트 비교.

```json
"goal": {
  "kind": "metric_difference",
  "metric": "segment_revenue",
  "metric_dimensions": ["segment"]
}
```

### 5. qualitative_evidence — 글로 된 설명

"왜 마진이 올랐나?" 같은 인과관계/설명 질문.

```json
"goal": {
  "kind": "qualitative_evidence",
  "concepts": ["gross margin", "product mix"],
  "predicates": ["driven by", "due to"]
}
```

- `concepts`: 찾아야 할 주제 (1~4개)
- `predicates`: 주제 간 관계 (1~3개). concept이 2개 이상일 때 필수.

## canonical metric 전체 목록

`goal.metric` 필드에는 **반드시** 아래 식별자 중 하나를 사용:

| Canonical ID | 설명 | Aliases (검색어만, metric 필드 ❌) |
|---|---|---|
| `revenue` | 매출 | total_revenue, net_sales, sales |
| `revenue_growth` | 매출 증가율 | revenue_growth_rate, sales_growth |
| `segment_revenue` | 부문별 매출 | segment_sales |
| `gross_margin` | 매출총이익률 | gross_profit_margin |
| `gross_profit` | 매출총이익 | |
| `operating_margin` | 영업이익률 | |
| `operating_income` | 영업이익 | |
| `net_income` | 순이익 | net_profit |
| `net_margin` | 순이익률 | |
| `eps` | 주당순이익 | earnings_per_share |
| `cost_of_revenue` | 매출원가 | cost_of_goods_sold, cogs |
| `operating_expense` | 영업비용 | total_operating_expenses, sg_and_a |
| `research_and_development` | R&D 비용 | rd_expense |
| `selling_general_and_admin` | 판관비 | sga |
| `operating_cash_flow` | 영업현금흐름 | cash_from_operations |
| `capital_expenditures` | 자본지출 | capex |
| `free_cash_flow` | 잉여현금흐름 | fcf |
| `fcf_margin` | FCF 마진 | free_cash_flow_margin |
| `cash_and_equivalents` | 현금 | cash, total_cash |
| `total_assets` | 총자산 | |
| `total_liabilities` | 총부채 | |
| `total_debt` | 총차입금 | debt |
| `shareholders_equity` | 주주지분 | total_equity |
| `roe` | ROE | return_on_equity |
| `roa` | ROA | return_on_assets |

### ❌ 절대 하면 안 되는 것

```
"metric": "sales"        → alias. "revenue"를 쓸 것
"metric": "net_profit"   → alias. "net_income"를 쓸 것
"metric": "fcf"          → alias. "free_cash_flow"를 쓸 것
"metric": "ebitda"       → dictionary에 없음. 가장 가까운 canonical ID 사용
"metric": "gross_profit_margin" → alias. "gross_margin"을 쓸 것
```

## 실제 작성 절차

1. **질문 분석**: 사용자가 무엇을 알고 싶은지 파악. 숫자인가, 설명인가, 비교인가.

2. **metric 선택**: 위 표에서 질문에 가장 가까운 canonical metric 선택.
   - "매출" → `revenue`
   - "마진" → `gross_margin` 또는 `operating_margin` (정확히 어느 마진인지 확인)
   - "이익" → `net_income` (순이익) 또는 `operating_income` (영업이익)
   - "현금흐름" → `operating_cash_flow` 또는 `free_cash_flow`

3. **goal 종류 선택**:
   - "얼마야?" → `metric_observation`
   - "추이가?" → `metric_time_series`
   - "변화율?" → `metric_change`
   - "A vs B?" → `metric_difference`
   - "왜? 원인이?" → `qualitative_evidence`

4. **objective 분리**: 숫자와 설명이 모두 필요하면 별도 objective.
   예: "매출총이익률 추이와 원인 분석"
   - objective 1: `metric_time_series` + `gross_margin` (숫자)
   - objective 2: `qualitative_evidence` + concepts ["gross margin"] (원인)

5. **periods 설정**: 질문에서 언급한 기간. 비교 질문은 2개 이상.

6. **alternatives 작성**: 공시에서 실제로 쓰이는 검색어.
   - "매출"을 찾을 때: `["net sales", "revenue"]`, `["total net sales", "annual revenue"]`
   - "매출총이익률"을 찾을 때: `["gross margin", "gross profit"]`

## 완성된 예시들

### 예시 1: 단일 숫자 (q1)

질문: "애플 2024 매출?"

```json
{
  "proposal": {
    "intent": "aapl_fy2024_revenue",
    "answer_scope": "direct",
    "uncertainty": "low",
    "document_types": ["10-K"],
    "periods": ["FY2024"],
    "objectives": [
      {
        "priority": "required",
        "alternatives": [{ "terms": ["net sales", "revenue"] }],
        "directness": "direct_required",
        "object_types": ["MetricObservation"],
        "goal": { "kind": "metric_observation", "metric": "revenue", "metric_dimensions": [] }
      }
    ]
  }
}
```

### 예시 2: 2년 비교 숫자 (q2)

질문: "애플 2024 순이익과 전년 대비 변화?"

```json
{
  "proposal": {
    "intent": "aapl_net_income_yoy",
    "answer_scope": "direct",
    "uncertainty": "low",
    "document_types": ["10-K"],
    "periods": ["FY2023", "FY2024"],
    "objectives": [
      {
        "priority": "required",
        "alternatives": [{ "terms": ["net income", "net earnings"] }],
        "directness": "direct_required",
        "object_types": ["MetricObservation"],
        "goal": { "kind": "metric_time_series", "metric": "net_income", "metric_dimensions": [] }
      }
    ]
  }
}
```

### 예시 3: 숫자 + 원인 설명 (q3)

질문: "매출총이익률 추이와 원인 분석?"

```json
{
  "proposal": {
    "intent": "aapl_gross_margin_trend_and_drivers",
    "answer_scope": "direct",
    "uncertainty": "medium",
    "document_types": ["10-K", "10-Q"],
    "periods": ["FY2023", "FY2024"],
    "objectives": [
      {
        "priority": "required",
        "alternatives": [{ "terms": ["gross margin", "gross profit"] }],
        "directness": "direct_required",
        "object_types": ["MetricObservation"],
        "goal": { "kind": "metric_time_series", "metric": "gross_margin", "metric_dimensions": [] }
      },
      {
        "priority": "required",
        "alternatives": [{ "terms": ["gross margin", "due to", "product mix"] }],
        "directness": "direct_required",
        "object_types": ["NarrativeEvidence"],
        "goal": {
          "kind": "qualitative_evidence",
          "concepts": ["gross margin", "product mix"],
          "predicates": ["driven by", "due to"]
        }
      }
    ]
  }
}
```

### 예시 4: 변화율 (q70 변형)

질문: "매출 증가율?"

```json
{
  "proposal": {
    "intent": "aapl_revenue_growth",
    "answer_scope": "direct",
    "uncertainty": "low",
    "document_types": ["10-K"],
    "periods": ["FY2023", "FY2024"],
    "objectives": [
      {
        "priority": "required",
        "alternatives": [{ "terms": ["net sales", "year over year"] }],
        "directness": "direct_required",
        "object_types": ["MetricObservation"],
        "goal": {
          "kind": "metric_change",
          "metric": "revenue",
          "metric_dimensions": [],
          "change": "growth_rate",
          "window": "year_over_year"
        }
      }
    ]
  }
}
```

## recovery 시 주의사항

proposal이 거부되어 recovery 피드백을 받았을 때:

1. **detail 필드를 읽는다**: `detail.field`가 어디가 잘못됐는지, `detail.offending_value`가 뭐가 잘못됐는지, `detail.valid_alternatives`가 뭘 써야 하는지 알려준다.

2. **해당 필드만 수정한다**: 다른 objective나 필드를 건드리지 않는다.

3. **goal 정의를 바꿀 때 주의**: recovery 후 proposal을 다시 쓸 때, 같은 objective 위치의 `metric`이나 `kind`를 바꾸면 goal ID 충돌이 발생할 수 있다. metric을 바꿔야 하면 해당 objective의 goal 정의(metric + kind + dimensions)를 통째로 일관되게 유지하라.

4. **같은 실수를 반복하지 않는다**: 피드백이 `proposal_metric_identity_invalid`이면, metric 필드를 canonical ID로 바꾼다. alias를 또 쓰지 않는다.
