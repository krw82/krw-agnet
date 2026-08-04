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
