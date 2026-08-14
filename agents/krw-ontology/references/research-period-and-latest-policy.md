---
name: research-period-and-latest-policy
description: "Period and latest policy. Defines 10-Q as current driver over 10-K, safe reporting-period labels, and inflection timing rules."
when_to_use: "when the relevant analysis context arises"
---

# Period And Latest Policy

## User-facing labels

Use filing- and observation-based labels in final answers. `CY...` is a stable
ontology routing bucket, not a user-facing fiscal-period label.

```text
Good: 2026년 3월 말 종료 분기, 2026년 10-Q, 2025년 10-K
Bad: an internal routing code, or a guessed fiscal quarter
```

If the issuer fiscal calendar matters and the evidence explicitly supplies its
label, use that exact label. Otherwise use an observed end date and basis, or
a neutral filing label.

## Fiscal calendar label guard

The ontology period label is never the default user-facing label. Some issuers'
fiscal quarters do not match a calendar routing bucket.

When retrieved evidence or source text uses an issuer fiscal label that differs
from a neutral filing label, show the source fiscal label with the filing only
when it is directly supplied by the evidence:

```text
Korean: Micron FY2026 Q3 10-Q
English: Micron fiscal Q3 2026 10-Q
```

After first mention, prefer `the latest 10-Q`, `latest quarter`, an observed
end date/basis, or the neutral filing label. Do not derive a fiscal label from
the routing period.

Do not infer that a newer filing period exists because a source text says
issuer fiscal Q3/Q4. Treat issuer fiscal labels as aliases for the confirmed
filing, not proof of another document.

## Runtime filing anchor

If the web runtime context provides a `Company filing anchor`, obey it.

Use the provided `Current driver` as the starting point for current company analysis and investment-decision answers.

Use the provided `Annual baseline` only for annual business mix, historical trend, and risk baseline.

Use `Historical context` only for cycle comparison or change over time.

Do not let older 10-K evidence dominate the current judgment when a Current driver is provided.

## ResearchState source anchors

Read `ResearchState.source_anchors` before applying generic document ordering.
Each anchor contains `ticker`, `period`, `document_type`, `role`, and
`source_label`.

Use returned roles as follows:

```text
current_driver
  use for current changes, latest-quarter evidence, financial impact, cost,
  cash flow, risk, and management commentary

annual_baseline
  use for business mix, segment structure, annual revenue/cost baseline,
  long-term risk baseline, and historical context

latest_available
  use as the latest confirmed 10-Q/10-K source

retrieved_evidence
  treat as a neutral evidence anchor; do not infer current/annual role from the
  label alone
```

Do not invent an anchor or filing role that is absent from ResearchState.

## Confirmed period boundary

Treat the runtime as-of date and filing availability as separate facts. The current date does not prove that a filing, earnings release, or reporting period exists.

Use an explicit reporting-period label only when it appears in:

```text
the runtime Company filing anchor
retrieved evidence
the user's request
```

Do not infer that the next quarter, annual report, or filing is available from the current date or the latest confirmed period. Do not invent an unconfirmed future period label in prose, tables, conclusions, or follow-up prompts.

For unpublished or unconfirmed future evidence, use:

```text
다음 실적 발표
향후 공시
다음 실적 업데이트
```

If the user explicitly names a future period, repeat it only while clearly describing it as future or not yet confirmed.

## Default period anchor

Unless the user asks for a specific historical period, specific filing, or long-horizon trend, start with the most recent available filing evidence by filing/period recency.

Use older filings as baseline context:

```text
annual revenue mix
business model baseline
historical trend
change, persistence, improvement, or deterioration
```

## Recent/latest questions

If the user says:

```text
최근
최신
최근 매출 동인
최근 분기
latest
recent
most recent
```

the same default applies with extra force: start with the most recent available filing by filing/period recency. A newer 10-Q beats an older 10-K for current drivers, financial impact, cost, cash flow, risk, and management commentary; a latest 10-K is primary only when no newer 10-Q exists.

Example: if the available documents include a 2025년 10-K and a newer 2026년
10-Q, lead with the newer 10-Q for current drivers and use the 10-K as annual
mix/business-baseline context.

If multiple quarterly filings are confirmed as available, lead with the most recent confirmed quarter.

Do not say that the latest 10-Q needs to be checked when it was available through tools or provided as the Current driver. Use it as the current driver, or state clearly that the latest 10-Q was not confirmed in the retrieved evidence.

## Investment-decision anchor

For buy/sell/hold/timing questions, anchor the judgment on the latest available filing.

```text
Current driver: latest available 10-Q when it exists
Annual baseline: latest 10-K when a newer 10-Q exists
Historical context: older 10-K/10-Q periods
```

Do not let older annual filing evidence dominate the final decision if a newer quarterly filing is available. Older 10-K evidence may explain business mix, segment structure, historical cycle, and risk baseline, but the current investor judgment should start from the newest filing driver.

## Business model plus recent drivers

Answer order:

```text
1. most recent filing drivers/current changes
2. annual mix/business baseline when it is not already the most recent filing
3. interpretation and caveats
```

Do not open with annual-only framing when the user explicitly asks for recent drivers.
