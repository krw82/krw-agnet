# 최적화 루프 로그 — 2026-09-07 (이터레이션 1~2)

무한 루프: 질문 → 답변 확인 → 0부터 문제점 검토 → 수정 → 반복.
슬라이스 1(2026-09-04, 4문 통과) 이후 첫 루프 이터레이션.

## 이터레이션 2 (같은 날): 계약 드리프트 — C문 부분통과의 진짜 원인

배터리 재실행 결과: **A(SO) 통과, B2(PLTR) 통과, C2(반도체) 재사망**.
C2 사망 원인을 capabilityd 직접 재현으로 특정:

- 모델이 `ontology.query_universe` 호출에 `response_format: "json"`을
  붙임 → 라이브 릴리스의 서버 계약은 이 필드를 **extra_forbidden**로
  거절(input_correction_required, 모델 수정 가능 4xx).
- 그런데 커널의 정규 내보내기 스키마(체크아웃 = 미래 코드 기준)는 이
  필드를 **허용** — 즉 "체크아웃 ↔ 라이브 릴리스" 계약 드리프트가
  커널 검증을 통과시키고 서버에서 죽였다. 실패는 ambiguous(재시도
  가능)로 분류되지만 회수 예산을 태우고 dependency_unavailable 폴백
  (영어 인용 덤프)로 직행.
- **수정 81fd707**: 런타임 검증자(수제 Rust)에서 `response_format`을
  모델 저작 표면에서 제거 — 정규 스키마/해시 핀은 불변, company_context
  의 기존 교리("전송 어휘는 모델 표면이 아니다")와 동일. trace 입력도
  동일 처리. 필드 제거 시 서버가 정상 응답(38KB)함을 실측 확인.
- **C3 통과**: AMAT/ADI/AMD 회사별 재고 수치(온톨로지 공시) + 사이클
  국면 판독(조정→소진→재충전, 과잉 vs 정상 재충전 기준 제시) + 양방향
  시나리오 + 범위 갭 고지 + 후속 질문. 세 문(커버/비커버/무티커)
  전부 통과.

알려진 잔여 마찰: 모델에게 광고되는 도구 스키마(정규 내보내기)에는
필드가 남아 있어 첫 호출 1턴이 수리로 소비될 수 있음 — 차기
후보로 model_input_contract 쌍 도입(광고 정합) 기록.

## 배터리와 판정 (이터레이션 1)

| 문 | 질문 | 런 | 1차 판정 | 근거 |
| --- | --- | --- | --- | --- |
| A (커버 회사) | SO 최근 실적은? | run_f7acb45f | **통과** | 공시급 심층: 상반기 순이익 $25.31억(+14%), OCF $42.8억, 가스 비용 전가 87%, O&M 분해(보수 +$30M, Nicor +$11M, 법무 −$20M), Southern Power 스윙. 조건부 전망 + 관찰 지점 + 데이터 갭 고지. |
| B (비커버) | PLTR 실적발표 이후 분위기? | run_0b02e610 | 1차 부분 → **B2 통과** | B2(배치 드레인 후): 본체 실적(순이익 $10.6억, EPS 0.41, 매출총이익 16.4억) + 내부자 거래 색 + 역합의 프레이밍("강한 실적 + 높은 기대치 줄다리기"). |
| C (무티커 업황) | 미국 반도체 업황? 재고 사이클 | run_0248ec1b | 1차 부분 → **C3 통과** | 계약 드리프트 수정 후: 회사별 재고 수치 + 사이클 국면 판독 + 양방향 시나리오. |

## 이터레이션 1 수정 (7 커밋)

1. **a2dd806** openbb.news(news_company, fmp) wiring — 라운드4.
   스키마/JCS 해시/계약 매처/디스패치 암(기사 수 1..=10, 기본 5, 최근 2주
   윈도는 업스트림 소유)/agent.yaml 레지스트리+도구/프롬프트(최근 사건
   컬러 의무). 배포 바인딩 예약(deployments/local) + capability-runtime
   어드미션 목록 등록 포함(별도 2 커밋).
2. **8b58674** 플래너 원장 단조성: `apply_intent_progress`가 재유도
   커버리지 하락을 보면 런 전체를 죽이던 것(IntentGoalProgressDrift,
   라이브 run_cd071289 사망)을 상위 수역 병합으로 교체. 재현 테스트
   `rederived_lower_clause_coverage_keeps_committed_progress`. 라이브
   A 런(2개 인텐트 query_context)이 이후 생존·통과.
3. **eaa88e6** 자유 문 관측 배치 드레인: question_only ProposalUnmapped
   오버라이드가 리더만 실행하던 것을 ≤4 전부 pending_supplemental_calls
   큐로 드레인(각 호출은 동일 상태머신 전환·검증 통과).
4. **3720bb6** (krw-agent-service) 게이트웨이 429 흡수(제출+폴, 상한
   지수 백오프) + 러너 진행 콜백 → SSE 중간 reasoning step(60초 간격,
   "N분 경과"). 3동시 SSE 폴링이 120req/min 가드를 치면 서비스가 런을
   버리고 거절하던 라이브 결함.
5. fmt 정규화(1fbb178 등).

오프라인: contracts 37 / agent-image 33 / run-engine 179 /
research-planner 58 / agent-service 94 — 전부 통과.

## 확인된 외부 블로커 (내 샌드박스 밖)

- **실적발표 트랜스크립트**(equity_fundamental_transcript): 무지정·연도만
  → fmp 유료 `earnings-transcript-list` 402; quarter 지정 → OpenBB→FMP
  직렬화 버그 422(문자열 '2' literal 거부, PLTR/AAPL 공통). 해소 조건:
  fmp 플랜 또는 OpenBB Platform 수판 수정. 그 전까지 openbb.news가
  실적발표 커버리지 색을 대신 담당(프롬프트에 명시).

## 다음 이터레이션 백로그 (0부터 재검토 예정)

1. **온톨로지 일시 실패 회복**: query_universe 1건 실패(retryable_read=t)
   가 복구 예산을 태우지 않고 dependency_unavailable 폴백으로 직행한
   경로 추적 — 재시도 정책 또는 폴백 컴포저 자연화(한국어 서술형).
2. **상태별 배치 규칙 정합**: PLTR ep05 — post-discovery 상태에서
   4배치가 decision_batch_size_invalid로 거절(프롬프트의 "≤4 배치
   허용"과 불일치). 상태 머신 규칙을 프롬프트에 정확히 반영하거나
   해당 상태의 배치 허용 여부 재검토.
3. transcript 블로커 해소 후 라운드5 wiring(모델 계약
   {ticker, year, quarter 필수} — list 엔드포인트 402 회피).
4. 워크스페이스 경험: 진행 상황 스트리밍 배터리 검증, 인용 라벨
   한국어화, agents.json 기능 플래그(widget-dashboard-*) 재검토.
5. 잔여 openbb 도구 어휘 검증(~215건)과 4xx 재시도 분류는 슬라이스 1
   백로그에서 이월.

## 운영 메모

- 스택 재기동 절차: `cargo build -p krw-agent -p krw-agentd` 후
  boot-stack.sh(포그라운드 대기형 — run_in_background로 실행) →
  start-coverage-sidecar.sh / start-agent-service.sh. teardown.sh는
  사이드카까지 정리.
- 게이트웨이 레이트 가드 기본 120req/60s(노드 local-gateway.ts,
  KRW_AGENT_GATEWAY_RATE_LIMIT_MAX). 서비스 러너는 이제 429를 흡수.
