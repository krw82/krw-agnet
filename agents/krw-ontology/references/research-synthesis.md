---
name: research_synthesis
description: "증거 기반 답변 합성 절차. 결론 선행·사실 vs 해석 구분·분석가 추정 규율·한국어 답변 예시 포함"
when_to_use: "composer 역할로 증거를 답변으로 합성할 때, '몰라요' 대신 추정+근거 답변이 필요할 때"
---

# Research synthesis

Use this reference only after the agent has received actual research results.
Write the answer from admitted evidence, not from a generic investment essay.

## Working order

1. Answer the user's actual question in the first two sentences.
2. State the relevant filing period and distinguish a confirmed fact from an
   interpretation.
3. Explain why the observed fact matters to an investor.
4. State the most material counter-signal, limitation, or missing comparison.
5. Suggest the smallest useful next question.

Do not broaden a narrow question into a full company review merely because
many related facts are available. If the evidence only supports a limited
answer, give that answer plainly and say what remains unconfirmed.

## Evidence discipline

- A direct filing statement can support a direct company fact.
- A numeric conclusion must preserve its metric, period, unit, currency, and
  comparison basis where those change the meaning.
- A comparison or trend needs the actual compatible observations; do not infer
  a trend from one value.
- A causal statement needs a direct company explanation. Related industry
  commentary is not enough.
- An interpretation may explain investment significance, but must use words
  such as "시사합니다", "가능성이 있습니다", or "추가 확인이 필요합니다" when
  the filing does not state the conclusion directly.

Never turn a missing fact, rejected proposal, tool error, or unavailable data
into a fact. Do not expose internal IDs, tool names, workflow steps, or
research diagnostics.

## Analyst estimation discipline

A professional analyst does not stop at "I don't know." When a direct filing
statement is unavailable, use the evidence you **do** have to build the most
likely interpretation, then label it clearly. An investor reading the answer
should learn something useful, not just be told that information is missing.

- When direct evidence is partial, combine it with related evidence
  (adjacent metrics, causal chain results, industry context) to form a
  reasonable estimate. State the estimate, the evidence it rests on, and the
  specific gap that remains.
- Prefer an evidence-grounded estimate over silence. "이 데이터만으로
  단정할 수는 없지만, A와 B를 함께 보면 C일 가능성이 높습니다" is more
  useful than "확인할 수 없습니다."
- Use `ontology.chain` results — connected BusinessActivity, ChangeEvent,
  and driver→outcome paths — as material for the estimate. A chain that
  links a product mix shift to a margin change lets you explain the margin
  even when the filing does not spell out the causal sentence.
- Never fabricate numbers. If you have no numeric evidence at all for a
  value, say so explicitly and offer the closest directional read (예: "구체적인
  수치는 이번 자료에 없지만, 관련 지표들이 상승세이므로 개선됐을 가능성이
  큽니다").
- Always state what would change your estimate. This turns a guess into a
  falsifiable analyst view.

## Normal answer example

Question meaning: "최근 연간 매출 흐름은 어떤가?"

When both quarterly (10-Q) and annual (10-K) data are available, use the
latest quarter as the main evidence and the annual filing as the baseline.
Show the comparison and trace the causal chain.

```markdown
## 결론

최신 분기 매출은 전년 동기 대비 약 X% 증가했습니다. 연간 기준으로도
성장 흐름이 이어지고 있으며, 매출 증가가 마진 개선으로 이어지는 구조입니다.

## 확인된 수치

| 항목 | 연간 (10-K) | 최신 분기 (10-Q) | 전년 동기 분기 |
| --- | --- | --- | --- |
| 매출 | 약 X억 달러 | 약 Y억 달러 | 약 Z억 달러 |
| 매출 성장률 | 약 A% | 약 B% (YoY) | — |

- 연간 매출은 FY2023 → FY2024에 걸쳐 증가했습니다 (10-K 기준).
- 최신 분기는 10-Q 기준이며, 연간과 분기 성장 흐름이 일치합니다.

## 매출 구성과 마진 연결고리

매출 증가의 질을 보려면, 매출이 어떤 부문에서 왔고 그것이 마진에
어떻게 연결되는지를 봐야 합니다.

- 매출 구성: [부문별 비중이 확인되면 제시]
- 인과 경로: [부문 믹스 변화 → 매출총이익률 → 영업이익률]

## 투자 의미

| 항목 | 쉽게 말하면 | 투자 의미 |
| --- | --- | --- |
| 성장 | 매출은 증가 | 사업 수요가 유지되고 있음 |
| 수익성 | 마진이 개선 또는 유지 | 성장이 이익으로 연결되고 있음 |

## 확인이 더 필요한 부분

1. 부문별(제품·서비스) 매출 내역을 확인할까요?
2. 매출 증가가 현금흐름으로 이어졌는지 볼까요?
3. 최근 분기에도 같은 성장 흐름이 유지됐는지 확인할까요?
```

## Evidence-limited example (analyst style)

When the direct causal sentence is missing from the filing, do not end the
answer at "확인할 수 없습니다." Combine what you have into a useful estimate.

```markdown
## 결론

공시가 매출 변화의 직접적인 원인을 한 문장으로 설명하지는 않습니다. 하지만
확보된 근거를 종합하면, 매출 감소는 주로 [추정 원인 A]와 [추정 원인 B]의
결합으로 보입니다.

## 확인된 것과 거기서 추론하는 것

- **직접 확인**: [공시에 있는 사실 — 예: "총 매출은 전년 대비 X% 감소"]
- **거기서 추론**: [분석가적 해석 — 예: "제품별 내역을 보면 iPhone 매출은
  줄었지만 Services는 늘었고, 이는 수요 위축보다 제품 믹스 변화로
  해석하는 게 자연스럽습니다"]
- **인과 추정의 근거**: ontology chain 결과에서 [driver → outcome 경로 —
  예: "제품 믹스 변화 → 단가 하락 → 매출 감소" 경로가 확인됩니다]

## 추정의 한계

이 해석은 [남은 불확실성] 때문에 확정적이지 않습니다. 구체적으로,
[관찰 X]가 나타나면 이 추정이 틀렸을 가능성이 있습니다.

## 다음에 확인하면 좋은 것

1. [추정을 가장 빠르게 검증할 수 있는 질문]
2. [추정이 틀렸을 때 대안 설명을 확인하는 질문]
3. [최근 분기 추세가 같은지 확인하는 질문]
```

The key shift: instead of declaring "cannot confirm" and stopping, state the
most likely interpretation, show the evidence chain it rests on, and identify
the observation that would overturn it. This is how a buy-side analyst
writes when the 10-K does not spell out the answer.
