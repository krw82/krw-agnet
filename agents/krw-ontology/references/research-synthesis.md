---
name: research_synthesis
description: "Use when the composer role synthesizes evidence into the final answer. Conclusion-first order, fact versus interpretation separation, analyst estimation discipline, and Korean answer examples."
when_to_use: "when the relevant analysis context arises"
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
- Do not introduce a peer, industry-average, market-share, or "above/below
  competitors" comparison unless the received evidence contains the aligned
  counterpart or benchmark. A company-only growth figure may be called material
  for that company, but never "fast in the industry/market" — even a caveat
  still makes that unsupported comparison. It is not evidence that the company
  outperformed peers.
- Do not compare an annual value with one quarterly value as if they were two
  consecutive points in the same trend. Use annual-to-annual or comparable
  quarter-to-quarter observations; otherwise label them as separate snapshots.
- A causal statement needs a direct company explanation. Related industry
  commentary is not enough.
- A product, customer, or market named only in a risk factor is evidence of
  exposure, not proof that it caused the reported growth. Attribute a growth
  driver only to a direct driver statement; otherwise keep it as a risk or
  label the connection as an estimate.
- Treat the company name as a factual field, not a language-completion task.
  Use the name supplied by the company context or use the ticker alone. Never
  invent or expand a ticker into a company name; a wrong name undermines an
  otherwise well-supported research answer.
- When the user asks for the "most important" risk, make a useful analyst
  selection from the observed mechanism, but do not present it as an official
  company ranking unless the filing actually ranks it. Say why it is the first
  risk to watch for this question.
- An interpretation may explain investment significance, but must use words
  such as "시사합니다", "가능성이 있습니다", or "추가 확인이 필요합니다" when
  the filing does not state the conclusion directly.
- Explicit brevity in the user's question controls the visible answer length,
  not the depth of research. For requests such as "간단히", "짧게", or "한
  문단으로", give one concise conclusion and only the few facts and one
  caveat needed to support it; do not expose a background table or unrelated
  generic observations.

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

## Period-safe presentation

When both quarterly (10-Q) and annual (10-K) data are available, use them for
different jobs rather than lining them up as a single series. Show one of the
following comparisons, never an annual-versus-one-quarter arrow:

- annual trend: FY2024 versus FY2025;
- current operating read: CY2025 Q1 versus CY2026 Q1; or
- separate snapshots: FY2025 annual baseline and CY2026 Q1 latest quarter.

For separate snapshots, write the period in each sentence and avoid language
such as "improved from FY2025 to Q1" or "Q1 annualizes to." A single quarter
must not be annualized and presented as a reported historical result. If an
answer needs both periods, a safe form is: "FY2025 annual operating cash flow
was X. Separately, CY2026 Q1 operating cash flow was Y, versus Z in the prior-
year quarter." This preserves useful context without inventing a trend.

When the question names several data families, organize the answer around the
ones actually observed. Do not open with a research-process disclaimer such
as "only the evidence obtained" or "this execution". Start with the investor
conclusion, state the one material missing fact in ordinary language beside
the affected claim, and then give the next useful question.

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
