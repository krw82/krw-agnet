# OpenBB Copilot 챗 → 실제 krw 엔진 배선 (gateway 모드) — 설계

- 날짜: 2026-09-07
- 상태: 승인됨 (오너, 2026-09-07)
- 대상 저장소: `live-spike/`(git 추적 없음 — 런처 스크립트 편집), `krw-agent-service`(변경 없음), `krw-agnet`(변경 없음)

## 1. 배경과 목표

OpenBB Workspace(pro.openbb.co) Copilot 챗에서 KRW 리서치 에이전트에게 질문하면
**실제 krw 엔진**이 돌아서 근거 인용이 붙은 한국어 답변을 내놓는 상태를 만든다.

필요한 부품은 이미 전부 존재한다:

- `krw-agent-service`의 `GatewayRunner`(`krw_agent_service/runners.py:193`) —
  게이트웨이 v1 계약(`POST {url}/runs` → `GET {url}/runs/{id}` 폴링,
  `state=="final"` → `final_output.markdown`)으로 실엔진 실행을 이미 지원한다.
  60초마다 진행 상황을 reasoning step으로 스트리밍하고(`PROGRESS_INTERVAL_SECONDS`,
  `runners.py:51`), 모든 실패는 정직한 한국어 거절로 수렴한다.
- 엔진 게이트웨이 스택 부트 스크립트(`krw-agnet/scripts/start_local_agent_gateway_stack.sh`) —
  게이트웨이 :4318 기본(`KRW_AGENT_GATEWAY_PORT`, 35행), Bearer 토큰을
  `.local/agent-gateway/secrets.env`의 `KRW_AGENT_GATEWAY_TOKEN`으로 발급(124행).
- 워크스페이스 등록 절차(`live-spike/RUNBOOK.ko.md` §4) — 이미 스파이크로 검증됨.

유일한 걸림돌: 스파이크 런처 `live-spike/run_spike.sh`가 stub을 강제한다 —

- 147행(백엔드)·167행(에이전트): `env -u KRW_AGENT_GATEWAY_URL -u KRW_AGENT_GATEWAY_TOKEN`
- 163행: `KRW_AGENT_RUNNER=stub` (모드 기본값 stub)

## 2. 접근법 (승인: A안)

**A안(채택)** — 런처에 `gateway` 모드 추가. 기본 동작(stub) 불변. 약 15줄.
**B안(기각)** — 러북 §7 문자 그대로 스크립트 직접 수정. 스파이크의 "안전한 stub
기본값" 원칙이 깨지고 재실행마다 편집 왔다갔다.
**C안(기각)** — 런처 무수정·서비스 수동 부트. 키 생성/헬스체크/teardown 트랩 등
검증된 관리 장치를 놓치고 검증 경로에서 벗어남.

A안이 유지하는 원칙: 새 실패점 추가 없음, 기존 메커니즘(`select_runner`의
URL-존재 기반 선택, `runners.py:487`) 재사용, 되돌리기 쉬움.

## 3. 설계 상세

### 3.1 `run_spike.sh` 변경

`KRW_SPIKE_AGENT_MODE`의 허용값에 `gateway`를 추가(기본값은 stub 유지, 35행).

에이전트 부트 블록(160–173행)의 모드 분기:

- `stub`: 현재와 동일(`KRW_AGENT_RUNNER=stub` + `env -u` 유지).
- `refusal`: 현재와 동일(`env -u`만).
- `gateway`:
  - `KRW_AGENT_GATEWAY_URL`이 비어 있으면 `die` — 한국어 fail-fast
    (예: "gateway 모드는 KRW_AGENT_GATEWAY_URL 이 필요합니다").
  - `env -u KRW_AGENT_GATEWAY_URL -u KRW_AGENT_GATEWAY_TOKEN` **제거** —
    운영자가 export한 URL/토큰이 uvicorn 에 상속된다. 토큰은 선택(게이트웨이가
    요구하면 필수).
  - `KRW_AGENT_RUNNER`를 강제하지 **않는다** — URL 존재만으로
    `select_runner()`가 `GatewayRunner`를 고른다.
  - `KRW_AGENT_TIMEOUT_SECONDS`가 미설정이면 **1800**(30분)을 명시 주입.
    서비스 기본 120초(`config.py:31`)는 실제 엔진 실행(수 분~수십 분)에
    부족하다. 운영자가 이미 설정했다면 그 값을 존중(덮어쓰지 않음).

백엔드 부트(147행)는 **그대로** — 잡큐 위젯의 실엔진 전환은 이번 범위 밖(§4 비목표).

READY 배너의 `[모드]` 줄에 gateway 모드면 대상 게이트웨이 URL과 타임아웃을 표시.

### 3.2 운영 절차 (원 커맨드 3단계)

```bash
# 1) 엔진 스택 부팅 (기존 스크립트 그대로; .env.local 시크릿 필요)
krw-agnet/scripts/start_local_agent_gateway_stack.sh
#    → 토큰 발급 확인: .local/agent-gateway/secrets.env

# 2) 스파이크 gateway 모드 부트
source .local/agent-gateway/secrets.env   # KRW_AGENT_GATEWAY_TOKEN
export KRW_AGENT_GATEWAY_URL=http://127.0.0.1:4318/v1/agent   # 프리픽스 필수
KRW_SPIKE_AGENT_MODE=gateway ./live-spike/run_spike.sh

# 3) 워크스페이스 질의 — 이전 스파이크에서 이미 등록했다면 URL 동일으로 그대로 동작
#    (신규 등록은 RUNBOOK §4)
```

주의: 게이트웨이 URL은 `/v1/agent` 프리픽스 포함(`AGENT_GATEWAY.md:58`의
계약이 `POST /v1/agent/runs`이고 `GatewayRunner`가 `{base}/runs`를 붙인다).

### 3.3 러북 보강

`RUNBOOK.ko.md` §7을 "스크립트 직접 수정" 안내에서 "gateway 모드 원 커맨드"
안내로 갱신한다(위 3단계 + 주의점). §4 기대 관찰에 gateway 모드 항목
(진행 표시 "근거를 확인하는 중… (N분 경과)" → 실제 인용 포함 답변) 추가.

## 4. 비목표

- 백엔드 잡큐 위젯(krw-backend 게이트웨이 러너)의 실엔진 전환 — 같은 env로
  나중에 전환 가능, 별도 진행.
- 디스패처/커버리지 운영값 세팅 — `GLM_API_KEY`·`KRW_AGENT_COVERAGE_URL`이
  없으면 티커 질문도 자유 문으로 fail-open 동작(첫 검증엔 허용). 회사 문
  사용은 운영 옵션으로 문서에만 기록.
- 스파이크 기본 동작 변경(stub 기본 유지), `krw-agent-service`/`krw-agnet` 코드 변경.

## 5. 오류 처리

이미 서비스 계층에서 수렴하도록 설계되어 있어 런처는 fail-fast만 추가한다:

- 게이트웨이 미설정/오타 → 런처 die(사전) 또는 `EngineUnavailable` → 정직한
  한국어 거절 스트림(런북 §6 디버깅 절차 그대로 적용).
- 엔진 실행 실패(`state ∈ failed/error/cancelled`)·타임아웃 → 동일 거절 수렴.
- 포트 점유(잔여 :8390 등) → 기존 lsof 사전 검사가 그대로 실패시킴(109행).

## 6. 검증 기준

성공(워크스페이스 UI): 질문 → reasoning 진행 표시가 60초마다 살아 있고 →
실제 엔진 run 완료 → 근거 인용(citations)이 붙은 한국어 마크다운 답변 +
고지문. 엔진 중단 시 정직한 거절.

회귀: 모드 미지정 부트는 기존 stub 스파이크와 완전 동일(§3.1의 stub/refusal
분기 무변경 확인). `bash -n run_spike.sh` 구문 검사. gateway 모드에서 URL
미설정 시 한국어 die 확인.

증거: `live-spike/evidence/`에 스크린샷, 원장은 기존 관례대로
`krw-agnet/.superpowers/sdd/` progress.md에 기록.
