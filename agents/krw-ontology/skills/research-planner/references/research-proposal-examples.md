# ResearchProposal v4 examples

These are semantic examples for the Flash planner. They are not physical MCP
requests. Each example is a complete provider function argument.

## 1. One direct numeric trend

Question meaning: “Show the last three annual revenue values and trend.”

```json
{
  "proposal": {
    "intent": "revenue_trend_10k",
    "answer_scope": "direct",
    "uncertainty": "low",
    "document_types": ["10-K"],
    "periods": ["FY2022", "FY2023", "FY2024"],
    "objectives": [
      {
        "priority": "required",
        "alternatives": [
          { "terms": ["net sales", "revenue"] },
          { "terms": ["total net sales", "annual revenue"] }
        ],
        "directness": "direct_required",
        "object_types": ["MetricObservation"],
        "goal": {
          "kind": "metric_time_series",
          "metric": "revenue",
          "metric_dimensions": []
        }
      }
    ]
  }
}
```

Use `metric_time_series` for a sequence or trend. Do not invent a growth-rate
calculation merely because the user asks whether a trend rose or fell.

## 2. A requested year-over-year calculation

Question meaning: “What was annual revenue growth?”

```json
{
  "proposal": {
    "intent": "revenue_growth_10k",
    "answer_scope": "direct",
    "uncertainty": "low",
    "document_types": ["10-K"],
    "periods": ["FY2023", "FY2024"],
    "objectives": [
      {
        "priority": "required",
        "alternatives": [
          { "terms": ["net sales", "year over year"] },
          { "terms": ["revenue", "annual growth"] }
        ],
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

Use `metric_change` only when the change itself is requested. The kernel
derives the pair of observations and calculation lineage.

## 3. One direct qualitative claim

Question meaning: “Does the filing directly link weaker customer demand to
revenue pressure?”

```json
{
  "proposal": {
    "intent": "customer_demand_revenue_pressure",
    "answer_scope": "direct",
    "uncertainty": "medium",
    "document_types": ["10-K", "10-Q"],
    "periods": ["FY2024"],
    "objectives": [
      {
        "priority": "required",
        "alternatives": [
          { "terms": ["customer demand", "revenue"] },
          { "terms": ["demand", "net sales"] }
        ],
        "directness": "direct_required",
        "object_types": ["NarrativeEvidence"],
        "goal": {
          "kind": "qualitative_evidence",
          "concepts": ["customer demand", "revenue"],
          "predicates": ["pressures", "reduces"]
        }
      }
    ]
  }
}
```

The goal asks for direct evidence. Related market commentary is not a
substitute for the requested causal link.

## 4. A number and its explanation must be separate objectives

Question meaning: “How did revenue change, and what explanation did the
company give?”

```json
{
  "proposal": {
    "intent": "revenue_change_and_company_explanation",
    "answer_scope": "direct",
    "uncertainty": "medium",
    "document_types": ["10-K"],
    "periods": ["FY2023", "FY2024"],
    "objectives": [
      {
        "priority": "required",
        "alternatives": [
          { "terms": ["net sales", "year over year"] },
          { "terms": ["revenue", "annual growth"] }
        ],
        "directness": "direct_required",
        "object_types": ["MetricObservation"],
        "goal": {
          "kind": "metric_change",
          "metric": "revenue",
          "metric_dimensions": [],
          "change": "growth_rate",
          "window": "year_over_year"
        }
      },
      {
        "priority": "required",
        "alternatives": [
          { "terms": ["revenue increase", "due to"] },
          { "terms": ["net sales", "primarily driven by"] }
        ],
        "directness": "direct_required",
        "object_types": ["NarrativeEvidence"],
        "goal": {
          "kind": "qualitative_evidence",
          "concepts": ["revenue", "company explanation"],
          "predicates": ["driven by", "due to"]
        }
      }
    ]
  }
}
```

Never put a metric and qualitative concepts/predicates inside one objective.
They need different evidence and are independently verified.

## 5. Preserve focus with a deferred extension

Question meaning: “Give the revenue trend; discuss geographic mix only if it
materially explains the result.”

```json
{
  "proposal": {
    "intent": "revenue_trend_with_optional_geography",
    "answer_scope": "direct",
    "uncertainty": "medium",
    "document_types": ["10-K"],
    "periods": ["FY2022", "FY2023", "FY2024"],
    "objectives": [
      {
        "priority": "required",
        "alternatives": [
          { "terms": ["net sales", "revenue"] }
        ],
        "directness": "direct_required",
        "object_types": ["MetricObservation"],
        "goal": {
          "kind": "metric_time_series",
          "metric": "revenue",
          "metric_dimensions": []
        }
      },
      {
        "priority": "deferred",
        "alternatives": [
          { "terms": ["geographic net sales", "revenue"] }
        ],
        "directness": "direct_preferred",
        "object_types": ["MetricObservation"],
        "goal": {
          "kind": "metric_time_series",
          "metric": "segment_revenue",
          "metric_dimensions": ["geographic"]
        }
      }
    ]
  }
}
```

At least one objective must be `required`. A deferred objective must not turn
a narrow question into broad exploratory research before the evidence shows it
is material.
