# 파일링 이벤트 카탈로그·뉴스 폴백 연결 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 채팅 company_research 분석가에게 3단 증거 폴백 사다리(파일링 카탈로그 → 피드 뉴스 → 웹 뉴스)를 제공하여, 사건형·시장반응형 질문에서 "미확인"이 세 단계 확인 후에만 선언되게 한다.

**Architecture:** 이미 배포된 MCP 도구(search_catalog_filings / get_filing_brief / list_feed_items / get_feed_context)를 krw-ontology 에이전트 배포에 바인딩하고(krw-feed가 쓰는 것과 동일한 값), 웹 뉴스 3단은 capability-runtime 로컬 빌트인(FMP 엔진, 중립 이름 `news.web_search`, fail-open)으로 구현한다. 증거는 단계별 등급(direct/strong → related/medium)으로 장부에 적입되고, 프롬프트 3곳에 조회 의무·인용 형식·체크리스트를 추가한다. 테스트 모델은 glm-5.3-flash로 교체한다.

**Tech Stack:** Rust(crates/krw-contracts, krw-ontology-adapter, capability-runtime, run-engine, protocol, provider-wire), YAML(agent.yaml, deployment-binding, model-registry), 프롬프트 마크다운, python 품질 매트릭스 러너.

**Spec:** docs/superpowers/specs/2026-08-28-filing-event-catalog-wiring-design.md

## Global Constraints

- 수정 가능한 저장소는 `~/krw-agnet`뿐(`~/krw-ontology-front`, `~/krw-ontology`는 읽기 전용).
- 배포는 항상 DeepSeek, 테스트는 GLM(이제 `glm-5.3-flash`).
- 벤더 은닉: 사용자 답변·에이전트 프롬프트에 FMP/Yahoo Finance/야후파이낸스 문자열 금지. 도구 이름은 `news.web_search`, 인용은 원 매체명.
- 자격증명 리터럴 금지: 소스·예시·테스트에 사용 가능한 키를 문자열로 쓰지 않는다(환경 변수/시크릿 파일만).
- DB 쿼리는 파라미터 바인딩만(psql 변수 `:'var'` 허용).
- 커밋 메시지에 자격증명/내부 URL 포함 금지.
- cargo 실행: `PATH=$HOME/.rustup/toolchains/1.97.1-aarch64-apple-darwin/bin:$PATH cargo ...`
- 스펙 §3의 증거 등급 표가 모든 증거 코드의 기준: 카탈로그=direct/strong, 뉴스(2·3단)=related/medium.

---

### Task 1: 웹 뉴스 계약 스키마 2종 추가

**Files:**
- Create: `contracts/kernel/v1/schemas/krw-web-news-search-input-v1.json`
- Create: `contracts/kernel/v1/schemas/krw-web-news-search-result-v1.json`
- Modify: `crates/krw-contracts/src/lib.rs` (커널 스키마 등록 테이블 — `final-markdown-v1`이 등록된 동일 위치)
- Test: `crates/krw-contracts/src/lib.rs` 기존 유닛 테스트들이 스키마 로딩을 검증

**Interfaces:**
- Produces: 계약 id `krw-web-news-search-input/v1`(properties: `ticker` 필수 1~16자, `limit` 선택 1~10 기본 5)와 `krw-web-news-search-result/v1`(items: `headline`, `publisher`, `published_at`, `url`, `summary` 각 문자열, max_items 10). 이후 Task 2(agent.yaml contracts), Task 4(builtin)가 참조.

- [ ] **Step 1: 입력 스키마 작성** — `contracts/kernel/v1/schemas/krw-web-news-search-input-v1.json`:

```json
{"$id":"urn:krw-agent:contract:krw-web-news-search-input/v1","$schema":"https://json-schema.org/draft/2020-12/schema","additionalProperties":false,"properties":{"limit":{"default":5,"maximum":10,"minimum":1,"type":"integer"},"ticker":{"maxLength":16,"minLength":1,"type":"string"}},"required":["ticker"],"type":"object"}
```

- [ ] **Step 2: 결과 스키마 작성** — `contracts/kernel/v1/schemas/krw-web-news-search-result-v1.json`:

```json
{"$id":"urn:krw-agent:contract:krw-web-news-search-result/v1","$schema":"https://json-schema.org/draft/2020-12/schema","additionalProperties":false,"properties":{"items":{"items":{"additionalProperties":false,"properties":{"headline":{"maxLength":300,"type":"string"},"published_at":{"maxLength":40,"type":"string"},"publisher":{"maxLength":120,"type":"string"},"summary":{"maxLength":1000,"type":"string"},"url":{"maxLength":500,"type":"string"}},"required":["headline","publisher","published_at"],"type":"object"},"maxItems":10,"type":"array"}},"required":["items"],"type":"object"}
```

- [ ] **Step 3: 등록 위치 확인** — Run: `grep -n "final-markdown-v1" crates/krw-contracts/src/*.rs`
  Expected: 커널 스키마 바이트 등록 매크로/테이블 위치(예: `schema_bytes!(..., "final-markdown-v1.json")` 패턴)가 나타난다. 같은 패턴으로 두 신규 파일을 등록한다.
- [ ] **Step 4: 빌드·테스트** — Run: `PATH=$HOME/.rustup/toolchains/1.97.1-aarch64-apple-darwin/bin:$PATH cargo test -p krw-contracts`
  Expected: PASS (기존 테스트 전부; 신규 스키마가 JSON으로 파싱됨).
- [ ] **Step 5: Commit** — `git add contracts/kernel/v1/schemas/krw-web-news-*.json crates/krw-contracts/src/lib.rs && git commit -m "feat(contracts): add web news search input/result schemas"`

### Task 2: agent.yaml — capability 5종·워크플로·검증자·어휘 금지

**Files:**
- Modify: `agents/krw-ontology/agent.yaml` (contracts 목록 ~line 118 뒤, capabilities ~line 327~, workflows company_research_v2 ~line 518~, validators action_limits ~line 892~, answer_policy ~line 851~)
- Modify: `agents/check-all.sh` (krw-ontology 패키지의 기대 binding_key 목록에 신규 5종 추가 — 스크립트 하단 per-package 검증이 있으면)
- Test: `agents/check-all.sh` + `cargo test -p run-engine`

**Interfaces:**
- Consumes: Task 1 계약 id 2종, 기존 canonical:krw-front 계약(krw-filing-search-input/v1 등 — 해시는 Step 1에서 생성).
- Produces: capability id `filing.search_events`, `filing.event_brief`, `news.feed_list`, `news.feed_context`, `news.web_search`. 워크플로 상태 `event_filing_search`, `event_filing_brief`, `news_feed_lookup`, `news_feed_context`, `news_web_search`. 전이 이벤트 `filing_search_has_value`, `filing_brief_has_value`, `news_lookup_has_value`, `news_context_has_value`, `news_web_has_value` (Task 4·5와 이름 정합).

- [ ] **Step 1: 기존 front 계약 해시 계산** — krw-feed가 이미 선언한 4개 계약의 content_hash를 재계산해 동일 값임을 확인(계산 방법 검증):
  Run: `cd ~/krw-agnet && python3 - <<'PY'
import hashlib
for f in ["krw-filing-search-input-v1","krw-filing-search-result-v1","krw-filing-brief-input-v1","krw-filing-brief-result-v1","krw-feed-list-items-input-v1","krw-feed-list-items-result-v1","krw-feed-context-input-v2","krw-feed-context-v2"]:
    print(f, hashlib.sha256(open(f"contracts/krw-front/v1/schemas/{f}.json","rb").read()).hexdigest())
PY`
  krw-feed/agent.yaml의 해당 content_hash와 대조. 일치하면 같은 방식으로 Task 1 신규 2종의 해시도 계산해 사용. 불일치면 JCS 정규화 후 재계산(`serde_jcs` 방식, `python3` 대신 기존 해시 생성 스크립트 탐색: `grep -rn "content_hash" scripts/ | head`).
- [ ] **Step 2: contracts 목록에 10종 추가** — `agents/krw-ontology/agent.yaml`의 `contracts:` 끝(`skill-content/v1` 항목 뒤)에 추가. 형식(해시는 Step 1 값):

```yaml
  - { id: krw-filing-search-input/v1, schema_ref: canonical:krw-front/krw-filing-search-input-v1, content_hash: "sha256:<STEP1>", semantic_authority: krw-ontology-front-typescript }
  - { id: krw-filing-search-result/v1, schema_ref: canonical:krw-front/krw-filing-search-result-v1, content_hash: "sha256:<STEP1>", semantic_authority: krw-ontology-front-typescript, max_items: 25 }
  - { id: krw-filing-brief-input/v1, schema_ref: canonical:krw-front/krw-filing-brief-input-v1, content_hash: "sha256:<STEP1>", semantic_authority: krw-ontology-front-typescript }
  - { id: krw-filing-brief-result/v1, schema_ref: canonical:krw-front/krw-filing-brief-result-v1, content_hash: "sha256:<STEP1>", semantic_authority: krw-ontology-front-typescript }
  - { id: krw-feed-list-items-input/v1, schema_ref: canonical:krw-front/krw-feed-list-items-input-v1, content_hash: "sha256:<STEP1>", semantic_authority: krw-ontology-front-typescript }
  - { id: krw-feed-list-items-result/v1, schema_ref: canonical:krw-front/krw-feed-list-items-result-v1, content_hash: "sha256:<STEP1>", semantic_authority: krw-ontology-front-typescript }
  - { id: krw-feed-context-input/v2, schema_ref: canonical:krw-front/krw-feed-context-input-v2, content_hash: "sha256:<STEP1>", semantic_authority: krw-ontology-front-typescript }
  - { id: krw-feed-context/v2, schema_ref: canonical:krw-front/krw-feed-context-v2, content_hash: "sha256:<STEP1>", semantic_authority: krw-ontology-front-typescript }
  - { id: krw-web-news-search-input/v1, schema_ref: canonical:krw-agent/krw-web-news-search-input-v1, content_hash: "sha256:<STEP1>", semantic_authority: krw-agent-capability-runtime }
  - { id: krw-web-news-search-result/v1, schema_ref: canonical:krw-agent/krw-web-news-search-result-v1, content_hash: "sha256:<STEP1>", semantic_authority: krw-agent-capability-runtime, max_items: 10 }
```

- [ ] **Step 3: capabilities 5종 추가** — `capabilities:` 목록 끝(`skill.load` 앞)에 추가:

```yaml
  - id: filing.search_events
    execution: { kind: remote, binding_key: search_catalog_filings }
    permission: read
    input_contract: krw-filing-search-input/v1
    output_contracts: [krw-filing-search-result/v1, normalized-capability-result/v1]
    idempotency: canonical_args
    result_ingest: filing_event_search_v1
    research_action:
      kind: targeted
      estimate: { historical_success_lower_ppm: 600000, expected_duplicate_ppm: 100000, failure_risk_upper_ppm: 150000, expected_latency_ms: 1500, expected_tokens: 300, expected_tool_cost_micros: 1500, expected_result_bytes: 131072 }
      conflict_domain: research:filing-events
    scope_binding:
      kind: trusted_ticker_set
      ticker_references:
        - { id: ticker, pointer_pattern: /ticker, value_kind: string }
      require_any_of: [ticker]
    parallel_safe: false
    prerequisites: [ontology.query_context]
  - id: filing.event_brief
    execution: { kind: remote, binding_key: get_filing_brief }
    permission: read
    # filing_event_id must be one observed in a prior filing.search_events
    # result of this run; the kernel guard rejects unobserved ids (same
    # pattern as ontology.chain object ids).
    input_contract: krw-filing-brief-input/v1
    output_contracts: [krw-filing-brief-result/v1, normalized-capability-result/v1]
    idempotency: canonical_args
    result_ingest: filing_event_brief_v1
    research_action:
      kind: targeted
      estimate: { historical_success_lower_ppm: 550000, expected_duplicate_ppm: 100000, failure_risk_upper_ppm: 150000, expected_latency_ms: 1500, expected_tokens: 300, expected_tool_cost_micros: 1500, expected_result_bytes: 131072 }
      conflict_domain: research:filing-event-brief
    scope_binding:
      kind: trusted_ticker_set
      ticker_references:
        - { id: ticker, pointer_pattern: /ticker, value_kind: string }
      require_any_of: [ticker]
    parallel_safe: false
    prerequisites: [filing.search_events]
  - id: news.feed_list
    execution: { kind: remote, binding_key: list_feed_items }
    permission: read
    input_contract: krw-feed-list-items-input/v1
    output_contracts: [krw-feed-list-items-result/v1, normalized-capability-result/v1]
    idempotency: canonical_args
    result_ingest: feed_issue_list_v1
    research_action:
      kind: context
      estimate: { historical_success_lower_ppm: 550000, expected_duplicate_ppm: 150000, failure_risk_upper_ppm: 150000, expected_latency_ms: 1500, expected_tokens: 300, expected_tool_cost_micros: 1500, expected_result_bytes: 131072 }
      conflict_domain: research:feed-issues
    scope_binding:
      kind: trusted_ticker_set
      ticker_references:
        - { id: tickers, pointer_pattern: /tickers, value_kind: string_array }
      require_any_of: [tickers]
    parallel_safe: false
    prerequisites: [ontology.query_context]
  - id: news.feed_context
    execution: { kind: remote, binding_key: get_feed_context }
    permission: read
    # issue_ids must come from a prior news.feed_list result of this run.
    input_contract: krw-feed-context-input/v2
    output_contracts: [krw-feed-context/v2, normalized-capability-result/v1]
    idempotency: canonical_args
    result_ingest: feed_issue_context_v1
    research_action:
      kind: targeted
      estimate: { historical_success_lower_ppm: 550000, expected_duplicate_ppm: 150000, failure_risk_upper_ppm: 150000, expected_latency_ms: 1500, expected_tokens: 300, expected_tool_cost_micros: 1500, expected_result_bytes: 131072 }
      conflict_domain: research:feed-issue-context
    scope_binding:
      kind: trusted_ticker_set
      ticker_references:
        - { id: tickers, pointer_pattern: /tickers, value_kind: string_array }
      require_any_of: [tickers]
    parallel_safe: false
    prerequisites: [news.feed_list]
  - id: news.web_search
    # Neutral-name, fail-open external headline lookup. The engine brand
    # never appears in prompts or answers; citations name the original
    # publisher only.
    execution: { kind: local, builtin: web_news_search }
    permission: read
    input_contract: krw-web-news-search-input/v1
    output_contracts: [krw-web-news-search-result/v1, normalized-capability-result/v1]
    idempotency: canonical_args
    result_ingest: web_news_v1
    research_action:
      kind: targeted
      estimate: { historical_success_lower_ppm: 400000, expected_duplicate_ppm: 200000, failure_risk_upper_ppm: 400000, expected_latency_ms: 3000, expected_tokens: 250, expected_tool_cost_micros: 1000, expected_result_bytes: 65536 }
      conflict_domain: research:web-news
    scope_binding:
      kind: trusted_ticker_set
      ticker_references:
        - { id: ticker, pointer_pattern: /ticker, value_kind: string }
      require_any_of: [ticker]
    parallel_safe: false
    prerequisites: [ontology.query_context]
```

주의: krw-feed의 `list_feed_items` 입력 스키마 필드명(`/tickers` 배열)과 `get_filing_brief` 입력 필드명을 Step 4 전에 원본에서 재확인한다: `python3 -m json.tool contracts/krw-front/v1/schemas/krw-feed-list-items-input-v1.json` — 포인터가 다르면 그에 맞게 수정.

- [ ] **Step 4: 워크플로 수정(company_research_v2)** — states에 추가(`chain_selected` 뒤):

```yaml
      - { id: event_filing_search, kind: capability, capability_id: filing.search_events, max_visits: 1 }
      - { id: event_filing_brief, kind: capability, capability_id: filing.event_brief, max_visits: 1 }
      - { id: news_feed_lookup, kind: capability, capability_id: news.feed_list, max_visits: 1 }
      - { id: news_feed_context, kind: capability, capability_id: news.feed_context, max_visits: 1 }
      - { id: news_web_search, kind: capability, capability_id: news.web_search, max_visits: 1 }
```

`ingest_evidence`의 `max_visits: 7` → `12`(주석의 "advertised evidence-producing read 전부 수용" 원칙 유지). transitions에 추가(`market_snapshot` 관련 뒤):

```yaml
      - { from: assess_obligations, on: filing_search_has_value, to: event_filing_search }
      - { from: assess_obligations, on: news_lookup_has_value, to: news_feed_lookup }
      - { from: assess_obligations, on: news_web_has_value, to: news_web_search }
      - { from: event_filing_search, on: evidence_observed, to: ingest_evidence }
      - { from: event_filing_search, on: brief_follow_up_has_value, to: event_filing_brief }
      - { from: event_filing_brief, on: evidence_observed, to: ingest_evidence }
      - { from: news_feed_lookup, on: evidence_observed, to: ingest_evidence }
      - { from: news_feed_lookup, on: context_follow_up_has_value, to: news_feed_context }
      - { from: news_feed_context, on: evidence_observed, to: ingest_evidence }
      - { from: news_web_search, on: evidence_observed, to: ingest_evidence }
      - { from: event_filing_search, on: no_evidence_observed, to: news_feed_lookup }
      - { from: news_feed_context, on: no_evidence_observed, to: news_web_search }
```

경로 복귀: 각 신규 상태에서 ingest 후 `assess_obligations` 복귀는 기존 `ingest_evidence on ingested → assess_obligations` 전이가 처리. `no_evidence_observed` 이벤트명은 기존 워크플로 규약 확인 후 정정: `grep -n "no_evidence\|correction_unresolved" agents/krw-ontology/agent.yaml`(capability 상태의 빈 결과 처리 규약이 이미 있으면 그 이름을 사용).
- [ ] **Step 5: validators·어휘 금지** — `action_limits` instructions에 추가:

```yaml
      - { op: max_calls, capability_id: filing.search_events, max: 1, code: filing_search_limit }
      - { op: max_calls, capability_id: filing.event_brief, max: 1, code: filing_brief_limit }
      - { op: max_calls, capability_id: news.feed_list, max: 1, code: feed_list_limit }
      - { op: max_calls, capability_id: news.feed_context, max: 1, code: feed_context_limit }
      - { op: max_calls, capability_id: news.web_search, max: 1, code: web_news_limit }
      - { op: state_precedes, before: event_filing_search, after: event_filing_brief, code: search_before_brief }
      - { op: state_precedes, before: news_feed_lookup, after: news_feed_context, code: list_before_context }
      - { op: state_precedes, before: news_feed_lookup, after: news_web_search, code: ladder_before_web_news }
```

`answer_policy.forbidden_user_terms`에 `FMP`, `Yahoo Finance`, `야후파이낸스` 추가(배열 뒤에 붙인다).
- [ ] **Step 6: 검증** — Run: `bash agents/check-all.sh && PATH=$HOME/.rustup/toolchains/1.97.1-aarch64-apple-darwin/bin:$PATH cargo test -p run-engine`
  Expected: PASS. `evidence_ingest_capacity_covers_each_advertised_research_read` 테스트가 신규 5종을 포함해 통과(max_visits 12가 충분한지 검증). 실패 시 해당 테스트의 계산식에 맞춰 max_visits 상향.
- [ ] **Step 7: Commit** — `git add agents/krw-ontology/agent.yaml agents/check-all.sh && git commit -m "feat(agent): wire filing-event and news fallback capabilities into company research"`

### Task 3: 어댑터 증거 매핑 4종 + 기존 FMP citation 중립화

**Files:**
- Modify: `crates/krw-ontology-adapter/src/lib.rs` (`map_market_snapshot`(~line 2049) 패턴 뒤에 신규 4함수)
- Test: 동일 파일 내 `#[cfg(test)]` 모듈(`map_market_snapshot` 테스트 ~line 3461 패턴)

**Interfaces:**
- Consumes: `MappingContext`, `EvidenceRecord`, `Directness`, `EvidenceGrade`, `PublicCitation`, `AdapterError`(이미 lib.rs에 존재).
- Produces (Task 4가 호출):
  - `pub fn map_filing_event_search(payload: &Value, ticker: &str, context: &MappingContext) -> Result<Vec<EvidenceRecord>, AdapterError>`
  - `pub fn map_filing_event_brief(payload: &Value, ticker: &str, context: &MappingContext) -> Result<EvidenceRecord, AdapterError>`
  - `pub fn map_feed_issue_context(payload: &Value, ticker: &str, context: &MappingContext) -> Result<EvidenceRecord, AdapterError>`
  - `pub fn map_web_news(payload: &Value, ticker: &str, context: &MappingContext) -> Result<Vec<EvidenceRecord>, AdapterError>`

공통 규칙(스펙 §3·§4.3): filing search/brief → `Directness::Direct`, `EvidenceGrade::Strong`, citation title "SEC filing event (8-K)"류(서류유형 포함, 벤더명 없음). feed context·web news → `Directness::Related`, `EvidenceGrade::Medium`, citation title에 원 매체/이슈 제목. 모든 레코드 `ensure_record_bound` 통과, `strong_claim_allowed`는 filing만 true. facts는 문자열 키-값(숫자는 f64), `MAX_MARKET_SNAPSHOT_METRICS`류 상한 재사용 또는 유사 상수(`MAX_FILING_EVENT_FACTS = 16`, `MAX_NEWS_FACTS = 12`).

- [ ] **Step 1: 실패 테스트 작성** — 기존 테스트 모듈에 추가(픽스처는 실측 데이터 형상):

```rust
    #[test]
    fn filing_event_search_maps_lrcx_8k_as_direct_strong_evidence() {
        let payload = serde_json::json!({
            "items": [{
                "filing_event_id": "lrcx-20260827-8k",
                "form_type": "8-K",
                "filing_date": "2026-08-27",
                "sec_items": ["5.02"],
                "event_tag": "leadership_or_board_change",
                "title": "Director departures"
            }]
        });
        let records = map_filing_event_search(&payload, "LRCX", &context("filing.search_events")).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].directness, Directness::Direct);
        assert_eq!(records[0].grade, EvidenceGrade::Strong);
        assert!(records[0].strong_claim_allowed);
        let cited = format!("{:?}", records[0].citation.title);
        assert!(cited.contains("8-K"));
        assert!(!cited.contains("FMP") && !cited.contains("Yahoo"));
    }
```

동일 구조로 3개 추가: `filing_brief_maps_brief_excerpt_as_direct`, `feed_context_maps_confirmed_facts_as_related_medium`(픽스처: title_ko "엔비디아 실적 호조에도 메모리 주식 동반 하락", confirmed_facts 배열), `web_news_maps_publisher_items_as_related_medium`(픽스처: publisher "Reuters", headline, published_at) + `web_news_citations_never_expose_vendor`(`format!("{:?}", citation)`에 FMP/Yahoo 부재 단언, 현재 FMP 노출 테스트와 구분).
- [ ] **Step 2: 실패 확인** — Run: `PATH=$HOME/.rustup/toolchains/1.97.1-aarch64-apple-darwin/bin:$PATH cargo test -p krw-ontology-adapter map_`
  Expected: FAIL — 신규 4함수 미정의.
- [ ] **Step 3: 구현** — `map_market_snapshot`의 구조(evidence_id=capability+content_hash, EvidenceSource에 context 필드, facts 추출, `ensure_record_bound`)를 그대로 따라 4함수 구현. web_news/feed는 items를 순회하며 레코드화(상한 초과 시 잘라냄, `Err` 아님). filing search도 items 순회 `Vec<EvidenceRecord>`.
- [ ] **Step 4: 통과 확인** — Run: `PATH=$HOME/.rustup/toolchains/1.97.1-aarch64-apple-darwin/bin:$PATH cargo test -p krw-ontology-adapter`
  Expected: PASS 전체.
- [ ] **Step 5: 기존 FMP 문자열 중립화** — Run: `grep -rn "FMP" crates/krw-ontology-adapter/src/lib.rs`
  `"Timestamped FMP research snapshot (advisory only)"` → `"Timestamped market snapshot (advisory only)"`로 교체. 해당 문자열을 단언하는 테스트가 있으면 함께 수정. `grep -rn "Timestamped FMP" crates/`로 잔여 0 확인.
- [ ] **Step 6: Commit** — `git commit -am "feat(adapter): filing-event and news evidence mappings with neutral citations"`

### Task 4: capability-runtime — 원격 디스패치 + web_news_search 빌트인

**Files:**
- Modify: `crates/capability-runtime/src/lib.rs` (입력 검증 디스패치 ~line 1133 `market_snapshot_input_identity` 패턴, 빌트인 실행 경로 — `grep -n "skill_load" crates/capability-runtime/src/lib.rs`로 빌트인 디스패치 위치 확인)
- Modify: `crates/capability-runtime/Cargo.toml` (HTTP 클라이언트 의존 — provider-wire가 쓰는 것과 동일 크레이트 확인: `grep -n "reqwest\|hyper" crates/provider-wire/Cargo.toml`)
- Test: `crates/capability-runtime/src/lib.rs` 테스트 모듈(`invocation(&catalog, "ontology.query", ...)` 패턴)

**Interfaces:**
- Consumes: Task 3의 map 함수 4종, Task 2의 capability 카탈로그(agent.yaml 컴파일 결과), 기존 `map_company_context`/`map_market_snapshot` 디스패치.
- Produces: 실행 가능한 5 capability. `news.web_search` 빌트인은 환경 변수 `KRW_WEB_NEWS_API_KEY`·`KRW_WEB_NEWS_API_BASE`(기본값 없음, 미설정=비활성)만 읽고, 5초 타임아웃, 1회 시도, 실패 시 `items: []`인 정상 결과(fail-open — 오류 결과 자체를 에러로 전파하지 않음).

- [ ] **Step 1: 빌트인 디스패치 위치 파악** — Run: `grep -n "skill_load\|Builtin" crates/capability-runtime/src/lib.rs | head -20`
  `execution.kind == local` 빌트인 실행 지점을 찾아 `web_news_search` 분기 추가 위치를 확정(스킬 로드와 달리 비동기 HTTP 필요 — async fn 내 분기).
- [ ] **Step 2: 실패 테스트(원격 4종 입력 검증)** — 기존 `invocation` 패턴으로:

```rust
    #[test]
    fn filing_search_input_identity_is_closed_to_ticker_and_filters() {
        let catalog = ...; // compile_agent_dir(agents/krw-ontology)
        let runtime = ...;
        let ok = runtime.invoke(&invocation(&catalog, "filing.search_events",
            serde_json::json!({"ticker":"LRCX","form_type":"8-K","limit":10}))).unwrap();
        assert!(ok.result["items"].is_array() || ok.normalized.is_some());
        let err = runtime.invoke(&invocation(&catalog, "filing.search_events",
            serde_json::json!({"ticker":"LRCX","form_type":"10-K"}))).unwrap_err(); // enum 밖
        assert!(format!("{err:?}").contains("invalid"));
    }
```

실제 어설션은 기존 `market_snapshot` 디스패치 테스트(lib.rs ~2814) 형식에 맞춰 조정. feed/web_news도 동일 구조 1개씩.
- [ ] **Step 3: 원격 4종 디스패치 구현** — 입력 identity 검증(스키마 기반) 후 MCP 호출 결과를 Task 3 map 함수로 변환해 반환. `filing.event_brief`는 직전 `filing.search_events` 결과에 관찰된 filing_event_id만 허용(런 상태의 관찰 집합 조회 — chain 구현이 참고: `grep -n "observed_object\|chain" crates/capability-runtime/src/lib.rs | head`).
- [ ] **Step 4: web_news_search 빌트인 구현** — FMP `GET {base}/api/v3/stock_news?tickers=<T>&limit=<N>` 호출(키는 헤더). 응답 항목을 `{headline: title, publisher: publisher|site, published_at: publishedDate, url: link, summary: text 앞 1000자}`로 매핑. 벤더 문자열이 로그에도 안 나가게: 로그에는 capability id만. 실패(타임아웃·HTTP 오류·파싱 오류)는 `{"items":[]}` 반환. 환경변수 미설정 시에도 `{"items":[]}`.
- [ ] **Step 5: 빌트인 테스트** — HTTP를 쏘지 않는 두 케이스: ① 미설정 환경 → `items: []`, ② 모킹 서버(기존 테스트의 로컬 리스너 패턴 있으면 재사용, 없으면 `tokio::spawn` + `std::net::TcpListener`로 최소 HTTPS 아님 로컬 엔드포인트 — `KRW_WEB_NEWS_API_BASE`를 http://127.0.0.1:포인트로 지정)에서 고정 JSON → 5항 매핑 단언.
- [ ] **Step 6: 실행** — Run: `PATH=$HOME/.rustup/toolchains/1.97.1-aarch64-apple-darwin/bin:$PATH cargo test -p capability-runtime`
  Expected: PASS 전체.
- [ ] **Step 7: Commit** — `git commit -am "feat(capability-runtime): filing/news dispatch and fail-open web news builtin"`

### Task 5: 배포 바인딩 + 엔드포인트 레지스트리

**Files:**
- Modify: `deployments/local/deployment-binding.krw-ontology.example.yaml`, `deployments/prod/deployment-binding.krw-ontology.example.yaml`
- Modify: `.local/agent-gateway/endpoint-registry.yaml`(로컬 검증용 — dev-stack이 재생성할 수 있으므로 재생성 스크립트 확인: `grep -n "endpoint-registry" scripts/dev-stack.sh`)
- Test: dev-stack 부팅(Task 8에서 최종, 여기선 구문 검증)

**Interfaces:**
- Consumes: Task 2 capability 정의의 binding_key 4종(search_catalog_filings, get_filing_brief, list_feed_items, get_feed_context). `news.web_search`은 로컬 빌트인이라 바인딩 불필요.
- Produces: 배포 파일에 4 바인딩. 값은 `deployments/local/deployment-binding.example.yaml`의 해당 항목과 동일(endpoint_ref krw-filings-local / krw-feed-local, credential_ref KRW_FILINGS_MCP_TOKEN / KRW_FEED_MCP_TOKEN, run-scoped, 서버 스키마 번들 해시·타임아웃 복사).

- [ ] **Step 1: 바인딩 4개 추가** — 두 example 파일의 `capabilities:` 끝에, 전체 바인딩 예시 파일(deployment-binding.example.yaml line 103~135·136~168)에서 해당 항목을 그대로 복사해 붙인다(해시·build 문자열 포함 — 동일 서버를 가리키므로 값이 같아야 함).
- [ ] **Step 2: 로컬 엔드포인트 확인** — 프론트 dev 스택이 filings/feed MCP URL을 노출하는지: `grep -rn "FILINGS_MCP\|FEED_MCP" ~/krw-ontology-front/.env* 2>/dev/null | sed 's/=.*/=<set>/'`(변수명만 확인). 노출되면 `.local/agent-gateway/endpoint-registry.yaml`에 두 엔드포인트 추가(url_env/ready_env/env 변수명은 front dev 문서 참조). 아니면 Task 8은 fixtures 리플레이로 진행(스펙 §8 허용).
- [ ] **Step 3: YAML 구문 검증** — Run: `python3 -c "import yaml,sys; [yaml.safe_load_all(open(f)) for f in ['deployments/local/deployment-binding.krw-ontology.example.yaml','deployments/prod/deployment-binding.krw-ontology.example.yaml']]" && echo OK`
- [ ] **Step 4: Commit** — `git add deployments/ .local/ 2>/dev/null; git commit -m "feat(deploy): bind filings and feed endpoints to krw-ontology deployment"`(`.local/`이 gitignore면 제외).

### Task 6: 프롬프트 3곳 + 어휘·체크리스트(미공시→추정 수정과 동반 커밋)

**Files:**
- Modify: `agents/krw-ontology/prompts/evidence-analyst.md`
- Modify: `agents/krw-ontology/references/research-synthesis.md`
- Modify: `agents/krw-ontology/references/final-markdown-contract.md`

**Interfaces:**
- Consumes: Task 2 capability id 5종(프롬프트는 도구를 `filing event search` / `news feed` / `web news lookup`으로 지칭).
- Produces: 검증 기준(Task 8 평가 항목): 사건형 질문에서 사다리 조회 후에만 '미확인', 8-K 인용 형식, "보도 기준(공시 미확인)" 라벨.

- [ ] **Step 1: evidence-analyst.md 트리거 추가** — 기존 미공시 트리거 문단 뒤에 추가(파일의 영어 문체 유지):

```markdown
When the question's premise names a specific corporate event or announcement —
a departure or appointment, a transaction, a guidance change, a market-wide
move after results — the filing catalog and news feed come before any
non-confirmation. Run the filing event search for the run's ticker first: an
8-K (or 6-K) whose items or event tag match the premise is the direct,
strong-grade evidence that the event exists, and its form type, filing date,
and item number belong in the ledger beside the claim it confirms. If the
catalog returns nothing, list the feed issues for the ticker and pull the
context of a matching issue: its confirmed facts are related-grade evidence
and must ride beside the claim as "reported" ("보도에 따르면"), never as
disclosed. Only when both come back empty may the answer state
non-confirmation, scoped to what was checked. The web news lookup is the last
rung: one call, publisher-only citations, and its absence or failure changes
nothing about the answer's structure.
```

- [ ] **Step 2: research-synthesis.md 인용 형식 추가** — "Prefer an evidence-grounded estimate over silence" 불릿 뒤에:

```markdown
Cite confirmed events by their filing identity — "8-K(2026-08-27 접수,
Item 5.02)" — with the substance the filing states, and build the investment
view on it as you would on any direct evidence. Evidence that comes from
reporting rather than disclosure keeps a visible label: "등록 매체 보도
기준(공시 미확인)" for feed issues and "외부 보도 기준(공시 미확인)" for web
lookup results, with the original publisher named and the observation that
would confirm it in filings kept as the natural follow-up. Reporting can
establish that the market moved; it never upgrades itself into disclosed
fact.
```

- [ ] **Step 3: final-markdown-contract.md 체크리스트 추가** — 22번 뒤 23번으로:

```markdown
23. When the question's premise is a specific event or market reaction, did
the answer check the ladder before saying it cannot confirm — and when an
event is confirmed, is it cited by filing identity (form type, filing date,
item), with reporting-based evidence labeled "보도 기준(공시 미확인)" and no
vendor or infrastructure name (no engine brands) anywhere in the answer?
```

- [ ] **Step 4: 검증** — Run: `bash agents/check-all.sh`
  Expected: PASS(semantic-markers 검증 포함 — 마커 추가 필요하면 스크립트 안내에 따라 `agents/fixtures/semantic-markers.tsv`에 행 추가).
- [ ] **Step 5: Commit(동반)** — `git add agents/krw-ontology/ && git commit -m "feat(prompts): event-premise ladder duty, filing identity citations, vendor-neutral reporting labels"` — 이 커밋은 작업 트리에 있던 미공시→추정 3파일 수정을 함께 탑재(스펙 §9 승인됨). 커밋 전 `git diff --stat agents/krw-ontology/prompts/evidence-analyst.md agents/krw-ontology/references/`로 동반 변경 범위 재확인.

### Task 7: GLM-5.3-flash 테스트 모델 교체

**Files:**
- Modify: `crates/protocol/src/lib.rs:19` (`GLM_MODEL_ID: &str = "glm-5.3"` → `"glm-5.3-flash"`)
- Modify: `deployments/local/model-registry.glm.yaml`, `deployments/prod/model-registry.glm.yaml` (model_id 3곳 + max_context/max_output 유지: 204800/131072 — flash 스펙은 1M이지만 커널 예산 산수가 현재 캡 기준이므로 유지)
- Modify: `crates/provider-wire/src/sse.rs`(~549, ~576), `crates/provider-wire/src/assembler.rs`(~530, ~751)의 "glm-5.3" 픽스처 문자열 → "glm-5.3-flash"
- Test: `cargo test -p protocol -p provider-wire -p runtime-config`

**Interfaces:**
- Produces: 모든 GLM 프로파일(glm_high/glm_max/glm_direct)이 glm-5.3-flash를 가리킴. prod 배포 시 이 파일이 DeepSeek와 함께 배포되지만 배포 프로필은 DeepSeek 레지스트리를 쓰므로 프로덕션 답변 경로 불변(배포 설정 확인: `deployments/prod/deployment-binding.*` 및 배포 설정의 provider).

- [ ] **Step 1: 실패 확인 아님(상수 교체는 즉시 테스트로 검증)** — 교체 후 Run: `PATH=$HOME/.rustup/toolchains/1.97.1-aarch64-apple-darwin/bin:$PATH cargo test -p protocol -p provider-wire -p runtime-config`
  Expected: FAIL 가능성 — `validate_model_profile`·픽스처 문자열 비교가 실패하면 해당 테스트의 기대값을 함께 갱신(테스트가 상수를 참조하면 자동 통과).
- [ ] **Step 2: 능력 플래그 조건 검토** — 레지스트리의 `provider_wire_capabilities`(thinking/tools/tool_choice)는 일단 현행 값 유지. Task 8의 1케이스 프로브에서 도구 호출 실패·thinking 오류가 나면 `supports_*` 플래그를 조정해 재검증(프로브 결과를 커밋 메시지에 기록).
- [ ] **Step 3: Commit** — `git commit -am "feat(provider): switch GLM test model to glm-5.3-flash"`

### Task 8: 품질 매트릭스 코퍼스 + GLM-5.3-flash 검증 실행

**Files:**
- Create: `/tmp/q-fallback/corpus.json`(러너 스키마: `{schema_version:1, suite_id, suite_version, cases:[{case_id, ticker, question, evaluation_focus}]}`)
- Test: 실제 매트릭스 실행 결과(`--report-dir` 사전 존재 금지)

**Interfaces:**
- Consumes: Task 1~7 전체(작동하는 dev-stack). dev-stack 재적재: `bash scripts/dev-stack.sh reload`(작업 트리 이미지 재봉인 — stale-pid·lockf exit-75 재시도 주의).

- [ ] **Step 1: 코퍼스 작성(총 50케이스 — 사용자 지시 2026-08-28)** — 계층 구성: ①사건형·카탈로그 계층 14 ②시장반응형·피드 뉴스 계층 12 ③웹 뉴스 계층 6 ④부정·정직 스코핑 4 ⑤메트릭 통제(사다리 미호출) 6 ⑥스타일 회귀(실사용자 문체) 8 = 50.

고정 앵커 5케이스(①lrcx_event_8k, ②nvda_market_reaction, ④ctas_proxy, ⑤ctrl_metric_no_ladder, ④ctrl_undisclosed_estimate):

```json
 {"case_id":"lrcx_event_8k","ticker":"LRCX","question":"LRCX 람리서치, 이사 두 명 사임 발표의 투자 관점과 근거를 확인해줘","evaluation_focus":["8-K(접수일·Item 5.02) 인용","사건 확정 서술","전이 경로와 제 판단","프로세스 누출 없음"]},
 {"case_id":"nvda_market_reaction","ticker":"NVDA","question":"NVDA 엔비디아 실적 호조에도 메모리 주식 동반 하락의 투자 관점과 근거를 확인해줘","evaluation_focus":["보도 기준 전제 확인(마이크론 등 하락 폭)","공시 근거 실적 분석","보도 기준(공시 미확인) 라벨","첫 문장 볼드·후속 3개"]},
 {"case_id":"ctas_proxy","ticker":"CTAS","question":"CTAS 신탁사 이사 Melanie W. Barstad, 2026년 주주총회 재선거 불참의 투자 관점과 근거를 확인해줘","evaluation_focus":["위임장 명세서 미제출 정직 서술","사건 위양 금지","방향성 판독 유지"]},
 {"case_id":"ctrl_metric_no_ladder","ticker":"MSFT","question":"MSFT 최근 분기 매출과 영업이익 근원을 설명해줘","evaluation_focus":["사다리 도구 미호출(일반 메트릭)","공시 근거 숫자","스타일 불변량"]},
 {"case_id":"ctrl_undisclosed_estimate","ticker":"MLM","question":"MLM 인수한 사업의 마진 기여는?","evaluation_focus":["미공시→추정 계약 유지(제 추정 라벨)","사다리 남용 없음"]}
```

나머지 45케이스 그라운딩(실제 데이터 기반 — 픽션 금지):
- ① 잔여 13: 프로덕션 `sec_filing_events`에서 최근(30일) 8-K/6-K를 `event_tag` 다양화하여 ≥10개 상이 티커로 선택(쿼리: form_type, filing_date, sec_items, event_tag, title; psql 레시피는 컨트롤러 제공). 질문은 이벤트 서술 + "의 투자 관점과 근거를 확인해줘".
- ② 잔여 11: `market_issues` + `market_issue_entities`에서 최근 이슈를 ≥8개 상이 티커로 선택(title_ko, summary_ko, confirmed_facts 활용). 기대: "보도 기준(공시 미확인)" 라벨.
- ③ 6: 최근 7일 내 시장 반응 질문이되 카탈로그·피드에 해당 이슈가 없는 티커(조회로 확인) — 웹 뉴스 계층 기대, 키 미설정이면 정직 스코핑도 합격으로 판정.
- ④ 잔여 2: 카탈로그·피드 어디에도 없는 사건형 질문 2종(미공시 사안 — 정직 스코핑 기대).
- ⑤ 잔여 5: 순수 메트릭 질문 5종(티커 상이). 기대: 사다리 미호출.
- ⑥ 8: `/tmp/q50/prod-user-50-corpus.json`이 살아 있으면 그중 8개 선택(`python3 -c "import json; c=json.load(open('/tmp/q50/prod-user-50-corpus.json'))['cases']; print(len(c))"`), 없으면 Supabase messages 레시피로 실사용자 질문 8개 재추출. 기대: 기존 불변량 유지.

`suite_id`는 `filing-news-fallback-50`, `suite_version` `1.0.0`. 모든 질문은 실사용자 문체(한국어). evaluation_focus는 계층 기대를 명시.
- [ ] **Step 2: dev-stack 재적재 + 봉인 확인** — Run: `bash scripts/dev-stack.sh reload` 후 `grep -r "filing event search first\|등록 매체 보도" .local/agent-gateway/cache/*/images/krw-ontology/blobs 2>/dev/null | head -3`
  Expected: 신규 프롬프트 구문이 봉인 이미지에서 발견됨.
- [ ] **Step 3: 매트릭스 실행(플러브 역할 겸, 50케이스 — 디태치드 실행)** — 50케이스 × 병렬 2는 런타임 수 시간이므로 백그라운드로 띄우고 완료를 폴링:

```bash
set -a; source ~/krw-agnet/.local/agent-gateway/frontend.env; set +a
source ~/krw-agnet/.local/agent-gateway/secrets.env
mkdir -p /tmp/q-fallback
nohup python3 ~/krw-agnet/scripts/run_live_quality_matrix.py --corpus /tmp/q-fallback/corpus.json --parallelism 2 --provider glm --report-dir /tmp/q-fallback/report-r1 --poll-seconds 10 > /tmp/q-fallback/matrix-r1.log 2>&1 &
echo $! > /tmp/q-fallback/matrix-r1.pid
```

시작 확인: 60초 내 `ls /tmp/q-fallback/report-r1/cases/`에 첫 case 디렉터리 생성 + 로그에 오류 없음. 이후 컨트롤러가 `find /tmp/q-fallback/report-r1/cases -name result.json | wc -l`로 완료 수 추적(50 도달 시 판정 개시). transport_failed 재시도는 단독(--case)으로 사다리 복구.
- [ ] **Step 4: 판정** — `<report>/cases/<id>/result.json`의 `answer_markdown`으로: ①계층 14케이스는 8-K/6-K(서류유형·접수일·Item) 인용 + 사건 확정 서술. ②계층 12케이스는 보도 기반 사실 서술 + "보도" 라벨. ③계층 6케이스는 원 매체명 인용 또는 정직 스코핑(키 미설정 시). ④계층 4케이스는 미확인 정직 서술 + 사건 위양 금지. ⑤계층 6케이스는 카탈로그/뉴스 인용 없이 공시 숫자. ⑥계층 8케이스는 기존 불변량(첫 볼드·후속 3·라벨·프로세스 누출 0). 전역: FMP/야후 문자열 0(`grep -ri "fmp\|yahoo\|야후" report-dir` → 0), lrcx_event_8k·nvda_market_reaction·ctas_proxy는 앵커 기대 그대로.
- [ ] **Step 5: flash 실패 분리 절차** — 스타일 위반 케이스가 나오면 `--case <id>` 재실행 후에도 동일 위반일 때, model-registry를 glm-5.3(풀)로 일시 복원해 동일 코퍼스 재실행 → 풀에서 통과하면 모델 효과(플래그 조정·Task 7 Step 2), 풀에서도 실패하면 프롬프트 결함(수정 후 재검증).
- [ ] **Step 6: 보고서 저장** — 평가서 `/tmp/q-fallback/_evaluation.md` 작성(집계 표 + 판정 근거 인용). 커밋 대상 아님(임시 산출물).

### Task 9: 전체 회귀 + 마무리(배포 게이트)

**Files:** 없음(검증·커밋만)

- [ ] **Step 1: 전체 테스트** — Run: `PATH=$HOME/.rustup/toolchains/1.97.1-aarch64-apple-darwin/bin:$PATH cargo test --workspace`
  Expected: PASS 전체. 실패는 태스크별 수정 후 재실행.
- [ ] **Step 2: 에이전트 전체 검증** — Run: `bash agents/check-all.sh`
  Expected: PASS.
- [ ] **Step 3: 스펙·계획 문서 커밋** — `git add docs/superpowers/ && git commit -m "docs: filing-event catalog and news fallback design spec + plan"`
- [ ] **Step 4: 배포 게이트** — 사용자 승인 후: `PATH=$HOME/.rustup/toolchains/1.97.1-aarch64-apple-darwin/bin:$PATH cargo run -q -p krw-agent-deploy -- --config ~/krw-agnet-prod/config/production-config-deepseek.json deploy` → 봉인 확인(신규 바인딩 4종 프롬프트·바인딩 해시) → 프로덕션에서 LRCX 질문 재현 E2E(Supabase 세션 확인). 승인 전 대기.

## Self-Review 기록

- 스펙 커버리지: §3 사다리(Task 2·3·4), §4.1~4.5(Task 5·2·4·3·6), §5 흐름(Task 8 판정), §6 오류(Task 4 Step 4 fail-open), §7 결정(Task 1·4 중립 이름), §8 모델·테스트(Task 7·8), §9 출시(Task 9). §4.1의 "krw_web_news_search 바인딩"은 구현 중 로컬 빌트인으로 확정되어 Task 4에 반영(스펙이 남긴 미확정 1건 해소).
- 플레이스홀더: `context("...")` 헬퍼 등 기존 테스트 유틸 재사용 표기는 실제 패턴 참조를 지시. 해시 `<STEP1>`은 Step 1 계산 절차가 값을 확정.
- 타입 정합: capability id·상태 id·전이 이벤트명이 Task 2↔4↔6에서 동일. map 함수 시그니처 Task 3↔4 일치.
- 알려진 리스크: (a) 전이 이벤트 어휘가 그래프 데이터가 아닌 엔진 상수에 하드코딩되어 있으면 Task 2 Step 6에서 run-engine 테스트 실패로 노출 → 해당 상수 추가로 해결.(b) 빈 결과 전이(`no_evidence_observed`) 이름은 Step 4의 grep으로 기존 규약에 맞춤.(c) 로컬 dev에서 filings/feed 엔드포인트 미노출 시 Task 8은 카탈로그/피드 없이 실행되어 lrcx/nvda 케이스가 웹 뉴스 단계로만 검증됨 — 이 경우 프로덕션 배포 후 E2E(Task 9 Step 4)가 1·2단의 최종 검증이 됨.
