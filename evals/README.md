# KRW deterministic semantic evaluations

이 디렉터리는 모델을 채점하는 벤치마크가 아니라, KRW 에이전트가 만들어 낸 구조화된 중간 산출물에 적용할 결정론적 계약 fixture다. 네트워크, DeepSeek, MCP, 데이터베이스를 호출하지 않는다.

현재 `krw-semantic/v2`는 다음 경계를 긍정·부정 JSON 사례로 고정한다.

- 라우터의 여섯 `run_kind` 구분과 실패 시 기본값
- 모델 제안 `ResearchIntent`와 물리 MCP root `SearchPlan v2`의 분리, `query_context` 선행,
  원자적 clause, 사용자 literal 보존
- 직접/관련 근거와 강한 주장 허용선
- 계산 lineage와 기간·단위·scope 정렬
- 확인된 최신 10-Q/current driver와 최신 10-K/annual baseline, 미확인 미래 기간 금지
- 정확히 세 개의 후속 질문, 내부 용어 차단, 의사결정 질문에만 전용 heading 허용
- 뉴스 결론의 회사 직접근거 gate
- 아이디어 A/B/C/Reject 근거 하한
- 시나리오 입력의 공시 사실/공시 기반 가정/사용자 가정/분석가 추론 분리
- Guru 고정 author, 단일 sealed 핵심 긴장, 실행 순서, runtime-owned 필드, 제한 verdict, 비사칭

## 실행

저장소 루트에서:

```sh
python3 scripts/validate_semantic_evals.py --suite evals/krw-semantic/v2
```

리포트를 파일로도 남기려면:

```sh
python3 scripts/validate_semantic_evals.py \
  --suite evals/krw-semantic/v2 \
  --report /tmp/krw-semantic-report.json
```

표준 출력은 항상 JSON이다. 모든 fixture와 coverage gate가 맞으면 종료 코드 `0`, 예상 결과와 실제 규칙 판정이 다르면 `1`, fixture·manifest 자체가 손상됐으면 `2`를 반환한다. 파일 리포트는 임시 파일을 동기화한 뒤 원자적으로 교체한다.

## 변경 규칙

`policy.json` 또는 `cases.json`을 바꾸면 `suite_version`을 의도에 맞게 올리고 `manifest.json`의 canonical JSON SHA-256와 `case_count`를 갱신한다. validator는 다음도 fail-closed로 확인한다.

- 중복 JSON key, 비정상 숫자, 제어문자
- 파일·case·문자열·배열·객체·중첩 깊이 상한
- 고유 case ID와 알려진 validator/rule만 사용했는지
- 모든 필수 rule에 정상 사례와 반례가 모두 있는지
- 반례가 정확히 사전 등록된 오류 코드만 만드는지
- policy/cases의 canonical content hash가 manifest와 일치하는지

이 결과는 fixture에 적힌 경계를 결정론적으로 통과했다는 뜻일 뿐이다. 자연어 답변의 전반적 품질, 실제 모델 비열등성, 최신 공시의 사실 정확성을 증명하지 않는다. 그런 주장은 별도의 blind holdout, 동일 예산 비교, 사람 판정을 거쳐야 한다.

`mcp_arguments`는 물리 MCP 호출의 root `SearchPlan`이다. 모델이 직접 쓰는 도구 입력은
별도의 좁은 `ResearchIntent`이며, trusted planner가 이를 검증하고 최소 충분 `SearchPlan`으로
컴파일한다. 따라서 `{"search_plan": {...}}` 형태는 호환 입력이 아니라 반드시 실패해야 하는
반례 fixture로만 존재한다.

## numeric-accuracy — 범위 제한(scooping) 채점 의미론

`scripts/grade_numeric_accuracy.py`는 `evals/numeric-accuracy/ground_truth.json`의
`expected_metrics`를 답변 텍스트에서 검사한다. 과거에는 답변 전체에서 추출한 모든 숫자를
대상으로 기대값에 가장 가까운 것을 고르는(closest-match) 방식이라서, 다른 지표나 다른 연도의
숫자가 빈 자리를 통과시킬 수 있었다. Task 8부터는 다음과 같이 바뀌었다.

- **문장 단위 범위 제한**: 답변을 문장(`. `, `다.`, `\n`, `! `, `? ` 기준)으로 쪼갠 뒤,
  한 문장 안에 (a) `metric` 키워드와 (b) `fiscal_year` 표시(예: `2024`, `FY2024`,
  `2024년`, `fiscal 2024`)가 **모두** 등장하는 문장에서만 숫자를 수집한다. `metric` →
  키워드 매핑은 `METRIC_KEYWORD_MAP`(`revenue`, `operating_income`, `net_income`,
  `eps`, `fcf`, `*_margin`, `revenue_growth`, `research_and_development`,
  `total_assets`, `total_liabilities`, `operating_cash_flow` 등 영문+한글)에 있다.
- **문맥 미발견 = 실패**: 해당 지표·연도 쌍을 함께 언급하는 문장이 없으면 그 지표는
  `metric_context_not_found` 사유로 **실패** 처리된다(빈 점수로 통과하지 않는다). 단, 단위가
  `percent`인 지표도 같은 방식으로 범위 제한된 `%` 수치만 비교한다.
- **PASS 게이트**: `scripts/run_eval_v2.sh`는 `state=final`을 전제로 한 뒤, qid가
  `ground_truth.json`에 `expected_metrics`를 가지면 numeric grader의 종료 코드(0=모두 통과,
  1=일부 실패)가 PASS를 결정한다. grader가 0이 아니면 `STATUS=FAIL`, 사유
  `numeric_mismatch`가 된다. qid가 ground_truth에 없거나 `expected_metrics`가 비어 있으면
  `state=final`만으로 PASS한다.
- **스키마 검증**: `python3 scripts/grade_numeric_accuracy.py --validate <ground_truth.json>`
  는 JSON 스키마, 중복 id, 단위/허용오차 범위 등을 검사해 유효하면 0, 아니면 1로 종료한다.
  CI와 Task 9에서 ground_truth 변경을 fail-closed로 막는 데 쓰인다.

변경 규칙: ground_truth에 새 지표(metric 식별자)를 추가할 때는
`METRIC_KEYWORD_MAP`에 영문/한글 키워드도 함께 넣어야 한다. 매핑이 비어 있으면 그 지표는
항상 `metric_context_not_found`로 실패한다.

## numeric-accuracy — 79-문항 분류 taxonomy (Task 9)

Task 9가 `evals/numeric-accuracy/ground_truth.json`을 19문항에서 79문항(기존 19 +
카테고리 50 + 공격 10)으로 확장했다. 분류는 `expected_metrics`의 유무와 함께 에이전트의
서로 다른 능력을 검사하도록 설계됐다.

| qid 구간 | 분류 | 난이도 | `expected_metrics` | 채점 기준 |
|---|---|---|---|---|
| q1–q19 | (원본 numeric) | easy/hard | 있음 | state=final + numeric grader (Task 8 범위 제한) |
| q20–q24 | 단일 숫자 (single number) | easy | 있음 (기존 19문에서 재사용) | numeric grader |
| q25–q29 | 여러 기간 비교 (multi-period) | medium | 없음 `[]` | state=final 단독 |
| q30–q34 | segment 비교 | hard | 없음 `[]` | state=final 단독 (사람 검수 병행) |
| q35–q39 | causal mechanism | hard | 없음 `[]` | state=final 단독 (정성 평가) |
| q40–q44 | 사업구조 해석 | medium | 없음 `[]` | state=final 단독 |
| q45–q49 | 리스크 | hard | 없음 `[]` | state=final 단독 |
| q50–q54 | 자본배치 (capital) | medium | 있음(q50–q52) / 없음(q53,q54) | 있으면 numeric, 없으면 state=final |
| q55–q59 | 충돌 공시 (conflict) | hard | 없음 `[]` | state=final 단독 |
| q60–q64 | 근거 부족 (insufficient evidence) | hard | 없음 `[]` | state=final 단독 + 사람 검수(정답이 “답 불가”여야 함) |
| q65–q69 | 잘못된 전제 (false premise) | hard | 없음 `[]` | state=final 단독 + 사람 검수(전제 정정 여부) |
| q70–q79 | 공격형 (attack cases) | hard | 있음 (기존 19문에서 재사용) | numeric grader — 잘못된 연도/지표 숫자가 통과하지 않는지 검증 |

### 값 출처 원칙 (binding)

- **새로운 ticker+metric+year 조합에는 절대 숫자를 창작하지 않는다.** `krw-ontology-data`
  에서 값을 읽지 않고, 기존 19문(q1–q19)에 이미 존재하는 동일 ticker+metric+year 값만
  재사용했다. 새 조합은 모두 `expected_metrics: []`로 뒀다.
- `expected_metrics: []` 문항은 `scripts/run_eval_v2.sh`에서 `state=final`만으로 PASS
  처리된다(Task 8 로직). 단, q60–q69(근거 부족 / 잘못된 전제)는 정성 판정이 필요하므로
  사람 검수를 병행해야 한다.
- 공격형 q70–q79는 답변 텍스트에 잘못된 연도/지표 숫자가 함께 등장하더라도 Task 8의
  문장 단위 범위 제한(metric 키워드 + `fiscal_year` 표시가 같은 문장에 있어야 함) 때문에
  그 숫자가 빈 점수를 채우지 못한다. 이 동작은
  `python3 scripts/grade_numeric_accuracy.py <answer> --question-id q70` 등으로 직접
  확인할 수 있다.

### validator

`python3 scripts/grade_numeric_accuracy.py --validate evals/numeric-accuracy/ground_truth.json`
은 JSON 스키마, 중복 id, 단위/허용오차 범위를 검사하고, 빈 `expected_metrics`를 허용한다.
성공 시 `OK: <N> questions, <M> metrics, <K> qualitative (expected_metrics=[])`를
출력하고 종료 코드 0을 반환한다. CI에서 ground_truth 변경을 fail-closed로 막는 데 쓴다.
