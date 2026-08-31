# openbb-gateway

엔진과 OpenBB 사이의 서비스 계층. 엔진 코어(Rust)는 건드리지 않는다(축2 원칙: 번역은 서비스층).

## 구성

### 1. 어댑터 (`openbb_gateway.adapter`)

공개 openbb-mcp 앞의 준비성 지문 역방향 프록시. 엔진의 `krw-openbb-local`
엔드포인트는 MCP 피어가 같은 오리진에서 `krw-capabilityd/readiness/v1`
문서를 서빙할 것을 요구하지만(fail-closed) 공개 서버에는 그 엔드포인트가
없어, 이 어댑터가:

- `GET /readyz` → 업스트림 `tools/list` 번들에서 매 60초 재계산한
  `tool_schema_sha256`(정규화 번들의 해시)과 `release_manifest_sha256`
  (업스트림 원문 바이트의 해시 — 서버가 바뀌면 함께 바뀜)으로 문서 합성
- 그 외 전부 → 업스트림으로 버퍼 프록시(홉별 헤더 제거, 8MiB 본문 상한)

```bash
OPENBB_GATEWAY_ALLOW_LOCAL_UPSTREAM=1 python3 -m openbb_gateway.adapter
# [openbb-gateway] http://127.0.0.1:9447 -> 127.0.0.1:8001
```

**보안 자세**: 아웃바운드는 http/https 의미만, 접속 전 호스트 검증.
루프백/사설/예약 주소는 기본 거부 — 로컬 개발 스택이 루프백 openbb를
프록시하는 것은 `OPENBB_GATEWAY_ALLOW_LOCAL_UPSTREAM=1`로 명시적 옵트인한
경우뿐(프로덕션이 내부 주소를 조용히 겨냥하지 못하게). 자격증명은 환경변수
이름으로만 읽고 값은 절대 로그하지 않는다.

라이브 검증(2026-08-31): 로컬 openbb(219도구) 상대로 readyz 지문 생성,
프록시 경로 tools/list 219개, 실제 `equity_price_quote` 호출(AAPL) 모두
정상. `live-spike/openbb_gateway_adapter.py`에서 이식 — 스파이크가 미뤄둔
호스트 검증·설정 주입·테스트를 이번에 완결했다.

### 2. 위젯 퍼블리셔 (`openbb_gateway.widget_publisher`)

엔진의 결정론적 `krw-visualization`(v4) 산출물 → 워크스페이스
`create_widget` 함수호출 페이로드 번역. 리서치 결과가 채팅 텍스트에
가라앉지 않고 대시보드 자산으로 남게 한다(축3).

- 뷰 1개 = 위젯 1개(`view_id` → `krw-{view_id}`)
- 결정론 UUID: `origin + widget_id` 해시(blake2b/8바이트) — 재게시 멱등
- 계보 보존: 데이터소스는 artifact 종류, 뷰의 evidence_ref + 산출물
  수준 provenance를 모두 병합해 전달. **URL을 조작하지 않는다**
- 차트 타입은 보수적으로 매핑(line→line_chart 등), 모르는 타입은
  원문 통과(추측 금지)
- 산출물 형식 불일치·계열 없는 뷰·origin 누락은 정직한 오류로 반려

호스트 어댑터는 `to_openbb_function_call()` 로 워크스페이스 SSE 채널에
전달한다. 배포된 워크스페이스의 인자 필드명이 다르면 이 모듈의 한 곳에서만
조정한다(엔진 무관).

## 테스트

```bash
cd services/openbb-gateway
PYTHONPATH=src python3 -m unittest discover -s tests   # 19 tests
# 또는 uv: uv run pytest
```

모든 단위테스트는 주입된 트랜스포트만 사용(네트워크 없음). 라이브
스모크는 위 명령 + curl.

## 환경변수

| 이름 | 기본 | 설명 |
|---|---|---|
| `OPENBB_ADAPTER_UPSTREAM_HOST` | 127.0.0.1 | 업스트림 openbb-mcp 호스트 |
| `OPENBB_ADAPTER_UPSTREAM_PORT` | 8001 | 업스트림 포트 |
| `OPENBB_ADAPTER_LISTEN_PORT` | 9444 | 어댑터 수신 포트 |
| `OPENBB_ADAPTER_BUILD_ID` | openbb-mcp-local-1 | 준비성 문서 build_id |
| `OPENBB_ADAPTER_RELEASE_SHA256` | sha256:0…0 | 릴리스 핀(제로 핀은 엔진이 거부) |
| `OPENBB_GATEWAY_ALLOW_LOCAL_UPSTREAM` | (끔) | 루프백/사설 업스트림 명시적 허용 |
| `OPENBB_ADAPTER_UPSTREAM_TIMEOUT_SECONDS` | 120 | 업스트림 타임아웃 |
