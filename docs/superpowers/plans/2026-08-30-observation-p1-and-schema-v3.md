# 관측 계층 P1 + 스키마 v3 정리 번들 — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 샌드박스(~/krw-ontology-v2)에서 ① 품질 무손실 스키마 정리(감사 확정본)와 ② 관측 계층 P1(시세·배수·FRED+빈티지 → observations.sqlite → 엔진 도구 2종 → 차트 합류)를 한 번의 샤드 스키마 v2→v3 재빌드로 동반 구현한다.

**Architecture:** 두 트랙. Track A는 krw-ontology의 죽은 코드/필드/테이블 제거 + SupportLink 파생 테이블 강등 + 텍스트 3중 저장 해소(external-content FTS)로, 읽히는 서빙 경로는 불변. Track B는 krw-ontology에 관측 수집·저장 계층(프로바이더 포트 + observations.sqlite v1)을 만들고, capability runtime(krw-agnet services, 임포트 스크립트로 krw-ontology 서브셋을 개명해 사용)에 고정 라우터 도구 2종을 추가한 뒤 엔진 계약·어댑터·바인딩까지 잇는다.

**Tech Stack:** Python 3.12(uv) / SQLite(FTS5 external-content) / Rust workspace(cargo) / MCP streamable-http / FMP·FRED REST(+openbb-core 포트 옵션)

**스펙:** `docs/superpowers/specs/2026-08-30-openbb-observation-chain-design.md` (이 plan은 그 P1+정리의 구현 계획)

## Global Constraints

- 작업 폴더: `~/krw-ontology-v2/{krw-ontology,krw-agnet}`, 브랜치 `feat/observation-v3-bundle`. **원본 저장소·`~/krw-ontology-data/releases/prod` 절대 수정 금지.**
- v2 빌드 출력: `~/krw-ontology-data/releases/v2-dev` env (prod `current` 심링크 분리).
- 근거 교리: 관측 데이터는 `advisory_only`, `strong_claim_allowed: false`, 추천·가격목표 근거 금지(불변).
- 밴더 중립: FMP/FRED/Polygon 명칭은 답변·도구 결과에서 스크럽(`forbidden_user_terms` 유지), 인용은 중립 명칭.
- 버전 규율: 스키마 변경 = 명시적 버전 범프. 샤드 `krw-ontology-company-shard/v2→v3`, `SPINE_PROJECTION_VERSION v6→v7`. 스파인 스키마(v4)·색인 불변.
- P1 정책: `VALUATION_STOP` 불변(완화는 P2). `TrustedMacroContext` 없음(P2).
- 테스트: 태스크마다 녹색. 네트워크 호출 금지(픽스처만). Rust는 `cargo test -p <crate>`.
- 커밋: 태스크 종료 시마다. 원자적, 메시지 prefix `feat|fix|refactor|test|chore`.

---

### Task A1: 죽은 추출 스테이지·필드 제거 (krw-ontology)

**Files:**
- Delete: `src/krw_ontology/pipeline/stages/extract_risks_drivers_headwinds.py`
- Delete: `tests/unit/test_extract_research_objects.py`
- Modify: `src/krw_ontology/schema/objects.py:235,262,315,715` (ranking_score 4곳)
- Modify: `src/krw_ontology/pipeline/stages/extract_external_factor_exposures.py:392`
- Test: `tests/unit/test_schema_objects.py` (기존 파일에 케이스 추가)

**Interfaces:**
- Consumes: 없음(첫 태스크)
- Produces: `ranking_score` 필드 소멸 — 이후 태스크는 이 필드를 참조하지 않음(감사로 서빙 읽기 0 확인됨)

- [ ] **Step 1: 실패 테스트 작성** — `tests/unit/test_schema_objects.py`에 추가:

```python
def test_ranking_score_field_is_removed_from_all_models():
    from krw_ontology.schema import objects as schema_objects

    source = inspect.getsource(schema_objects)
    assert "ranking_score" not in source, (
        "ranking_score is write-only at serving; ranking uses "
        "specificity_score/boilerplate_score/materiality_hint (audit 2026-08-30)"
    )


def test_retired_risk_driver_headwind_stage_is_absent():
    stage_path = (
        Path(__file__).parents[2]
        / "src"
        / "krw_ontology"
        / "pipeline"
        / "stages"
        / "extract_risks_drivers_headwinds.py"
    )
    assert not stage_path.exists()
    from krw_ontology.pipeline.orchestrator import PIPELINE_STAGES

    assert "extract_risks_drivers_headwinds" not in PIPELINE_STAGES
```

- [ ] **Step 2: 실패 확인** — `uv run pytest tests/unit/test_schema_objects.py -k "ranking_score or retired_risk" -v` → FAIL
- [ ] **Step 3: 구현** — 위 Delete/Modify 4파일 반영. `extract_external_factor_exposures.py`의 `exposure["ranking_score"] = round(ranking, 3)` 라인과 `ranking` 계산식(해당 라인 위 3~5줄) 제거. 모델 4곳의 `ranking_score: float | None = None` 제거. `import inspect`/`from pathlib import Path`가 테스트 파일에 없으면 추가.
- [ ] **Step 4: 통과 확인** — Step 2 명령 재실행 → PASS. 이후 `uv run pytest tests/unit -x -q` 전체 녹색 확인(ranking_score를 쓰는 다른 테스트가 있으면 그 테스트도 이 단계에서 함께 수정 — 감사상 존재하지 않음).
- [ ] **Step 5: 커밋** — `git add -A && git commit -m "refactor: remove dead risks-drivers-headwinds stage and write-only ranking_score"`

### Task A2: 스키마 단일 소스화 (krw-ontology)

**Files:**
- Delete: `ontology/schema/objects.yaml` (런타임 로더 없음, v0.1.0 드리프트)
- Modify: `src/krw_ontology/cli/init_workspace.py:106-111,277-291,313-366` (낡은 objects.yaml 템플릿 발행 제거)
- Modify: `src/krw_ontology/pipeline/stages/validate_ontology.py` (교차검증 추가)
- Test: `tests/unit/test_registry_contract.py` (신규)

**Interfaces:**
- Consumes: `registry.py load_ontology_registry()`, `builder.py OBJECT_FILE_KEYS`(builder.py:233)
- Produces: `assert_registry_matches_object_file_keys()` — A3/A4가 artifact 키 집합을 바꿀 때 이 검증이 기준

- [ ] **Step 1: 실패 테스트** — `tests/unit/test_registry_contract.py` 신규:

```python
"""registry.yaml is the only artifact-file authority; builder keys must match."""
from krw_ontology.agent_index.builder import OBJECT_FILE_KEYS
from krw_ontology.ontology_registry import load_ontology_registry  # 실제 모듈경로는 registry.py 로드부와 일치시킬 것


def test_registry_artifact_files_match_builder_object_file_keys():
    registry = load_ontology_registry()
    registry_files = set(registry.artifact_files())  # 접근자가 다르면 registry 구조에 맞게 조정
    assert registry_files == set(OBJECT_FILE_KEYS), (
        f"drift: only-registry={registry_files - set(OBJECT_FILE_KEYS)} "
        f"only-builder={set(OBJECT_FILE_KEYS) - registry_files}"
    )


def test_objects_yaml_is_not_shipped():
    assert not (REPO_ROOT / "ontology" / "schema" / "objects.yaml").exists()
```

(실행 시 `load_ontology_registry`의 실제 임포트 경로·접근자를 `src/krw_ontology/pipeline/stages/build_governance.py:30` 인접 코드에서 확인해 맞춘다. REPO_ROOT는 기존 테스트의 경로 관례 따름.)

- [ ] **Step 2: 실패 확인** — `uv run pytest tests/unit/test_registry_contract.py -v` → FAIL
- [ ] **Step 3: 구현** — ① `ontology/schema/objects.yaml` 삭제 ② `init_workspace.py`에서 objects.yaml 템플릿 문자열/발행 블록 제거(해당 워크스페이스에 registry.yaml 사본만 남김) ③ registry 로더에 `artifact_files()` 접근자 없으면 추가(`registry.yaml`의 28 `artifact_file` 값 집합 반환).
- [ ] **Step 4: 통과 확인** — Step 2 재실행 PASS + `uv run pytest tests/unit -x -q` 녹색
- [ ] **Step 5: 커밋** — `refactor: make registry.yaml the single artifact authority, drop unread objects.yaml`

### Task A3: SupportLink 객체 → 샤드 파생 테이블 강등 (krw-ontology, v3 범프 1차)

**사실 근거(감사 2026-08-30):** 서빙은 claim의 `supported_by_quotes` 내장 필드로 조인(store.py:4837-4906). `retriever/research_kernel/discovery/spine_router`에 SupportLink 문자열 0건. SupportLink는 18.39M 객체의 51%를 차지하는 결정론적 파생물. JSONL 아티팩트(재결정성·감사)는 유지, 샤드 `objects` 테이블·텍스트 사본·스파인 locator 행에서만 제외.

**Files:**
- Modify: `src/krw_ontology/agent_index/builder.py` — ① `_create_schema`에 `support_links` 테이블 DDL 추가 ② SupportLink 로드 경로를 objects 삽입에서 `support_links` 삽입으로 전환(`support_links` 아티팩트 키는 OBJECT_FILE_KEYS에 유지하되objects 적재 제외) ③ `COMPANY_SHARD_SCHEMA_VERSION = "krw-ontology-company-shard/v3"` 범프(builder.py 상수) ④ `SPINE_PROJECTION_VERSION` v6→v7
- Modify: `src/krw_ontology/agent_index/store.py:577` 영역 — SupportLink 조회를 `support_links` 테이블 폴백으로
- Modify: `src/krw_ontology/mcp_server/tools.py:596-676` — TRACE_ONLY/alias는 유지(trace가 파생 테이블에서 해석되도록 store 조회만 교체)
- Test: `tests/unit/test_shard_schema_v3.py` (신규)

**Interfaces:**
- Produces: 샤드 테이블 `support_links(object_id PK, ticker, from_id, to_id, support_type, support_role, stance, support_strength, inference_level, requires_inference, evidence_grade, evidence_strength, json)` — A4/A5·store 조회가 의존

- [ ] **Step 1: 실패 테스트** — `tests/unit/test_shard_schema_v3.py`:

```python
def test_support_links_live_in_derived_table_not_objects(tmp_path):
    shard = build_minimal_shard_with_one_claim_and_quote(tmp_path)  # 기존 빌더 테스트 픽스처 재사용(아래 Step 3에서 최소 픽스처 작성)
    with sqlite3.connect(f"file:{shard}?mode=ro", uri=True) as conn:
        object_rows = conn.execute(
            "SELECT COUNT(*) FROM objects WHERE type='SupportLink'"
        ).fetchone()[0]
        link_rows = conn.execute("SELECT COUNT(*) FROM support_links").fetchone()[0]
    assert object_rows == 0
    assert link_rows >= 1
    # trace still resolves a support link id
    store = open_store(shard)
    traced = store.trace("support_link:SO:CY2023:10K:direct_quote_support:36ff437050")
    assert traced is not None
```

(픽스처는 기존 `tests/unit` 내 샤드 빌드 헬퍼를 재사용한다 — builder 테스트에 이미 최소 청구+인용구 픽스처가 있음. 없으면 claim 1·quote 1·support_link 1 JSONL을 직접 만들어 빌드.)

- [ ] **Step 2: 실패 확인** → FAIL(테이블 부재)
- [ ] **Step 3: 구현** — 위 DDL로 `_create_schema` 확장, SupportLink 적재 분기, store 조회 전환, 두 버전 상수 범프. `verify_agent_index`의 테이블 허용집합에 `support_links` 추가(버리파이어가 미세조정 대상).
- [ ] **Step 4: 통과 확인** — 신규 테스트 PASS + `uv run pytest tests/unit -x -q` 전체(기존 SupportLink 관련 테스트가 objects 경로를 가정하면 이 단계에서 파생 테이블 기준으로 수정)
- [ ] **Step 5: 커밋** — `refactor: demote SupportLink to derived shard table (schema v3), 51% object reduction`

### Task A4: 텍스트 3중 저장 해소 — object_text 삭제 + external-content FTS (krw-ontology, v3 확정)

**사실 근거:** `object_text`와 `object_search_text` DDL이 완전 동일(builder.py:4179-4205). `object_text` 읽는 코드 0건. `object_fts`가 content= 없는 FTS5(11컬럼 전체 사본, SO 샤드에서 363MB).

**Files:**
- Modify: `src/krw_ontology/agent_index/builder.py:4149-4205` — ① `object_text` DDL/DROP/insert(≈:5252)/BASE_FRAGMENT_TABLES(:271)/(:452) 제거 ② `object_fts`를 `content='object_search_text'` external-content로 변경 ③ 적재 후 `INSERT INTO object_fts(object_fts) VALUES('rebuild')` 호출(불변 빌드라 트리거 불필요)
- Modify: 검증기 — 테이블 집합에서 `object_text` 제거, object_fts contentless/external 검증
- Test: `tests/unit/test_shard_schema_v3.py`에 케이스 추가

**Interfaces:**
- Produces: 스키마 v3 최종형 — A5 재빌드가 소비. FTS 쿼리(`_fts_query`의 `term*`)는 변경 없음(external-content도 동일 MATCH 동작)

- [ ] **Step 1: 실패 테스트 추가**:

```python
def test_object_text_table_is_gone_and_fts_is_external_content(tmp_path):
    shard = build_minimal_shard_with_one_claim_and_quote(tmp_path)
    with sqlite3.connect(f"file:{shard}?mode=ro", uri=True) as conn:
        tables = {r[0] for r in conn.execute("SELECT name FROM sqlite_master WHERE type='table'")}
        sql = conn.execute("SELECT sql FROM sqlite_master WHERE name='object_fts'").fetchone()[0]
    assert "object_text" not in tables
    assert "content='object_search_text'" in sql
    # retrieval still works through FTS
    store = open_store(shard)
    hits = store.fts_search("guidance")  # 픽스처 단어에 맞게 조정
    assert hits  # FTS 결과 유지 = 품질 불변 확인
```

- [ ] **Step 2: 실패 확인** → FAIL
- [ ] **Step 3: 구현** — 위 변경. `object_search_text`의 6텍스트 컬럼은 유지(external-content의 원본). `SHARD_FTS_TABLES`(builder.py:465) 불변.
- [ ] **Step 4: 통과 확인** — 신규 PASS + `uv run pytest tests/unit -x -q` 녹색
- [ ] **Step 5: 커밋** — `refactor: drop object_text, external-content FTS — text stored once (schema v3)`

### Task A5: v2-dev 전체 재빌드 + 품질 패리티 게이트 (krw-ontology)

**Files:**
- Create: `scripts/rebuild_v2_dev.sh` (빌드 런처 — ops/deploy-with-local-release.sh 관행 참조)
- Create: `tests/quality/parity_report_v3.md` (산출물 기록)

**Interfaces:**
- Consumes: A3/A4의 v3 스키마, 공유 소스 `~/krw-ontology-data/companies`(읽기 전용)
- Produces: `~/krw-ontology-data/releases/v2-dev/<release_id>/` — Track B가 사용

- [ ] **Step 1: 빌드 런처 작성** — CLI 진입 `uv run krw-ontology ...`(하위명령은 `src/krw_ontology/cli/main.py`의 빌드/릴리스 명령 확인 후 기입: 스파인 빌드→검증→manifest→프로모트 순서, root=`~/krw-ontology-data`, env=`v2-dev`). 타임아웃 여유(전량 재빌드, 15분 폴링 관행)로 실행.
- [ ] **Step 2: 구조 검증** — 릴리스 `verify/` 결과 녹색 + 객체 수 감소 확인: `global_object_count` 18,392,007 → ~9.0M(±10%, SupportLink 9.39M 제외 근사) + `object_count_by_type`에 SupportLink 0.
- [ ] **Step 3: 품질 패리티 게이트(핵심)** — ① `uv run pytest tests/unit/test_router_benchmark_gate.py -v` 녹색(router_gold_v2 + planned_gold_v2 — 빌드된 스토어 대상) ② answer gold v1(`benchmarks/agent_sdk_answer_gold_v1.json`) 대상 게이트 녹색 ③ 프로덕션 current vs v2-dev 릴리스로 동일 질의 10개(골든에서 발췌) `krw_ontology_query_context` 실행 → evidence_units의 `object_id·metric 값·directness` 정합(문구 아닌 데이터 패리티) — 불일치 0 허용.
- [ ] **Step 4: 크기 리포트** — 샤드 총량·스파인 크기 비교표를 parity_report_v3.md에 기록(기대: 샤드 218GB→대략 90-120GB, 스파인 47GB→~30GB; 수치는 기록용, 게이트 아님).
- [ ] **Step 5: 통합 커밋** — 런처+리포트 커밋. **게이트 실패 시 Track B 진입 금지 — 원인 규명 후 재빌드.**

### Task B1: 관측 프로바이더 포트 + 어댑터 (krw-ontology)

**Files:**
- Create: `src/krw_ontology/observation/__init__.py`
- Create: `src/krw_ontology/observation/ports.py` — 프로바이더 포트 프로토콜
- Create: `src/krw_ontology/observation/providers/fmp.py` — 시세 이력(`historical-price-full`)·배수 이력, 기존 `market/snapshot.py`(krw-agnet services)의 요청 규율 재사용(타임아웃 1.25s→수집은 10s, 512KB→4MB 상한, 리다이렉트 거부)
- Create: `src/krw_ontology/observation/providers/fred.py` — 시리즈 관측 + ALFRED 빈티지(`fred/series/observations` + `vintage_dates` 파라미터)
- Create: `src/krw_ontology/observation/providers/polygon.py` — 일별 OHLCV(키 있을 때만, `POLYGON_API_KEY` 없으면 미등록)
- Test: `tests/unit/observation/test_providers.py` + `tests/fixtures/observation/{fmp_price.json,fmp_ratios.json,fred_cpi.json,fred_cpi_vintage.json,polygon_daily.json}`

**Interfaces:**
- Produces (B3이 소비):

```python
class ObservationProvider(Protocol):
    provider_name: str  # "fmp" | "fred" | "polygon"
    def fetch_series(self, request: SeriesFetchRequest) -> SeriesFetchResult: ...

@dataclass(frozen=True)
class SeriesFetchRequest:
    series_key: str          # 예: "price_close_usd_daily"
    provider_series_id: str  # 예: "AAPL" (fmp historical) / "CPIAUCSL" (fred)
    ticker: str | None       # 가격·배수 도메인만
    start: str | None        # ISO date
    end: str | None

@dataclass(frozen=True)
class SeriesFetchResult:
    series_key: str
    provider: str
    observations: Sequence[RawObservation]  # phenomenon_time, value, result_time, vintage, provenance
    status: Literal["available", "unavailable"]
```

- [ ] **Step 1: 픽스처 확보** — 각 엔드포인트 1회 라이브 호출로 실응답 녹화(FMP 키는 기존 env, FRED 무키 `api_key=...`는 env `FRED_API_KEY`, Polygon 없으면 스킵 — 어댑터만 미작성). 응답을 fixtures에 저장(키·계정 식별자 마스킹).
- [ ] **Step 2: 실패 테스트** — fixture 로드 → 어댑터 파싱 → RawObservation 필드(특히 FRED 빈티지의 result_time≠phenomenon_time) 검증:

```python
def test_fred_adapter_parses_vintage_observations():
    body = load_fixture("observation/fred_cpi_vintage.json")
    provider = FredProvider(api_key="test", opener=fixture_opener(body))
    result = provider.fetch_series(
        SeriesFetchRequest(series_key="cpi_yoy", provider_series_id="CPIAUCSL", ticker=None, start=None, end=None)
    )
    assert result.status == "available"
    obs = result.observations
    assert {o.vintage for o in obs} >= {"2026-06-01", "2026-07-01"}  # 동일 phenomenon_time의 두 빈티지
    same_period = [o for o in obs if o.phenomenon_time.startswith("2026-05")]
    assert len({o.value for o in same_period}) >= 1  # 개정 전/후 값 공존 허용
```

- [ ] **Step 3: 실패 확인** → FAIL(모듈 부재)
- [ ] **Step 4: 구현** — ports.py + 어댑터 3종. 모든 외부 실패는 `status="unavailable"`(예외 금지, snapshot 교리). 프로바이더 포트 격리로 openbb-core 도입은 폴백 대상일 뿐 필수 아님(스펙 D2).
- [ ] **Step 5: 통과 확인 + 커밋** — `feat: observation provider ports with FMP/FRED(vintage)/Polygon adapters`

### Task B2: 시리즈 시드 + 지표 사전 확장 (krw-ontology)

**Files:**
- Create: `ontology/observation/series_seed.yaml` — 40 FRED 시리즈 + 가격/배수 시리즈 정의(domain, provider, provider_series_id, canonical_metric, unit, frequency, adjustment, factor)
- Modify: `pyproject.toml` force-include에 `ontology/observation/series_seed.yaml` 추가(hatch 관행, taxonomy와 동일)
- Modify: `ontology/schema/metric_dictionary.yaml` — 관측 지표 등록(`last_price`, `trailing_pe_ttm`, `price_to_book_ttm`, `cpi_yoy`, `fed_funds_rate`, `unemployment_rate`, `dgs10_yield`, … 각 display_name/category/unit/aliases)
- Test: `tests/unit/observation/test_series_seed.py`

**Interfaces:**
- Produces: `load_series_seed() -> dict[str, SeriesDefinition]` (B3 소비). FRED 시드: 물가(CPIAUCSL, CPILFESL, PCEPILFE), 금리·수익률(FEDFUNDS, DFEDTARU, DGS2, DGS10, DGS30, T10Y2Y, T10YIE), 고용(UNRATE, PAYEMS, ICSA), 성장(GDP, GDPC1, INDPRO, RSAFS), 주택(HOUST), 시장(VIXCLS, SP500) — factor는 `factor_taxonomy` 예비값(inflation/interest_rate/labor/growth/housing/market_volatility, P2 정식 조인 전 라벨만).

- [ ] **Step 1: 실패 테스트**:

```python
def test_every_seed_metric_is_in_metric_dictionary():
    seed = load_series_seed()
    catalog = metric_dictionary_catalog()
    for series_key, definition in seed.items():
        assert catalog.canonicalize(definition.canonical_metric) is not None, series_key

def test_fred_seed_has_vintage_domain_and_factor_label():
    seed = load_series_seed()
    cpi = seed["macro_cpi_yoy"]
    assert cpi.provider == "fred" and cpi.domain == "macro"
    assert cpi.factor in KNOWN_FACTOR_LABELS
```

- [ ] **Step 2: 실패 확인** → FAIL
- [ ] **Step 3: 구현** — 시드 YAML + 로더 + metric_dictionary 등록(aliases에 한글 포함: "소비자물가", "기준금리" …). 주의: metric_dictionary sha256 바인딩이 릴리스 manifest에 핀되므로 A5 재빌드 이후 바인딩 갱신 필요 — B3 빌드 태스크에서 처리.
- [ ] **Step 4: 통과 확인 + 커밋** — `feat: observation series seed and metric dictionary extension`

### Task B3: observations.sqlite 스토어 + 빌더 + 검증기 (krw-ontology)

**Files:**
- Create: `src/krw_ontology/observation/store.py` — DDL·버전 상수·적재·질의
- Create: `src/krw_ontology/observation/builder.py` — 배치 빌드(프로바이더 → 불변 아티팩트)
- Modify: 릴리스 파이프라인 — `indexes/observations.sqlite`를 릴리스 산출물에 포함(chart_series와 동일한 절차), metric_dictionary 바인딩 갱신
- Test: `tests/unit/observation/test_store.py`

**Interfaces:**
- Produces (B4/B5 소비): 버전 상수 `OBSERVATIONS_SCHEMA_VERSION = "krw-ontology-observations/v1"`, 테이블 5종(스펙 §6: `series_catalog`, `observations` PK(series_key, phenomenon_time, vintage), `ohlcv_observations` PK(ticker, trade_date), `observation_revisions`, `release_events`) + 질의 함수:

```python
def query_market_series(ticker: str, canonical_metric: str, periods: int) -> dict: ...      # format: "market-series/v1"
def query_macro_series(canonical_metric: str, limit: int) -> dict: ...                       # format: "macro-series/v1"
def query_recent_releases(factor: str | None, limit: int) -> dict: ...                       # 최근 발표(서프라이즈 포함, P1은 FRED 발표일만)
```

- [ ] **Step 1: 실패 테스트** — 메모리 빌드: fixture 프로바이더 → 빌드 → 스키마 검증 → 질의(빈티지 2개 중 최신 result_time 승리, supersede 엣지 생성, unavailable은 unavailable 행 없이 상태만):

```python
def test_latest_vintage_wins_and_supersede_edge_exists(tmp_path):
    store = build_store_from_results(tmp_path, [vintage_fetch_result("2026-05-01", old=296.1, new=296.4)])
    latest = store.latest_observation("macro_cpi_yoy", "2026-05-01")
    assert latest.value == 296.4
    revisions = store.revisions_for("macro_cpi_yoy", "2026-05-01")
    assert len(revisions) == 1 and revisions[0].revision_kind == "revision"
```

- [ ] **Step 2: 실패 확인** → FAIL
- [ ] **Step 3: 구현** — DDL·빌더·`verify_observations_schema`(테이블 집합·버전 하드실패, chart_series 검증 관행)·릴리스 통합. `market-series/v1` 페이로드 규격: `{format, ticker, canonical_metric, unit, currency, status, source_usage:"research_only", advisory_only:true, fetched_at(빌드시각), as_of(최신현상시점), points:[{date, value, observation_id}]}` — vendor 필드는 `provider` 내부용만, 응답 투영에서 재기입.
- [ ] **Step 4: 소규모 실제 빌드** — FRED 3시리즈 + 티커 1개 가격으로 `releases/v2-dev` observations.sqlite 1회 생성 → 검증 통과 기록.
- [ ] **Step 5: 통과 확인 + 커밋** — `feat: observations store v1 with vintage supersede edges and release integration`

### Task B4: chart_series 관측 합류 (krw-ontology)

**Files:**
- Modify: `src/krw_ontology/agent_index/chart_series.py` — `source_class`에 `"observation"` 추가, 빌더가 observations.sqlite에서 시리즈 투영, 차트 안전 지표 확장(가격·거시), 빌더 버전 범프(v3→v4), FTS/기간 규칙 그대로(관측 기간은 `CY2026M07` 월간 허용 확장 — FRED 월간. 기간 정규식에 `M` 추가)
- Test: `tests/unit/test_chart_series.py`에 관측 케이스 추가

**Interfaces:**
- Produces: chart pack에 `source_class:"observation"` 시리즈가 `object_id` 대신 `observation_id` 포인터로 참여(기존 파이프라인 `_meta`→Rust는 관측도 evidence_ref로 수용하도록 B6에서 확장)

- [ ] **Step 1: 실탘 테스트** — observations fixture 스토어 → chart_series 빌드 → CPI 24개월 라인 시리즈 생성·차트 안전 통과 확인
- [ ] **Step 2: 실패 확인 → 구현 → 통과 확인** — 위 내용. `compile` 규칙(Rust)과 기간 문자열 합의: 월간은 `CY2026M07` (Rust 파서 확장은 B6).
- [ ] **Step 3: 커밋** — `feat: chart series joins observation store (monthly macro series)`

### Task B5: capability runtime 도구 2종 (krw-agnet: 임포트 리핀 + descriptors)

**Files:**
- Modify: `scripts/import_capability_runtime.sh:7` — SOURCE_COMMIT을 krw-ontology v2 커밋으로 갱신 + `SOURCE_PATHS`에 `src/krw_ontology/observation/__init__.py`, `observation/store.py`, `observation/ports.py` 추가 → 실행(`services/krw-ontology-runtime`에 개명 반입)
- Create: `services/krw-ontology-runtime/src/krw_capability_runtime/observation/tools.py` — `krw_market_series`/`krw_macro_series` 핸들러(입력 검증·바운드·투영·밴더 스크럽)
- Modify: `services/.../transport/mcp/descriptors.py:508-509` — 도구 2종 등록(`market.series`, `macro.series` 논리 id, 레인 BROAD, readOnlyHint) + 도큐스트링 "15 ontology-side tools (13 ontology + 2 observation), 1 market, 14 Guru" 갱신. 카운트 불변은 `server_schema_bundle_hash` 핀(broker·deployment)이 기계적으로 강제.
- Test: `services/krw-ontology-runtime/tests/test_observation_tools.py`

**Interfaces:**
- Consumes: B3의 `query_market_series`/`query_macro_series`(임포트된 `krw_capability_runtime.observation.store`)
- Produces: MCP 도구 `krw_market_series(ticker, metric, periods)` / `krw_macro_series(metric, limit)` — 출력 `market-series/v1`/`macro-series/v1`, `_meta` 프레젠테이션 팩(B4 시리즈, PRESENTATION_META_KEY 재사용)

- [ ] **Step 1: 실패 테스트** — 도구 호출이 ①스토어 질의 위임 ②`advisory_only:true·source_usage:"research_only"` 보존 ③`provider` 원문 노출 없음(밴더 스크럽) ④스토어 미구성/빈 시계열 → `status:"unavailable"`(예외 아님) 임을 검증:

```python
def test_market_series_tool_scrubs_vendor_and_stays_advisory(tmp_path):
    register_fake_release(tmp_path)  # observations.sqlite 픽스처 1개
    result = market_series_tool(ticker="SO", metric="last_price", periods=5)
    assert result["format"] == "market-series/v1"
    assert result["advisory_only"] is True
    assert "fmp" not in canonical_json(result).lower()
```

- [ ] **Step 2: 실패 확인 → 구현(임포트 리핀 포함) → 통과 확인** — `cd services/krw-ontology-runtime && uv run pytest tests/test_observation_tools.py -v` 녹색 + 기존 runtime 테스트 전체 녹색.
- [ ] **Step 3: 커밋 (krw-agnet)** — `feat: krw_market_series and krw_macro_series capability tools over observation store`

### Task B6: 엔진 계약 + 어댑터 매핑 (krw-agnet)

**Files:**
- Create: `contracts/kernel/v1/schemas/market-series-request-v1.json`, `market-series-result-v1.json`, `macro-series-request-v1.json`, `macro-series-result-v1.json` (content_hash 핀 관행)
- Modify: `crates/capability-runtime/src/lib.rs` — `CapabilityResultIngest`에 `MarketSeriesV1`, `MacroSeriesV1` 변형 + 프리페치용 `market_series_preflight_invocation`은 **P1 불필요**(run-start 스냅샷은 기존 유지) — 워크플로우 호출용 등록만
- Modify: `crates/krw-ontology-adapter/src/lib.rs` — `map_market_series`/`map_macro_series`(시작점 `map_market_snapshot`:2053 패턴 복제): 형식·advisory·연구전용 검증 후 `EvidenceRecord`(grade Unverified, `strong_claim_allowed:false`, citation.document_type `market_series`/`macro_series`, predicates `market_series_<metric>`/`macro_<metric>`) + `_meta` 팩 통과(관측 팩은 schema_version 2 수용 확장)
- Modify: `crates/krw-presentation/src/lib.rs` — 기간 파서 `CY2026M07` 월간 수용 + `reaction` 인텐트는 P2 제외(P1: line/bar/donut에 월간 축만)
- Test: `crates/krw-ontology-adapter` 단위 테스트에 위조 거부 케이스

- [ ] **Step 1: 실패 테스트(Rust)** — ①`advisory_only:false` 위조 거부 ②vendor 문자열 누출 거부 ③정상 페이로드 → EvidenceRecord 불변식 ④`CY2026M07` 시리즈로 line 아티팩트 컴파일:

```rust
#[test]
fn market_series_forgery_advisory_flag_is_rejected() {
    let mut payload = valid_market_series_payload("SO", "last_price");
    payload["advisory_only"] = json!(false);
    let delta = map_market_series(&payload, &["SO"], Context::test());
    assert!(delta.evidence_records.is_empty());
    assert!(delta.contains_unavailable_marker());
}
```

- [ ] **Step 2: 실패 확인** — `cargo test -p krw-ontology-adapter market_series` → FAIL
- [ ] **Step 3: 구현 → 통과 확인** — `cargo test -p krw-ontology-adapter -p krw-presentation -p capability-runtime` 녹색
- [ ] **Step 4: 커밋** — `feat: engine contracts and advisory mappings for market/macro series`

### Task B7: 에이전트 배선 + 배포 바인딩 (krw-agnet)

**Files:**
- Modify: `agents/krw-ontology/agent.yaml` — capabilities 2종(`market.series`, `macro.series`, execution remote binding, input/output contracts, `result_ingest`, `scope_binding: trusted_ticker_set` — macro는 스코프 면제), 워크플로우 상태 `market_series_lookup`/`macro_series_lookup`(max_visits 1, prerequisites `ontology.query_context`), action_limits·추정치(research_action estimate) — `market.snapshot`(agent.yaml ≈:385) 블록을 템플릿으로
- Modify: `deployments/local/deployment-binding*.yaml`, `deployments/prod/deployment-binding*.yaml` — 바인딩 2종(`mcp_tool_name`, endpoint_ref `krw-ontology-local` 재사용, server_schema_bundle_hash 갱신)
- Test: `crates/runtime-config` 바인딩 검증 테스트 + agent.yaml 계약 테스트(기존 스타일)

- [ ] **Step 1: 실패 테스트** — 런타임 설정 로더가 신규 바인딩을 필수 키 없이 거부하는 기존 패턴에 2종 추가 + agent.yaml 파서 테스트(상태 전이·action_limits 위반 거부)
- [ ] **Step 2: 실패 확인 → 구현 → 통과 확인** — `cargo test -p runtime-config -p agent-kernel`(또는 agent.yaml 검증 크레이트) 녹색
- [ ] **Step 3: 커밋** — `feat: agent workflow states and deployment bindings for observation tools`

### Task B8: 엔드투엔드 스모크 + 품질 매트릭 서브셋 (양 저장소)

**Files:**
- Create: `scripts/test_observation_e2e.sh` — v2-dev 릴리스 루트로 로컬 스택 게이트웨이+capabilityd 기동(build-env 관행: stale-pid 정리, set -a env, 타임아웃), `tools/list`에 도구 2종 확인, `tools/call` 1회씩(available 경로 + unavailable 경로)
- Test: 품질 매트릭 `--case` 서브셋 3건(기존 관행): 관측 포함 질문 1("SO 최근 주가 흐름과 밸류에이션 배수는?") + 기존 골든 2건 회귀(관측 미사용 답변 불변 확인)

- [ ] **Step 1: e2e 스모크 실행** — 도구 2종 리스트·호출 성공, 어댑터 통과, 차트 1건(`last_price` 라인) `chat_message_visualizations` 투영 로컬 확인
- [ ] **Step 2: 매트릭 서브셋 녹색** — 환각 0, 프로세스 누출 0, advisory 라벨("관측 시점 데이터" 표기) 확인. **실패 시 Track 중단·수정·재측정.**
- [ ] **Step 3: 최종 리포트+커밋** — parity_report_v3.md에 e2e 결과 추가, 양 저장소 커밋 정리(`git log --oneline` 리뷰), 원본 저장소 반영은 별도 승인 대기(사용자 결정).

---

## 셀프리뷰 기록 (2026-08-30)

- 스펙 커버: P1 도메인(시세 B1·배수 B1·FRED+빈티지 B1/B2), observations.sqlite 5테이블(B3), 지표 사전(B2), 도구 2종(B5), 엔진 6단계 체크리스트(B5-B7: 계약·descriptors·agent.yaml·어댑터·바인딩·프리플라이트=B8 tools/list), 차트 합류(B4/B6), 정리 A1-A4, 재빌드+패리티(A5) — 스펙 P1 범위 전부. P2(체인 4종·VALUATION_STOP 완화·TrustedMacroContext·reaction 차트)·P3(워크스페이스)은 명시적 제외 — 분리 후속 계획.
- 타입 일치: `SeriesFetchRequest/RawObservation`(B1)→빌더(B3)→`query_market_series`(B3)→핸들러(B5)→`map_market_series`(B6)→agent.yaml(B7) 이름 일관. `observation_id` 포인터(B4↔B6) 합의.
- 플레이스홀더 스캔: A5 빌드 CLI 하위명령·B1 라이브 픽스처 취득은 "실행 시 확인" 항목 — 코드가 아니라 운영 데이터 취득 단계로 판단, 유지(임의 명령 각색 금지 원칙상 실제 CLI 확인 후 기입).
