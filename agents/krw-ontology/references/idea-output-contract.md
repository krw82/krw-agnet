---
name: idea_output_contract
description: "idea_generation 최종 답변 구조. 우선순위 결론→후보 깔때기 표→A/B/C/Reject→차기 리서치 질문"
when_to_use: "idea_generation 최종 한국어 답변을 작성할 때"
---

# Output Contract

## Contents

```text
Opening
Candidate Funnel
Candidate Detail
Labels
Handoff
Final Style
```

## Opening

Start with one sentence that states the research priority, not an investment recommendation.

Good:

```text
공시 근거 기준으로는 VST와 ETN을 먼저 깊게 조사할 가치가 있습니다. 이는 매수 추천이 아니라 리서치 우선순위입니다.
```

Avoid:

```text
VST와 ETN을 사는 것이 좋습니다.
가장 수익률이 높을 종목은 VST입니다.
```

## Candidate Funnel

Use a compact interpretation table:

```text
| 등급 | 기업 | 후보로 잡힌 이유 | 실적 연결 | 왜 지금 보나 | 첫 반증 위험 |
```

Keep the normal list to 3-5 candidates. Include Reject rows only when they teach the user why a tempting false positive was removed.

## Candidate Detail

For each A candidate, include:

```text
왜 지금 보는가
공시에서 확인되는 직접 노출
매출·마진·현금흐름 연결 경로
가장 큰 부담
가장 먼저 틀릴 수 있는 지점
다음 상세 조사 프롬프트
```

For B and C candidates, keep detail shorter:

```text
현재 확인되는 연결
부족한 근거
등급이 올라가기 위한 조건
```

For Reject:

```text
왜 이 화면에서 제외했는가
어떤 연결이 증명되지 않았는가
```

## Labels

Use:

```text
A - 바로 상세 리서치할 후보
B - 조건 확인이 필요한 후보
C - 테마 연관성만 우선 확인된 후보
Reject - 이번 화면에서는 제외
```

Always clarify:

```text
등급은 리서치 우선순위이며 매수·매도 의견이 아니다.
```

The label depends on the screen, not the company's overall quality. A company
can be excellent but C or Reject for this specific screen.

## Handoff

End with 1-3 immediately reusable same-company prompts for the highest-priority candidates.

Heading:

```text
### 다음 상세 조사
```

Examples:

```text
1. VST의 데이터센터 전력 수요가 실제 매출과 현금흐름으로 연결되는지 자세히 분석해줘.
2. ETN의 최근 수주 신호가 매출총이익률 개선으로 이어지는지 확인해줘.
3. XYL이 B 후보에 머문 핵심 근거 공백만 자세히 봐줘.
```

Do not use the normal `이어서 볼 질문` exactly-three format.

## Final Style

```text
Korean Markdown
interpretation before raw figures
short paragraphs
plain Korean financial language
no internal implementation terms
no invented market, consensus, positioning, or valuation data
```
