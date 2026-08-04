---
name: research-investment-decision-questions
description: "투자결정 질문 프레임 (지금 사? 보유? 팔아?) → 판단 라벨/대시보드/조건"
when_to_use: "company_research 리서치에서 필요할 때"
---

# Investment Decision Brief Rules

Use this reference only to convert vague buy/sell/hold-style Korean questions into an internal English investment brief before KRW ontology MCP calls.

This file is primarily an internal English brief rule.

It also defines a detailed but scannable final report shape, but only for the triggered buy/sell/hold-style questions in this reference.

Do not let this reference change ordinary research questions.

## Trigger Scope

Apply only when the user's main intent is a vague investment decision, timing, or holding question:

```text
지금 사?
사도 돼?
사 말아?
지금 들어가도 돼?
팔아야 해?
보유해도 돼?
장기적으로 괜찮아?
이 주식 괜찮아?
좋은 회사야?
지금 매수?
물타도 돼?
비중 줄여?
손절해야 해?
```

Do not apply this reference when the user asks a clearer normal research question:

```text
metric/numeric series
business model or company overview
risk/thesis/scenario analysis
direct exposure check
comparison
contract/event/factual lookup
sector/global/macro discovery
```

For normal research questions, keep the normal KRW ontology classification and normal internal English brief.

## Core Action

Do not translate the Korean question literally.

Convert it into a concise ontology-friendly English investment brief that asks what filing evidence is needed to judge the decision frame.

The internal brief should:

```text
- preserve ticker, company name, period, price, valuation, portfolio, or comparison assumptions from the user
- follow the web runtime Company filing anchor when provided
- use latest available filing evidence first
- treat the latest 10-Q as the current driver when available
- use the latest 10-K as annual business baseline when a newer 10-Q exists
- optimize for evidence retrieval, not final prose
- avoid definitive buy/sell recommendations, target prices, or ratings
```

Do not expose the internal English brief in the final Korean answer.

For investment-decision answers, the final judgment must be anchored on the web runtime Company filing anchor when provided. Start from `Current driver`, use `Annual baseline` only for business mix/annual trend/risk baseline, and use `Historical context` only for cycle comparison or change over time. If a newer 10-Q exists after the latest 10-K, use the 10-Q as the current driver and use the 10-K only as annual baseline/business mix context. Do not say the latest 10-Q needs to be checked if it was available through tools or provided as the Current driver.

## Evidence Dimensions For The Brief

Bias the brief toward decision-relevant filing evidence:

```text
revenue durability
demand indicators
segment performance
margin direction
cash-flow conversion
FCF quality
capex and reinvestment burden
debt, liquidity, and balance sheet risk
share count, dilution, SBC, and buybacks
capital allocation
management commentary and filing notes
material risks that could strengthen or weaken the thesis
```

Use numbers internally to verify the judgment, but the final answer should emphasize investor interpretation unless a number materially changes the decision frame.

## Brief Templates

Use one of these patterns as the internal English brief. Adjust only for the user's ticker/company, period, and explicit assumptions.

### New Buy / Entry Timing

For:

```text
지금 사?
사도 돼?
지금 들어가도 돼?
```

Internal brief:

```text
{Company} current new-buy decision frame: latest available filing evidence on growth durability, demand indicators, segment performance, margin direction, cash-flow conversion, capex and reinvestment burden, balance sheet and liquidity risk, dilution/SBC, capital allocation, and material risks. Assess what supports buying now versus waiting. Do not produce a definitive recommendation, rating, or target price.
```

### Buy Versus Wait

For:

```text
사 말아?
```

Internal brief:

```text
{Company} buy-versus-wait decision frame: filing-supported positives, negatives, current drivers, margin and cash-flow durability, capex burden, balance sheet risk, capital allocation, dilution/SBC, and material risks. Identify which filing conditions support a new buy, waiting, or avoiding the stock. Do not produce a definitive recommendation, rating, or target price.
```

### Hold

For:

```text
보유해도 돼?
계속 들고 가도 돼?
```

Internal brief:

```text
{Company} hold decision frame: whether latest available filing evidence shows the investment thesis is intact, improving, or deteriorating. Focus on revenue durability, demand indicators, segment performance, margin direction, cash-flow conversion, capex burden, balance sheet/liquidity, dilution/SBC, capital allocation, and material risks.
```

### Sell Risk

For:

```text
팔아야 해?
손절해야 해?
```

Internal brief:

```text
{Company} sell-risk decision frame: filing evidence for thesis break, growth slowdown, demand deterioration, margin compression, cash-flow deterioration, leverage or liquidity pressure, dilution, capital allocation deterioration, or worsening material risk exposure. Do not produce a definitive sell recommendation, rating, or target price.
```

### Long-Term Quality

For:

```text
장기적으로 괜찮아?
장투해도 돼?
```

Internal brief:

```text
{Company} long-term investment quality frame: business model durability, revenue visibility, segment quality, margin structure, reinvestment runway, cash generation, capital allocation discipline, balance sheet resilience, dilution/SBC, and structural risks. Assess whether filing evidence supports long-term quality versus watch-first caution.
```

### Business Quality Versus Stock Decision

For:

```text
좋은 회사야?
이 주식 괜찮아?
```

Internal brief:

```text
{Company} business quality versus stock decision frame: latest available filing evidence on durable growth, segment economics, margin quality, cash-flow conversion, reinvestment needs, balance sheet risk, dilution/SBC, capital allocation, and risks that could make the business high quality but the stock less attractive, or vice versa.
```

## Final Answer Boundary

After using the internal brief, follow the normal KRW ontology research answer rules.

For triggered buy/sell/hold-style questions only, write a detailed but scannable compact investment-decision report.

Do not make it a short quick answer unless the user explicitly asks for "짧게", "한 줄로", or "간단히".

Follow `references/plain-korean-investor-language.md` more strictly than in normal answers. The user is asking for a practical investment judgment, so translate professional filing analysis into plain Korean before presenting it.

The first visible sentence must be the practical filing-supported judgment.
This opening judgment sentence is outside the heading sequence and must appear before any Markdown heading, title, table, caveat, or disclaimer.

Good opening:

```text
내 판단: 신규 매수는 아직 성급함. 보유자는 유지 가능.
```

Do not start with a disclaimer such as "공시자료만으로는 판단할 수 없습니다" unless the user explicitly asks for a target price, personalized portfolio instruction, or exact valuation.

Use standardized decision labels:

```text
매수 후보
분할 관심
확인 후 접근
관망 우위
보유 가능
비중 축소 검토
리스크 우위
피하는 쪽
```

Exact output contract for triggered buy/sell/hold-style questions:

Start with the opening `내 판단:` sentence, then use the exact Korean heading order below. The heading order begins after the opening judgment sentence. Do not rename, omit, merge, or reorder these headings unless the user explicitly asks for "짧게", "한 줄로", or "간단히".

```text
내 판단: {filing-supported decision}. {new buyer and/or holder split judgment}.

# {Ticker or Company} 투자 판단 요약

## 판단 라벨
| 구분 | 판단 | 이유 |
|---|---|---|
| 신규 매수 | {standardized label} | {one-line filing-supported reason} |
| 기존 보유 | {standardized label} | {one-line filing-supported reason} |

## 판단 대시보드
| 항목 | 쉽게 말하면 | 투자 의미 |
|---|---|---|
| 성장/수요 | {plain Korean signal} | {what it means for buy/hold/sell judgment} |
| 수익성 | {plain Korean signal} | {what it means for earnings durability} |
| 현금흐름/투자 부담 | {plain Korean signal} | {what it means for financial flexibility or entry timing} |
| 재무 안정성/희석 | {plain Korean signal} | {what it means for shareholder risk} |
| 핵심 리스크 | {plain Korean signal} | {what would make the judgment worse or better} |

## 왜 이렇게 보나
Separate positive filing evidence from risk or confirmation points in Korean prose.

## 판단이 바뀌는 조건
Explain what future filing/earnings evidence would make the view more positive or more negative. Use qualitative condition language unless the user supplied a threshold or filings/guidance explicitly provide one.

## 최종 판단
Separate good company, good stock, and good entry timing.

## 이어서 볼 질문
End with exactly 3 short Korean follow-up prompts the user can send immediately.
```

Do not apply this report shape to ordinary research questions.

For triggered buy/sell/hold-style questions, do not omit `판단 라벨`, `판단 대시보드`, `왜 이렇게 보나`, `판단이 바뀌는 조건`, `최종 판단`, or `이어서 볼 질문` unless the user explicitly asks for a short answer.

Never start the final answer with `#`, `##`, a table, or a section label. The opening sentence must be visible first:

```text
내 판단: ...
```

Do not infer whether the user is in profit, loss, 익절, or 손절 상태 unless the user states cost basis, return, purchase price, or position status. For "손절할까?" questions, answer from filing-supported thesis-break risk and hold/sell risk, not assumed profit/loss.

Good opening for 손절 questions:

```text
내 판단: 공시 근거만 보면 손절은 아직 성급함. 보유자는 실적 추세가 꺾이는 신호를 확인할 때까지 유지 가능.
```

In the report, prefer qualitative investment interpretation over raw numeric tables. Use numbers only when they materially change the judgment. The dashboard should read like a general-investor judgment table, not a filing data table; use columns like `쉽게 말하면` and `투자 의미`, and avoid columns that are primarily raw figures such as "공시 근거" packed with numbers.

Avoid unexplained English terms and acronyms in this report. If a term like FCF, CapEx, SBC, gross margin, RPO, backlog, dilution, or WFE is necessary, explain it once in Korean and then use the Korean expression.

Do not invent numeric thresholds, percentage thresholds, valuation bands, time horizons, checklist cutoffs, allocation sizes, buy levels, sell levels, stop-loss levels, margin thresholds, growth thresholds, or "N out of M conditions" rules unless the user supplied them or the filing/guidance explicitly supports them. Use qualitative condition language instead.

Bad threshold:

```text
HBM 수주 가시성이 12개월 이상 확보되면 추가 매수.
마진이 60% 밑으로 내려가면 손절.
조건 중 2개 이상이 발생하면 비중 축소.
```

Good condition language:

```text
HBM 수주 가시성이 더 명확해지고, 고마진 제품 믹스가 유지된다는 증거가 쌓이면 판단이 긍정적으로 바뀐다.
마진 회복이 지연되고 수요 개선이 가격/재고 개선으로 연결되지 않는 흐름이 반복되면 손절 판단이 강해진다.
```

For these triggered questions, the three final follow-ups must be direct prompts the user can send immediately, not abstract analyst research topics, generic research questions, raw metric prompts, or requests for personal inputs. Render them as a numbered Markdown list using `1.`, `2.`, `3.`.

The goal is to reduce the user's next-step burden and create a natural second question. Do not ask the user to provide investment period, risk tolerance, target price, position size, cost basis, allocation, or other personal inputs unless the user already supplied them.

Default follow-up pattern:

```text
1. Continuation condition: what would keep, strengthen, weaken, or break the current judgment
2. Scenario split: upside / downside / sideways, or positive / negative / neutral
3. Opposite view: weak assumptions, downside risks, thesis-break signals, or conflicting company comments
```

Choose the set that matches the user's intent:

General investment-decision follow-ups:

```text
1. 이 판단이 유지되는 조건과 깨지는 조건을 나눠줘.
2. 상승·하락·횡보 시나리오별 체크포인트를 보여줘.
3. 반대로 봐야 할 리스크 신호만 따로 정리해줘.
```

Holder / stuck position / sell-risk follow-ups:

```text
1. 보유자가 계속 봐도 되는 조건만 정리해줘.
2. 매도 판단이 강해지는 신호만 따로 뽑아줘.
3. 상승·하락·횡보 시나리오별 체크포인트를 보여줘.
```

New-buy follow-ups:

```text
1. 지금 신규 매수자가 확인해야 할 조건만 정리해줘.
2. 기다려야 하는 이유와 지금 봐도 되는 이유를 나눠줘.
3. 상승·하락·횡보 시나리오별 체크포인트를 보여줘.
```

Keep follow-ups focused on the same company and the user's original decision type. Do not introduce new peer companies, tickers, or comparison prompts unless the user explicitly asked for comparison or named peers.

Good follow-ups:

```text
- 보유자가 계속 봐도 되는 조건만 정리해줘.
- 매도 판단이 강해지는 신호만 따로 뽑아줘.
- 이 종목을 버티기 어려워지는 공시 코멘트가 있는지 봐줘.
- 지금 신규 매수자가 확인해야 할 조건만 정리해줘.
- 기다려야 하는 이유와 지금 봐도 되는 이유를 나눠줘.
- 이 판단이 유지되는 조건과 깨지는 조건을 나눠줘.
- 상승·하락·횡보 시나리오별 체크포인트를 보여줘.
- 반대로 봐야 할 리스크 신호만 따로 정리해줘.
```

Bad follow-ups:

```text
내 투자기간과 리스크 기준을 알려주면 다시 정리해줄게.
{Ticker} vs {Peer} 중 지금 더 나은 쪽을 비교해볼까?
{Ticker}의 AI 수익화는 어떻게 봐야 하나요?
FY2027 가이던스가 시장 기대에 부합할까?
자사주 매입 규모와 자본 배분 정책을 분석해줘.
데이터센터 매출 성장률 둔화 속도가 핵심인가?
마진 영향은 무엇인가요?
리스크는 무엇인가요?
{Ticker}의 HBM 매출 비중은?
{Ticker}의 Capex 규모는?
{Ticker}의 현금성 자산과 부채는?
```

The final Korean answer may use clear non-prescriptive judgment labels such as:

```text
매수 후보로 볼 수 있음
공격적 매수보다는 확인 후 접근
기존 보유자는 유지 가능
신규 진입자는 관망에 가까움
장기 성장 투자자에게 더 맞음
현금흐름 중시 투자자에게는 부담이 큼
단기 반등 근거는 약함
공시상 thesis가 깨졌다는 신호는 아직 약함
```

Avoid:

```text
무조건 매수
무조건 매도
지금 사세요
팔아야 합니다
목표가는 얼마입니다
투자의견 매수/보유/매도
개인 투자 조언
```
