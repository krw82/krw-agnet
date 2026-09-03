# P1 리뷰 수정 번들 + 오픈소스 민감정보 정리 — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 2026-09-03 전체 리뷰에서 확정된 P1 5건 + 핵심 P2 4건을 수정하고, 저장소 4곳을 오픈소스 공개 가능 상태(민감정보 제거)로 만든다.

**Architecture:** 각 태스크는 독립 실행 가능한 수정 단위(저장소 1개, 테스트 사이클 1회, 커밋 1개). 수정은 모두 기존 계약(docstring에 명시된 불변식)을 계약대로 작동하게 만드는 방향이며 새 기능이 없다. 마지막 태스크는 git 추적 파일 전수 스캔으로 민감정보를 찾아 제거한다.

**Tech Stack:** Python 3.12(uv) / pytest / httpx / FastAPI / Rust(참조만)

## Global Constraints

- 작업 폴더: `~/krw-ontology-v2` (샌드박스). 원본 저장소·`~/krw-ontology-data/releases/prod` 수정 금지.
- 네트워크 호출 금지(테스트는 픽스처/목 트랜스포트만). FRED 실측 픽스처 재기록은 별도 작업으로 분리(이 플랜에는 코드 계약 수정만).
- 커밋: 태스크 종료 시마다, 원자적, prefix `fix|test|chore`.
- 브랜치: main 위 저장소(krw-backend, krw-agent-service, openbb-fmp-extra)는 `fix/p1-review-batch` 브랜치 생성 후 작업. krw-ontology는 기존 `feat/observation-v3-bundle`, krw-agnet는 기존 `feat/engine-v2-e1`에서 이어감.
- 오픈소스 교리 유지: 벤더 토큰(FMP/FRED/Polygon)은 서빙 텍스트에 비노출, 키·토큰·개인정보는 커밋·로그·에러 메시지에 비노출.

---

### Task 1: FRED 빈티지 모드 — realtime_start 조기 바운드 (krw-ontology)

**근거:** FRED `series/observations`는 `realtime_start`/`realtime_end` 미지정 시 둘 다 **오늘**로 기본값이 잡힌다. 빈티지 모드가 아무 파라미터도 보내지 않으면(fred.py:88-93) 실수집에서 시기당 정확히 1행(vintage=수집일)만 돌아와 `observation_revisions`가 항상 비고, 이 브랜치의 핵심 기능(포인트인타임 빈티지)이 무음 노오프. ALFRED 전체 빈티지 뷰는 명시적 조기 `realtime_start`(문서상 가장 이른 날짜 `1776-07-04`)이 필요하다. 관측창 필터는 이미 클라이언트 측(fred.py:139-143)이므로 조기 바운드를 추가해도 첫 릴리스 보존 불변식(2026-05-20 빈티지 유지 테스트)은 그대로 만족된다.

**Files:**
- Modify: `src/krw_ontology/observation/providers/fred.py:79-99, 1-11` (docstring 포함)
- Test: `tests/unit/observation/test_providers.py` (기존 `test_fred_vintage_mode_requests_all_vintages_without_realtime_bounds` 교체)

**Interfaces:**
- Consumes: 기존 `SeriesFetchRequest`, `urlencode` 파라미터 빌드
- Produces: 빈티지 모드 URL에 `realtime_start=1776-07-04` 항상 포함 (비빈티지 모드는 변경 없음)

- [ ] **Step 1: 실패 테스트로 교체** — 기존 테스트의 단언 3줄(`assert "realtime_start" not in url` 등)을 다음으로 교체:

```python
    # All-vintages view REQUIRES an explicit early realtime_start: FRED
    # defaults the real-time window to *today* when omitted, which would
    # collapse every series to a single fetch-date vintage (no revisions).
    assert "realtime_start=1776-07-04" in url
    assert "realtime_end" not in url
    assert "vintage_dates" not in url
```

테스트 이름을 `test_fred_vintage_mode_requests_all_vintages_with_early_realtime_start`로 변경한다. 같은 파일에 비빈티지 모드 URL 계약 고정 테스트를 추가:

```python
def test_fred_latest_mode_sends_no_realtime_bounds():
    requests: list[Any] = []
    provider = FredProvider(
        api_key="test",
        include_vintages=False,
        opener=fixture_opener(load_fixture("fred_cpi_vintage.json"), requests=requests),
    )
    provider.fetch_series(_fred_request())
    url = requests[0].full_url
    assert "realtime_start" not in url
    assert "realtime_end" not in url
```

- [ ] **Step 2: 실패 확인** — `uv run pytest tests/unit/observation/test_providers.py -k "realtime" -v` → FAIL
- [ ] **Step 3: 구현** — fred.py `params` 빌드 블록의 빈티지 주석 블록(88-93)을 교체:

```python
        if self._include_vintages:
            # All-vintages (ALFRED) view: FRED defaults the real-time window
            # to *today* when realtime_start is omitted, which would collapse
            # every series to one fetch-date vintage and collect zero revision
            # history. The documented earliest bound selects every vintage;
            # observation_start/end (the phenomenon window) stay untouched and
            # the phenomenon window is still applied client-side below, so a
            # first release published before request.start survives.
            params["realtime_start"] = "1776-07-04"
```

모듈 docstring 3-5행("no real-time or ``vintage_dates`` bounds are sent")을 "an explicit early ``realtime_start`` bound selects the ALFRED all-vintages view"로 수정. `params_hash`가 키를 포함하므로 프로바이던스 해시는 자동 갱신된다.
- [ ] **Step 4: 통과 확인** — Step 2 재실행 PASS + `uv run pytest tests/unit/observation -q` 녹색
- [ ] **Step 5: 커밋** — `fix(observation): send early realtime_start so vintage mode actually collects revisions`

### Task 2: openbb-fmp-extra — URL 인코딩 + API 키 비노출 (openbb-fmp-extra)

**근거:** `build_url`(helpers.py:26-34)이 f-string 조립이라 (a) `symbol="GCUSD&from=2000-01-01"` 같은 값이 쿼리 인젝션 되고 (b) aiohttp `InvalidURL` 계열 예외의 메시지에 URL(=apikey) 전체가 문자열화되어 전파된다. openbb-core `amake_request`는 전송 예외를 감싸지 않는다(openbb-core 1.6.13 `provider/utils/helpers.py:385-390` 확인됨).

**Files:**
- Modify: `openbb_fmp_extra/providers/fmpextra/utils/helpers.py`
- Test: `tests/test_fmpextra.py` (신규 케이스 추가)

**Interfaces:**
- Consumes: `amake_request` (openbb-core), 기존 fetcher들의 `build_url(endpoint, api_key, **params)` 호출 시그니처 — 불변
- Produces: 동일 시그니처. `get_data_many`는 전송 실패 시 **URL/키를 포함하지 않는** `OpenBBError`를 raise

- [ ] **Step 1: 실패 테스트** — `tests/test_fmpextra.py`에 추가:

```python
import pytest
from openbb_core.app.errors import OpenBBError

from openbb_fmp_extra.providers.fmpextra.utils.helpers import build_url, get_data_many


def test_build_url_percent_encodes_parameter_values():
    url = build_url("quote", "SECRETKEY", symbol="GC&USD also spaces")
    assert "symbol=GC%26USD%20also%20spaces" in url
    assert "&" not in url.split("symbol=")[1].split("&apikey=")[0]


def test_build_url_keeps_apikey_last_and_encoded():
    url = build_url("quote", "k&y=1")
    assert url.endswith("apikey=k%26y%3D1")


@pytest.mark.asyncio
async def test_get_data_many_never_leaks_url_or_apikey_on_transport_error(monkeypatch):
    async def exploding(url, **kwargs):
        raise RuntimeError(f"boom while fetching {url}")  # message carries the URL

    monkeypatch.setattr(
        "openbb_fmp_extra.providers.fmpextra.utils.helpers.amake_request", exploding
    )
    # amake_request is imported inside the function; patch at the source module.
    monkeypatch.setattr(
        "openbb_core.provider.utils.helpers.amake_request", exploding
    )
    with pytest.raises(OpenBBError) as excinfo:
        await get_data_many("https://financialmodelingprep.com/stable/quote?symbol=X&apikey=SECRETKEY")
    assert "SECRETKEY" not in str(excinfo.value)
    assert "financialmodelingprep.com" not in str(excinfo.value)
```

(실행 시 `OpenBBError` 실제 임포트 경로와 `amake_request` 지연 import 패치 지점을 확인해 맞춘다 — helpers.py:13이 함수 내 import이므로 소스 모듈 패치가 유효하다.)

- [ ] **Step 2: 실패 확인** — `python -m pytest tests/test_fmpextra.py -k "build_url or leaks" -v` → FAIL
- [ ] **Step 3: 구현** — helpers.py 전체 교체:

```python
"""FMP extra helpers: request + response handling for the stable API."""

from __future__ import annotations

from typing import Any
from urllib.parse import urlencode

BASE_URL = "https://financialmodelingprep.com/stable"


async def get_data_many(url: str, **kwargs: Any) -> list[dict]:
    """Get a list of records from an FMP stable endpoint.

    Transport failures are re-raised as ``OpenBBError`` carrying the exception
    CLASS NAME only: aiohttp/yarl errors stringify to the full request URL,
    which contains the API key — it must never propagate to logs or clients.
    """
    from openbb_core.app.errors import OpenBBError
    from openbb_core.provider.utils.helpers import amake_request

    try:
        data = await amake_request(url, **kwargs)
    except Exception as exc:  # noqa: BLE001 - redaction boundary, see docstring
        if isinstance(exc, OpenBBError):
            raise
        raise OpenBBError(f"FMP request failed: {exc.__class__.__name__}") from exc
    if isinstance(data, dict):
        if "error" in data:
            raise OpenBBError(str(data["error"])[:200])
        return [data]
    if not isinstance(data, list):
        raise OpenBBError("unexpected FMP response type")
    return [d for d in data if isinstance(d, dict)]


def build_url(endpoint: str, api_key: str | None, **params: Any) -> str:
    """Return a stable-API URL with non-None query parameters attached.

    Values are percent-encoded via ``urlencode`` (an unencoded ``&`` in a
    symbol would inject extra query parameters); the key rides the query
    string exactly as FMP documents it — callers must never log the URL.
    """
    query = {key: value for key, value in params.items() if value is not None}
    query["apikey"] = api_key or ""
    return f"{BASE_URL}/{endpoint}?{urlencode(query)}"
```

(`OpenBBError`의 임포트 경로가 `openbb_core.app.errors`가 아니면 실행 시 실제 경로로 잡는다.)
- [ ] **Step 4: 통과 확인** — Step 2 재실행 PASS + `python -m pytest tests -q` 전체 녹색
- [ ] **Step 5: 커밋** — `fix(helpers): percent-encode query params and redact transport errors (API key never leaks)`

### Task 3: krw-backend — DCF 그리드 검증이 잘못된 코너/하한 검사 (krw-backend)

**근거:** dcf.py:116은 `GROWTH_STEPS[0]`(최솟값 −0.5)로 최대 코너를 검사하고 `1.0 <=` 하한을 덧붙여, 위젯이 허용한다는 −2~10% 성장률 중 1.5% 미만 전체를 422 거부. 계약(docstring "any grid corner")대로라면 `GROWTH_STEPS[-1]`(최댓값 +0.5) 대 `WACC_STEPS[0]`(최솟값 −1.0)을 검사해야 한다. 최종 안전망은 equity_value_per_share의 g>=w 검사(dcf.py:138)가 이미 담당.

**Files:**
- Modify: `krw_backend/dcf.py:113-124`
- Test: `tests/test_dcf_endpoints.py`

**Interfaces:**
- Produces: `validate_grid_bounds(wacc_percent, growth_percent)` — 시그니처 불변, `growth_percent ∈ [-2.0, 1.5)`가 더 이상 DcfMathError 아님

- [ ] **Step 1: 실패 테스트** — `tests/test_dcf_endpoints.py`에 추가 (임포트는 기존 파일의 관례 따름):

```python
def test_validate_grid_bounds_accepts_documented_growth_range():
    from krw_backend.dcf import validate_grid_bounds

    # The widget advertises growth -2~10; every documented growth at a
    # sane wacc must pass (the old guard rejected growth < 1.5).
    for growth in (-2.0, -1.0, 0.0, 1.0, 1.49, 10.0):
        validate_grid_bounds(9.0, growth)


def test_validate_grid_bounds_rejects_dangerous_corner():
    from krw_backend.dcf import DcfMathError, validate_grid_bounds
    import pytest

    # max growth corner (g+0.5) must stay clear of min wacc corner (w-1.0).
    with pytest.raises(DcfMathError):
        validate_grid_bounds(1.0, 1.0)  # corners: growth 1.5 vs wacc 0.0
```

- [ ] **Step 2: 실패 확인** — `KRW_BACKEND_API_KEYS=test python -m pytest tests/test_dcf_endpoints.py -k grid_bounds -v` → 1번째 FAIL
- [ ] **Step 3: 구현** — `validate_grid_bounds` 코너 검사 줄(dcf.py:116)을 교체:

```python
    if not 0.5 <= wacc_percent <= 30.0:
        raise DcfMathError("wacc는 0.5%~30% 범위여야 합니다.")
    if not -2.0 <= growth_percent <= 10.0:
        raise DcfMathError("성장률은 -2%~10% 범위여야 합니다.")
    if not growth_percent + GROWTH_STEPS[-1] < wacc_percent + WACC_STEPS[0] - 0.05:
        raise DcfMathError(
            "성장률 그리드 최댓값이 할인율 그리드 최솟값 이상입니다 — "
            "wacc를 올리거나 growth를 내리세요."
        )
```

(범위 검사를 코너 검사보다 먼저 두어 오류 메시지 순서를 예측 가능하게 한다.)
- [ ] **Step 4: 통과 확인** — Step 2 재실행 PASS + `KRW_BACKEND_API_KEYS=test python -m pytest tests -q` 녹색
- [ ] **Step 5: 커밋** — `fix(dcf): validate the actual max-growth vs min-wacc grid corner; stop rejecting growth < 1.5`

### Task 4: krw-backend — openbb 평면 실패를 502로 매핑 (krw-backend)

**근거:** `fetch_dcf_inputs`(dcf.py:57-110)는 `httpx.ConnectError/ConnectTimeout`(기본 대상 127.0.0.1:6900이 꺼져 있을 때 = 최다 실패)와 `.json()`의 `JSONDecodeError`(HTML 에러 페이지)를 밖으로 새어나가게 둔다. app.py는 `DcfInputError`만 502로 매핑하므로 결과는 500+트레이스백. docstring 계약("Upstream data missing or malformed (mapped to 502)")대로 래핑.

**Files:**
- Modify: `krw_backend/dcf.py:57-110`
- Test: `tests/test_dcf_endpoints.py` (또는 신규 `tests/test_dcf_inputs.py`)

**Interfaces:**
- Produces: `fetch_dcf_inputs`는 전송/디코딩 실패 시 `DcfInputError` raise — 미처리 예외 경로 소멸

- [ ] **Step 1: 실패 테스트**:

```python
import httpx
import pytest

from krw_backend.dcf import DcfInputError, fetch_dcf_inputs


def _inputs_page(handler) -> httpx.MockTransport:
    return httpx.MockTransport(handler)


@pytest.mark.asyncio
async def test_fetch_dcf_inputs_wraps_transport_failure_as_input_error():
    def handler(request):
        raise httpx.ConnectError("connection refused", request=request)

    with pytest.raises(DcfInputError):
        await fetch_dcf_inputs("AAPL", 5, client=httpx.AsyncClient(transport=_inputs_page(handler)))


@pytest.mark.asyncio
async def test_fetch_dcf_inputs_wraps_non_json_body_as_input_error():
    def handler(request):
        return httpx.Response(200, text="<html>proxy login page</html>")

    with pytest.raises(DcfInputError):
        await fetch_dcf_inputs("AAPL", 5, client=httpx.AsyncClient(transport=_inputs_page(handler)))
```

- [ ] **Step 2: 실패 확인** — `KRW_BACKEND_API_KEYS=test python -m pytest tests/test_dcf_endpoints.py -k inputs_wraps -v` → FAIL (ConnectError/ValueError가 그대로 새어나감)
- [ ] **Step 3: 구현** — gather 블록과 파싱 블록을 하나의 try로 묶는다. dcf.py:66-110을 다음 구조로 (본체 로직은 불변):

```python
    try:
        try:
            cash, balance, quote, metrics = await asyncio.gather(
                client.get(f"{base}/equity/fundamental/cash", params=params),
                client.get(f"{base}/equity/fundamental/balance", params={**params, "limit": 1}),
                client.get(f"{base}/equity/price/quote", params={"provider": "fmp", "symbol": ticker}),
                client.get(f"{base}/equity/fundamental/metrics", params={**params, "limit": 1}),
            )
        except httpx.HTTPError as exc:
            raise DcfInputError(
                f"openbb 평면 연결 실패: {exc.__class__.__name__}"
            ) from exc

        for response, what in ((cash, "cash"), (balance, "balance"), (quote, "quote"), (metrics, "metrics")):
            if response.status_code != 200:
                raise DcfInputError(f"openbb {what}: HTTP {response.status_code}")

        try:
            cash_rows = _parse_results(cash.json(), "cash")
            balance_row = _parse_results(balance.json(), "balance")[0]
            quote_row = _parse_results(quote.json(), "quote")[0]
            metrics_row = _parse_results(metrics.json(), "metrics")[0]
        except ValueError as exc:  # json.JSONDecodeError
            raise DcfInputError("openbb 응답이 JSON이 아닙니다 (HTML 에러 페이지?).") from exc
        # ... (fcfs/balance 파싱 본체는 기존 그대로, finally/aclose 유지)
    finally:
        ...
```

기존 `own_client` finally 구조는 유지하고, 파싱 본문(fcfs 루프 ~ return DcfInputs)은 기존 코드를 그대로 안쪽에 둔다.
- [ ] **Step 4: 통과 확인** — Step 2 재실행 PASS + 기존 DCF 엔드포인트 테스트 전부 녹색
- [ ] **Step 5: 커밋** — `fix(dcf): map openbb transport/decode failures to DcfInputError (502, never a raw 500)`

### Task 5: krw-backend — 비ASCII 키 500 + /docs 무인증 노출 (krw-backend)

**근거:** (a) `hmac.compare_digest(str, str)`은 비ASCII 문자열에서 TypeError → 무인증 500(auth.py:37). (b) app.py:132-138이 FastAPI 기본 docs를 켜두어 `/docs`·`/redoc`·`/openapi.json`이 무인증 — 모듈 docstring("Unauthenticated surface: /healthz only")과 모순. (c) `AUTHED_PATHS`(test_auth.py)에 신규 엔드포인트 미추가 — 인증 회귀 탐지 불가.

**Files:**
- Modify: `krw_backend/auth.py:37`, `krw_backend/app.py:133-138`
- Test: `tests/test_auth.py`, `tests/test_readonly_cors.py` 또는 `tests/test_widgets_json.py`에 docs 케이스

**Interfaces:**
- Produces: 비ASCII `X-API-Key` → 403(500 아님). `/docs`, `/redoc`, `/openapi.json` → 404.

- [ ] **Step 1: 실패 테스트** — test_auth.py에 추가:

```python
def test_non_ascii_api_key_is_rejected_not_500(client):
    response = client.get("/widgets.json", headers={"X-API-Key": "키123"})
    assert response.status_code == 403


def test_api_docs_are_disabled(client):
    for path in ("/docs", "/redoc", "/openapi.json"):
        assert client.get(path).status_code == 404
```

`AUTHED_PATHS` 목록에 `"/dcf_heatmap"`, `"/dcf_summary"`, `"/jobs"`, `"/jobs/submit"`을 추가(파라미터 필요한 것은 최소 요청 또는 422/401 우선 검증 방식은 기존 케이스 관례 따름 — 401/403이 422보다 먼저인 경로만 편입).

```python
AUTHED_PATHS = [
    "/widgets.json",
    "/macro_series",
    "/macro_chart",
    "/dcf_heatmap",
    "/dcf_summary",
    "/markdown/queue_help",
    "/jobs",
    "/jobs/submit",
]
```

- [ ] **Step 2: 실패 확인** — `KRW_BACKEND_API_KEYS=test python -m pytest tests/test_auth.py -v` → 신규 2케이스 FAIL
- [ ] **Step 3: 구현** — auth.py:37을 `hmac.compare_digest(key.encode("utf-8"), x_api_key.encode("utf-8"))`로. app.py create_app에 `docs_url=None, redoc_url=None, openapi_url=None` 추가.
- [ ] **Step 4: 통과 확인** — 전체 `python -m pytest tests -q` 녹색
- [ ] **Step 5: 커밋** — `fix(auth,app): bytes compare for non-ASCII keys (403 not 500); disable unauthenticated /docs surface`

### Task 6: krw-agent-service — SSE 스트림 절대 완결 (krw-agent-service)

**근거:** flow.py:118은 `EngineUnavailable`만 잡고, runners.py:320-327은 `httpx.TimeoutException`/`httpx.HTTPError`만 잡는다. `httpx.InvalidURL`(스킴 없는 `KRW_AGENT_GATEWAY_URL` — 전형적 설정 실수)은 httpx 예외 계층 밖이고, gateway가 `markdown`에 문자열 아닌 값을 반환하면 `scrub_vendors`의 `re.sub`에서 TypeError. 둘 다 SSE 제너레이터를 중간에 중단 — runners.py:184-187 docstring이 "절대 일어나지 않는다"고 약속한 바로 그 실패.

**Files:**
- Modify: `krw_agent_service/runners.py:296-327`, `krw_agent_service/flow.py:114-127`
- Test: `tests/test_gateway_runner.py`, `tests/test_query_stream.py`

**Interfaces:**
- Produces: `GatewayRunner.run`은 모든 예외를 `EngineUnavailable`로 수렴(문자열은 클래스명만). `answer_flow`는 어떤 runner 예외에도 스트림 완결.

- [ ] **Step 1: 실패 테스트** — test_gateway_runner.py에 추가:

```python
@pytest.mark.asyncio
async def test_invalid_gateway_url_converges_on_engine_unavailable():
    runner = GatewayRunner(url="not-a-scheme:14318", transport=_ok_transport())
    with pytest.raises(EngineUnavailable):
        await runner.run("AAPL 실적은?")


@pytest.mark.asyncio
async def test_non_string_markdown_converges_on_engine_unavailable():
    # gateway returns markdown: 12345 → must be EngineUnavailable, not TypeError
    ...
```

(두 번째 케이스는 MockTransport가 `{"state":"final","final_output":{"markdown":12345}}`를 반환하도록 기존 헬퍼 재사용.) test_query_stream.py에 추가:

```python
@pytest.mark.asyncio
async def test_stream_completes_when_runner_raises_unexpected_exception():
    class ExplodingRunner:
        name = "exploding"

        async def run(self, question):
            raise RuntimeError("boom")

    events = [e async for e in answer_flow("AAPL 실적은?", ExplodingRunner())]
    # Refusal path: warning + refusal chunks, stream completes, no raise.
    assert any(e.get("type") == "copilotStatusUpdate" for e in events)
    assert any(e.get("type") == "copilotMessageChunk" for e in events)
```

- [ ] **Step 2: 실패 확인** — `python -m pytest tests -k "invalid_gateway or non_string or completes_when_runner" -v` → FAIL
- [ ] **Step 3: 구현** — ① runners.py 예외 체인 끝에(320-327) 다음을 추가 (순서 중요 — EngineUnavailable 재발산이 먼저):

```python
        except httpx.TimeoutException as exc:
            raise EngineUnavailable(
                f"게이트웨이 시간 초과 ({timeout_seconds:.0f}초 내 완료 안 됨)"
            ) from exc
        except httpx.HTTPError as exc:
            raise EngineUnavailable(
                f"엔진에 연결할 수 없습니다: {exc.__class__.__name__}"
            ) from exc
        except EngineUnavailable:
            raise
        except Exception as exc:  # noqa: BLE001 - the stream must always finish
            raise EngineUnavailable(
                f"게이트웨이 처리 중 오류: {exc.__class__.__name__}"
            ) from exc
```

② markdown 검증(runners.py:301-305)을 교체:

```python
                        markdown = final_output.get("markdown")
                        if not isinstance(markdown, str) or not markdown:
                            raise EngineUnavailable(
                                "게이트웨이 결과에 markdown 문자열이 없습니다."
                            )
```

③ flow.py:118 except 블록 뒤에 방어 계층 추가:

```python
    except EngineUnavailable as exc:
        ... (기존 거부 경로 그대로)
    except Exception as exc:  # noqa: BLE001 - stream must complete regardless
        yield reasoning_step(
            event_type="WARNING",
            message=scrub_vendors("엔진 처리 중 예상치 못한 오류가 발생했습니다."),
            details={"사유": exc.__class__.__name__},
        ).model_dump()
        for chunk in split_chunks(scrub_vendors(REFUSAL_MARKDOWN)):
            yield message_chunk(chunk).model_dump()
        return
```

④ runners.py GatewayRunner docstring(184-187)의 보장 문구를 "모든 예외(전송·URL·디코딩·응답 타입)가 EngineUnavailable로 수렴"으로 갱신.
- [ ] **Step 4: 통과 확인** — Step 2 재실행 PASS + `python -m pytest tests -q` 녹색
- [ ] **Step 5: 커밋** — `fix(runners,flow): converge every failure on EngineUnavailable — the SSE stream always completes`

### Task 7: krw-agent-service — 스크럽 NFKC/구분자 강화 + 티커 오탐 (krw-agent-service)

**근거:** (a) guardrails.py:25는 ASCII 전용 매칭이라 전각(`ＦＭＰ`)·단일 구분자(`f.m.p`, `f_m_p`, `f m p`)가 우회된다 — "can never leak" docstring 주장과 배치. (b) `extract_ticker`(runners.py:48-70)가 1글자 토큰을 잡고 차단 목록에 US/OR/BY 등이 없어 "M&A 거래" → M, "US 시장 전망" → US로 잘못된 기업 조사 런이 발생한다(인용 포함 확신 답변 = 정직한 거부보다 나쁨). 2글자 이상 요구 시 손실(단일 문자 티커 F, A 등)은 docstring에 명시된 대로 거부로 안전 위임.

**Files:**
- Modify: `krw_agent_service/guardrails.py:20-47`, `krw_agent_service/runners.py:44-70`
- Test: `tests/test_guardrails.py`, `tests/test_gateway_runner.py`

**Interfaces:**
- Produces: `scrub_vendors`/`contains_vendor_token` 시그니처 불변, NFKC + 구분자 우회 패턴 추가. `extract_ticker` — 2~5글자만 후보.

- [ ] **Step 1: 실패 테스트** — test_guardrails.py에 추가:

```python
def test_scrub_covers_fullwidth_homoglyphs():
    assert scrub_vendors("ＦＭＰ 데이터 기준") == "외부 소스 데이터 기준"
    assert scrub_vendors("Ｆｒｅｄ 지표") == "외부 소스 지표"


def test_scrub_covers_single_separator_obfuscation():
    for poisoned in ("f.m.p", "f_m_p", "f m p", "F.M.P 기준"):
        assert not contains_vendor_token(scrub_vendors(poisoned)), poisoned
```

test_gateway_runner.py에 추가:

```python
def test_extract_ticker_ignores_common_english_and_korean_question_words():
    assert extract_ticker("M&A 거래 알려줘") is None
    assert extract_ticker("US 시장 전망은?") is None
    assert extract_ticker("OR 클라우드 사업 분석") is None


def test_extract_ticker_still_finds_real_tickers():
    assert extract_ticker("SO 최근 실적은?") == "SO"
    assert extract_ticker("AAPL 실적은?") == "AAPL"
```

- [ ] **Step 2: 실패 확인** — `python -m pytest tests -k "fullwidth or separator_obfuscation or ignores_common" -v` → FAIL
- [ ] **Step 3: 구현** — ① guardrails.py: 토큰을 NFKC 정규화한 입력에서 매칭하고, 글자 사이 최대 1개의 구분자(`.`/`_`/`-`/공백)를 허용하는 패턴 추가:

```python
import unicodedata

_SEPARATOR = r"[._\s-]?"


def _token_variants(token: str) -> tuple[re.Pattern[str], ...]:
    escaped = [re.escape(ch) for ch in token]
    loose = _SEPARATOR.join(escaped)
    return (
        re.compile(rf"(?i)(?<![a-zA-Z0-9]){re.escape(token)}(?:io)?(?![a-zA-Z0-9])"),
        re.compile(rf"(?i)(?<![a-zA-Z0-9]){loose}(?![a-zA-Z0-9])"),
    )

_TOKEN_PATTERNS: tuple[re.Pattern[str], ...] = tuple(
    pattern for token in VENDOR_TOKENS for pattern in _token_variants(token)
)


def _normalize(text: str) -> str:
    # NFKC folds fullwidth/homoglyph spellings (ＦＭＰ → FMP) to ASCII so the
    # ASCII-boundary patterns above stay meaningful.
    return unicodedata.normalize("NFKC", text)


def scrub_vendors(text: str | None) -> str:
    if not text:
        return text or ""
    scrubbed = _normalize(text)
    for pattern in _TOKEN_PATTERNS:
        scrubbed = pattern.sub(NEUTRAL_TERM, scrubbed)
    return scrubbed
```

docstring의 "can never leak"를 "known ASCII spellings plus fullwidth-NFKC and single-separator obfuscations are rewritten; this is not a general adversarial-paraphrase filter"로 정직하게 교체. `contains_vendor_token`도 `_normalize` 적용. ② runners.py:48 `_TICKER_RE = re.compile(r"(?<![A-Za-z0-9])[A-Z]{2,5}(?![A-Za-z0-9])")`로 변경하고 차단 목록에 `"AM", "BY", "DO", "GO", "IF", "IN", "IS", "IT"(있음), "MY", "NO", "OF", "ON", "OR", "PM", "US", "VS", "NY", "LA"` 추가. docstring에 "단일 문자 티커(F, A 등)는 거부로 위임" 명시.
- [ ] **Step 4: 통과 확인** — Step 2 재실행 PASS + `python -m pytest tests -q` 녹색
- [ ] **Step 5: 커밋** — `fix(guardrails,runners): NFKC + separator-obfuscation scrub; ticker heuristic stops matching 1-letter/acronym words`

### Task 8: krw-agnet — 위젯 퍼블리셔 format 키 드리프트 (krw-agnet)

**근거:** Rust 생산자는 `artifact_format` 키로 내보내지만(krw-presentation/src/lib.rs:811) Python 게시자는 `format`을 `.get` 기본값과 함께 검사해(widget_publisher.py:91) 잘못된 포맷의 "정직한 거부"가 실제 아티팩트에서 절대 발동하지 않고, 테스트 픽스처(test_widget_publisher.py:17)가 잘못된 계약을 고정한다.

**Files:**
- Modify: `services/openbb-gateway/src/openbb_gateway/widget_publisher.py:88-94`
- Test: `services/openbb-gateway/tests/test_widget_publisher.py`

**Interfaces:**
- Produces: `publishable_widgets`는 `artifact_format`(주) 또는 `format`(호환) 키를 검사; 둘 다 없거나 값이 틀리면 `WidgetPublishError`.

- [ ] **Step 1: 실패 테스트** — test_widget_publisher.py에 추가:

```python
def test_engine_emitted_artifact_format_key_is_accepted():
    artifact = artifact_fixture()
    artifact.pop("format")
    artifact["artifact_format"] = "krw-visualization"  # what lib.rs:811 emits
    payloads = publishable_widgets(artifact, origin="run/1")
    assert len(payloads) == len(artifact["views"])


def test_missing_format_key_is_rejected_not_masked():
    artifact = artifact_fixture()
    artifact.pop("format")
    artifact.pop("artifact_format", None)
    with self.assertRaises(WidgetPublishError):
        publishable_widgets(artifact, origin="run/1")
```

(unittest.TestCase 스타일이면 `with self.assertRaises`, 함수 스타일이면 pytest.raises — 기존 파일 관례 따름.) 기존 fixture의 `"format": "krw-visualization"`를 `"artifact_format": "krw-visualization"`으로 교체(과거 `format` 키 호환은 별도 케이스로 유지).
- [ ] **Step 2: 실패 확인** — `cd services/openbb-gateway && python -m pytest tests/test_widget_publisher.py -v` → 신규 2케이스 FAIL
- [ ] **Step 3: 구현** — widget_publisher.py:91-93을 교체:

```python
    # The engine (krw-presentation lib.rs) emits "artifact_format"; "format"
    # is accepted for backwards compatibility. No masking default: a missing
    # or wrong format key is an honest rejection, never a silent pass.
    format_key = artifact.get("artifact_format") or artifact.get("format")
    _require(
        format_key == ARTIFACT_FORMAT,
        f"unsupported artifact format: {format_key!r}",
    )
```

- [ ] **Step 4: 통과 확인** — Step 2 재실행 PASS + 서비스 테스트 전체 녹색
- [ ] **Step 5: 커밋** — `fix(widget-publisher): validate the artifact_format key the engine actually emits`

### Task 9: 오픈소스 민감정보 스캔 + 정리 (전체 저장소)

**근거:** 공개를 준비 중이므로 git **추적** 파일에 키·토큰·개인 식별 정보가 있으면 안 된다. (git 히스토리 재작성은 이 플랜 범위 밖 — 스캔 결과에 권고만 기록.)

**Files:**
- Create: `~/krw-ontology-v2/OSS_READINESS.md` (워크스페이스 루트, 요약 보고서)
- Modify: 각 저장소 `.gitignore` (필요 시), `git rm --cached` (실수로 추적된 로컬 산출물)

**Interfaces:**
- Consumes: `git ls-files`, 패턴 스캔
- Produces: 스캔 보고서 + 수정 커밋들

- [ ] **Step 1: 스캔** — 저장소별 `git ls-files` 대상으로 다음을 검출:
  1. 시크릿 패턴: `apikey=`, `api_key`에 리터럴 값, `Bearer `, `token:`/`TOKEN=`, `sk-`, FRED/FMP/Polygon/Supabase 키 형태(32자+ 고엔트로피), `.env` 파일 추적 여부
  2. 개인 식별: `username`, `/Users/`, 이메일 주소 패턴
  3. 로컬 산출물 추적 여부: `krw-agnet/.local/`(에이전트 게이트웨이 로그·어댑터), `*.sqlite`, `releases/`, 터널 로그
  4. URL·호스트: 내부 호스트명·포트가 문서에 하드코딩된 곳(정보성 — 치환 여부 판단)
- [ ] **Step 2: 분류** — 각 히트를 [치환 필요 / .gitignore+rm --cached / 문서 메모만 / 무해]로 분류. 치환 필요 항목은 값을 환경변수 참조나 플레이스홀더로 교체.
- [ ] **Step 3: 수정 + 커밋** — 저장소별로 `chore: scrub local artifacts / secrets for open-source readiness` 커밋. `.gitignore`에 `.local/`, `*.sqlite`, `.env*` 등 추가.
- [ ] **Step 4: 보고서** — `OSS_READINESS.md`에: 발견·조치 목록, git 히스토리에 시크릿이 존재하는 경우 `git filter-repo` 후 폐기-재작성 권고(공개 직전 1회), 라이선스·README 공개 전 체크리스트(벤더 상표/이름 — 스크럽 교리와 연계), AGPL(openbb-krw의 openbb-core 의존) 고지 필수 여부 확인.
- [ ] **Step 5: 커밋** — 보고서는 샌드박스 루트(비 git)에 두고 각 저장소 커밋만 수행.

---

## Self-Review

1. **커버리지:** 리뷰 P1 5건(FRED 빈티지, fmp 키 유출, DCF 그리드, DCF 예외 매핑, SSE 완결) = Task 1-4, 6. P1 아닌 권장 사항 중 승인 범위(골든 계약·민감정보) = Task 5, 7, 8, 9. adapter 응답 캡/DNS 핀잉/jobs TTL은 본 플랜 범위 밖(차후 과제로 OSS_READINESS에 기록).
2. **플레이스홀더:** 없음 — 모든 코드 단계에 실제 코드 명시. 실행 시 확인 필요한 임포트 경로 2곳(OpenBBError, amake_request 패치 지점)은 명시적으로 표기.
3. **타입 일관성:** `validate_grid_bounds`·`scrub_vendors`·`extract_ticker`·`publishable_widgets` 모두 시그니처 불변 확인.
