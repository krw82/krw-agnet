# 배포 연결 안정화 (Deploy Connectivity Stabilization) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 배포 실패가 리서치 엔진↔웹 단절을 "다음 배포까지" 영구화하는 5개 래칭을 제거하여, 배포가 실패해도 시스템이 스스로 회복되게 만든다.

**Architecture:** 두 개의 독립 배포 가능한 위상으로 구성. Phase A(엔진 하드닝)는 하트비트 ABI 드리프트로 인한 크래시 루프(로그상 24,535회 치명적 exit)를 프로세스 내 백오프 재시도로 전환하고 launchd 재시작 정책을 교정한다. Phase B(배포 래칭 해소)는 단발성 파이프라인 명령에 bounded retry를 걸고, 실패한 배포를 전체 재배포 없이 복구하는 `reconcile` 서브커맨드를 추가하며, 실패 1위 원인인 `frontend-source-clean-commit` 프리플라이트를 자동 clean worktree로 우회한다.

**Tech Stack:** Rust (tokio, clap, tracing), PostgreSQL 마이그레이션(SQL), bash launchd plist, python 검증 스크립트.

## 근거 (2026-08-24 전수 분석, 수정 전)

- 배포 92건 중 성공 34 / 실패 45 / 중단 12. admission_close 이후 모든 실패는 `TerminalAdmission::Closed`로 봉인 (`bins/krw-agent-deploy/src/pipeline.rs:3358-3371`, 롤백 명령 없음).
- 실행기는 `readiness.` 접두사만 재시도 (`pipeline.rs:458-467`); scp만 3회 재시도(`pipeline.rs:82`), 나머지 단발성.
- 하트비트 프로시저가 필드 정확-일치 강제(`migrations/0022_daemon_mcp_readiness.sql`의 `assert_request` 동일 배열) + 엔진이 `InvalidResponse`/`Rejected`를 치명적으로 취급(`crates/persistence/src/daemon.rs:1539-1541`, `bins/krw-agentd/src/main.rs:430`) → 스키마-바이너리 창구 실패 시 2초 간격 크래시 루프(로그 24,094 + 441회).
- launchd `KeepAlive={SuccessfulExit:false}` + `ThrottleInterval 2`(초당 급지 재시작, exit-0 시 재시작 안 함) — `packaging/launchd/com.krw.agentd.plist:11-13`.
- 로그에 타임스탬프 없음(`bins/krw-agentd/src/main.rs:256` `.without_time()`).
- 프리플라이트 `frontend-source-clean-commit` 실패 11회 = 실패 원인 1위 (`bins/krw-agent-deploy/src/checks.rs:226-251`).

## Global Constraints

- 수정 가능한 저장소는 `~/krw-agnet`뿐. `~/krw-ontology-front`는 읽기 전용 — 웹 측 변경(admission 파생 상태화, 핀 레지스트리 추종)은 이 계획의 범위 밖이며 문서 끝 "Out of Scope"에 명시한다.
- fail-closed 철학은 유지한다: "조건이 안 되면 거부"는 그대로 두고, "조건이 회복되면 자동 재개"만 추가한다.
- 배포 프로바이더는 항상 DeepSeek, 로컬 테스트는 GLM(사용자 확립 규칙).
- 소스/예제/테스트에 사용 가능한 credential 리터럴 금지. SQL은 파라미터 바인딩만.
- 모든 커밋 전 `cargo test` 관련 크레이트 통과. 스키마 커밋 후 `cargo build --bins` 재실행(부분 디버그 재빌드 함정).
- 마이그레이션은 additive-first(expand-contract): 이번 계획의 0023는 expand 절반만 한다. contract(필드 제거)는 별도 후속.

---

## Phase A — 엔진 크래시 루프 제거 (Tasks 1-4, 독립 배포 가능)

### Task 1: 하트비트 요청 계약의 하위호환 (migration 0023)

**Files:**
- Create: `migrations/0023_heartbeat_request_backward_compatibility.sql`
- Create: `scripts/test_heartbeat_request_tolerance.py`

**Interfaces:**
- Consumes: `agent_store.assert_request(p_request, p_allowed, p_required)` (`migrations/0001_agent_v1.sql:202` — allowed ⊇ required 분리 이미 지원), `agent_store.request_boolean(p_request, p_key)` (`migrations/0001_agent_v1.sql:300` — 필드 누락 시 예외).
- Produces: `agent_v1.heartbeat_daemon`이 `mcp_ready` 없는 요청(구 바이너리)을 수용. 응답 형태는 불변(엔진 측 내성은 Task 2/3가 담당).

- [ ] **Step 1: 실패하는 계약 테스트 작성**

`scripts/test_heartbeat_request_tolerance.py` 생성. 프로덕션 DB에는 쓰지 않고 로컬 개발 스택 DB(`~/krw-agnet/.local/agent-gateway/secrets.env`의 `KRW_AGENT_DATABASE_URL` 또는 환경변수) 대상, 모든 호출을 `BEGIN ... ROLLBACK`으로 감싸 흔적을 남기지 않는다:

```python
#!/usr/bin/env python3
"""Contract test: heartbeat_daemon tolerates a pre-0023 request shape.

All calls run inside BEGIN/ROLLBACK so the shared heartbeat table is
never durably modified by this test. Run against the LOCAL dev stack
database only (never production).
"""
import json
import os
import subprocess
import sys

DB_URL = os.environ.get("KRW_AGENT_DATABASE_URL")
if not DB_URL:
    sys.exit("KRW_AGENT_DATABASE_URL (local dev stack) is required")

BASE_REQUEST = {
    "abi_version": "1",
    "daemon_id": "tolerance-test-daemon",
    "provider": "glm",
    "descriptor_artifact_hash": "sha256:" + "0" * 64,
    "release_set_hash": "sha256:" + "0" * 64,
    "runtime_version": "0.1.0",
    "heartbeat_ttl_ms": 30000,
}

def call(proc_sql: str) -> subprocess.CompletedProcess:
    wrapped = f"BEGIN; SELECT agent_v1.heartbeat_daemon('{proc_sql}'::jsonb); ROLLBACK;"
    return subprocess.run(
        ["psql", DB_URL, "-v", "ON_ERROR_STOP=0", "-q", "-At", "-c", wrapped],
        capture_output=True, text=True,
    )

def main() -> int:
    failures = []

    legacy = call(json.dumps(BASE_REQUEST))  # mcp_ready 없음
    if legacy.returncode != 0:
        failures.append(f"legacy request (no mcp_ready) was rejected: {legacy.stderr.strip()[:200]}")

    modern = call(json.dumps({**BASE_REQUEST, "mcp_ready": True}))
    if modern.returncode != 0 or "true" not in modern.stdout:
        failures.append(f"modern request (mcp_ready=true) broken: {modern.stderr.strip()[:200]}")

    unready = call(json.dumps({**BASE_REQUEST, "mcp_ready": False}))
    if unready.returncode != 0 or "false" not in unready.stdout:
        failures.append(f"mcp_ready=false not round-tripped: {unready.stderr.strip()[:200]}")

    extra = call(json.dumps({**BASE_REQUEST, "mcp_ready": True, "rogue_field": 1}))
    if extra.returncode == 0:
        failures.append("unknown extra field was accepted (must stay rejected)")

    if failures:
        for f in failures:
            print(f"FAIL: {f}", file=sys.stderr)
        return 1
    print("heartbeat request tolerance contract: PASS")
    return 0

if __name__ == "__main__":
    sys.exit(main())
```

- [ ] **Step 2: 테스트 실행 — legacy 케이스가 실패하는지 확인**

Run: `set -a; source .local/agent-gateway/secrets.env; set +a; python3 scripts/test_heartbeat_request_tolerance.py`
Expected: FAIL — "legacy request (no mcp_ready) was rejected: ... boolean_field_required" (현재 프로시저는 8필드 정확 일치).

- [ ] **Step 3: migration 0023 작성**

`migrations/0023_heartbeat_request_backward_compatibility.sql`:

```sql
-- 0023_heartbeat_request_backward_compatibility.sql
-- Expand half of an expand/contract pair: `mcp_ready` moves from required
-- to optional so a daemon binary built before 0023 can still publish a
-- receipt while a deploy is between its migration stage and its local
-- activation stage. A pre-0023 daemon passed its own boot MCP preflight
-- (main.rs run_mcp_preflight predates this field), so the honest default
-- for an absent field is true; defaulting false would re-create the exact
-- multi-hour outage this migration removes (2026-08 analysis: 24,094
-- fatal Rejected + 441 InvalidResponse daemon exits).
-- Contract (field removal) happens only in a later migration, after every
-- deployed binary sends the field.
BEGIN;

CREATE OR REPLACE FUNCTION agent_v1.heartbeat_daemon(p_request jsonb) RETURNS jsonb
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, agent_store, public
AS $function$
DECLARE
    v_ttl_ms bigint;
    v_expires_at timestamptz;
    v_mcp_ready boolean;
BEGIN
    PERFORM agent_store.assert_request(
        p_request,
        ARRAY['abi_version','daemon_id','provider','descriptor_artifact_hash',
              'release_set_hash','runtime_version','heartbeat_ttl_ms','mcp_ready'],
        ARRAY['abi_version','daemon_id','provider','descriptor_artifact_hash',
              'release_set_hash','runtime_version','heartbeat_ttl_ms']
    );
    PERFORM agent_store.assert_text(p_request, 'daemon_id', 128);
    PERFORM agent_store.assert_text(p_request, 'provider', 16);
    PERFORM agent_store.assert_hash(p_request->>'descriptor_artifact_hash');
    PERFORM agent_store.assert_hash(p_request->>'release_set_hash');
    PERFORM agent_store.assert_text(p_request, 'runtime_version', 128);
    IF p_request->>'provider' NOT IN ('glm', 'deepseek') THEN
        RAISE EXCEPTION USING ERRCODE = 'K1000', MESSAGE = 'invalid_daemon_provider';
    END IF;
    v_ttl_ms := agent_store.request_bigint(p_request, 'heartbeat_ttl_ms', 30000, 120000);
    v_expires_at := clock_timestamp() + v_ttl_ms * interval '1 millisecond';
    v_mcp_ready := CASE WHEN p_request ? 'mcp_ready'
        THEN agent_store.request_boolean(p_request, 'mcp_ready')
        ELSE true
    END;

    INSERT INTO public.agent_v1_daemon_heartbeats(
        daemon_id, provider, descriptor_artifact_hash, release_set_hash,
        runtime_version, mcp_ready, last_seen_at, heartbeat_expires_at, updated_at
    ) VALUES (
        p_request->>'daemon_id', p_request->>'provider',
        p_request->>'descriptor_artifact_hash', p_request->>'release_set_hash',
        p_request->>'runtime_version', v_mcp_ready,
        clock_timestamp(), v_expires_at, clock_timestamp()
    ) ON CONFLICT (daemon_id) DO UPDATE SET
        provider = EXCLUDED.provider,
        descriptor_artifact_hash = EXCLUDED.descriptor_artifact_hash,
        release_set_hash = EXCLUDED.release_set_hash,
        runtime_version = EXCLUDED.runtime_version,
        mcp_ready = EXCLUDED.mcp_ready,
        last_seen_at = EXCLUDED.last_seen_at,
        heartbeat_expires_at = EXCLUDED.heartbeat_expires_at,
        updated_at = EXCLUDED.updated_at;

    RETURN jsonb_build_object(
        'ready', true,
        'mcp_ready', v_mcp_ready,
        'heartbeat_expires_at', v_expires_at
    );
END;
$function$;

COMMIT;
```

- [ ] **Step 4: 로컬 스택에 적용하고 테스트 통과 확인**

Run: `psql "$KRW_AGENT_DATABASE_URL" -v ON_ERROR_STOP=1 -f migrations/0023_heartbeat_request_backward_compatibility.sql && python3 scripts/test_heartbeat_request_tolerance.py`
Expected: PASS 4/4 (legacy 수용, mcp_ready true/false 왕복, unknown 필드 거부 유지).

- [ ] **Step 5: Commit**

```bash
git add migrations/0023_heartbeat_request_backward_compatibility.sql scripts/test_heartbeat_request_tolerance.py
git commit -m "fix(migrations): make the heartbeat request contract backward-compatible"
```

---

### Task 2: 시작 하트비트 — 치명적 exit 대신 bounded 재시도

**Files:**
- Modify: `bins/krw-agentd/src/main.rs:425-445` (initial heartbeat)
- Test: `bins/krw-agentd/src/main.rs` (같은 파일 `#[cfg(test)]` 모듈 — 기존 테스트가 없으면 신규 추가)

**Interfaces:**
- Consumes: `client.execute(&HeartbeatDaemonRequest) -> Result<HeartbeatReceipt, _>` (main.rs:430에서 이미 사용), receipt 필드 `.ready`, `.heartbeat_expires_at`.
- Produces: `pub(crate) async fn publish_initial_heartbeat<F, Fut>(...)` — Task 3가 아니라 이 태스크 내에서만 사용. 재시도 예산 `INITIAL_HEARTBEAT_MAX_ATTEMPTS: u32 = 30`, 지수 백오프 1s→60s.

- [ ] **Step 1: 실패하는 단위 테스트 작성**

main.rs 하단에 테스트 모듈 추가(없으면 생성):

```rust
#[cfg(test)]
mod initial_heartbeat_tests {
    use std::time::Duration;

    struct AttemptPlan {
        failures_before_success: u32,
    }

    async fn plan_result(
        attempt: u32,
        plan: &AttemptPlan,
    ) -> Result<(), String> {
        if attempt <= plan.failures_before_success {
            Err("simulated K-code rejection".to_owned())
        } else {
            Ok(())
        }
    }

    #[tokio::test(started = false)]
    async fn initial_heartbeat_retries_transient_rejections_until_budget() {
        let plan = AttemptPlan { failures_before_success: 3 };
        // 4번째 시도에서 성공해야 함: 0..=3 실패 후 성공.
        let mut attempts = 0u32;
        let outcome = super::retry_with_backoff(
            INITIAL_HEARTBEAT_MAX_ATTEMPTS,
            Duration::from_millis(1),
            Duration::from_millis(2),
            || {
                attempts += 1;
                Box::pin(plan_result(attempts, &plan))
            },
        )
        .await;
        assert!(outcome.is_ok(), "must succeed after transient failures");
        assert_eq!(attempts, 4);
    }

    #[tokio::test(started = false)]
    async fn initial_heartbeat_gives_up_after_the_budget() {
        let mut attempts = 0u32;
        let outcome = super::retry_with_backoff(
            3,
            Duration::from_millis(1),
            Duration::from_millis(2),
            || {
                attempts += 1;
                Box::pin(async { Err::<(), _>("permanent".to_owned()) })
            },
        )
        .await;
        assert!(outcome.is_err(), "must exhaust the budget and fail");
        assert_eq!(attempts, 3);
    }
}
```

- [ ] **Step 2: 테스트 실행 — 컴파일 실패 확인**

Run: `cargo test -p krw-agent-agentd initial_heartbeat`
Expected: FAIL — `retry_with_backoff` / `INITIAL_HEARTBEAT_MAX_ATTEMPTS` 정의 없음.

- [ ] **Step 3: 재시도 헬퍼 + 호출부 구현**

main.rs 상수부(`MCP_PREFLIGHT_DEADLINE` 근처)에:

```rust
/// Bounded retry budget for the startup heartbeat. 30 attempts at
/// 1s doubling to a 60s cap is ~17 minutes — longer than any observed
/// deploy window between the migration stage and local activation, which
/// is exactly the window whose ABI drift used to crash-loop the daemon
/// (24,094 fatal exits in the 2026-08 analysis).
const INITIAL_HEARTBEAT_MAX_ATTEMPTS: u32 = 30;
```

헬퍼 함수(파일 하단, 테스트 모듈 위):

```rust
/// Retry `try_once` with exponential backoff (`first` doubling up to `cap`)
/// for at most `max_attempts`. The sleeper is real; tests shrink the
/// durations so the loop stays fast.
async fn retry_with_backoff<F, Fut, T, E>(
    max_attempts: u32,
    first: std::time::Duration,
    cap: std::time::Duration,
    mut try_once: F,
) -> Result<T, E>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, E>>,
    E: std::fmt::Display,
{
    let mut delay = first;
    for attempt in 1..=max_attempts {
        match try_once().await {
            Ok(value) => return Ok(value),
            Err(error) if attempt == max_attempts => return Err(error),
            Err(error) => {
                warn!(
                    attempt,
                    max_attempts,
                    next_delay_ms = delay.as_millis() as u64,
                    %error,
                    "startup heartbeat attempt failed; retrying with backoff"
                );
                tokio::time::sleep(delay).await;
                delay = delay.saturating_mul(2).min(cap);
            }
        }
    }
    unreachable!("loop returns from both match arms")
}
```

호출부 교체(main.rs:430):

```rust
let initial_daemon_heartbeat = retry_with_backoff(
    INITIAL_HEARTBEAT_MAX_ATTEMPTS,
    std::time::Duration::from_secs(1),
    std::time::Duration::from_secs(60),
    || async { client.execute(&daemon_heartbeat).await },
)
.await?;
if !initial_daemon_heartbeat.ready {
    return Err(StartupError::InitialHeartbeatNotReady.into());
}
```

(기존 `StartupError` 변형인지 확인: `!ready` 분기가 기존에 어떤 에러를 쓰는지 main.rs:431-434를 그대로 유지한다. `?`는 예산 소진 후에만 발생하므로 영구 장애 여전히 fail-closed.)

- [ ] **Step 4: 테스트 통과 확인**

Run: `cargo test -p krw-agent-agentd`
Expected: PASS (신규 2건 포함 전체 녹색).

- [ ] **Step 5: Commit**

```bash
git add bins/krw-agentd/src/main.rs
git commit -m "fix(agentd): retry the startup heartbeat with backoff instead of exiting"
```

---

### Task 3: 클레임 루프 — ABI 응답 드리프트를 재시도형으로 재분류

**Files:**
- Modify: `crates/persistence/src/daemon.rs:692-706` (claim 루프 Err 분기), `crates/persistence/src/daemon.rs:1539-1541` (분류기)
- Test: `crates/persistence/src/daemon.rs` 기존 `#[cfg(test)]` 모듈에 추가

**Interfaces:**
- Consumes: `AgentV1Error::{Database, InvalidResponse, Rejected}`, `jittered_backoff(config.error_backoff_min, config.error_backoff_max, error_attempt, worker_id)` (daemon.rs:693에서 이미 사용).
- Produces: `fn is_retryable_claim_error(&AgentV1Error) -> bool` — `Database` 또는 `InvalidResponse` (procedure 불문). `Rejected`는 여전히 치명적(K-code 계약 위반은 코드 버그이므로 즉시 드러나는 것이 맞다; 유일한 예외는 하트비트이며 Task 2가 이미 담당).

- [ ] **Step 1: 실패하는 분류기 테스트 작성**

daemon.rs 테스트 모듈에 추가:

```rust
#[test]
fn claim_errors_from_response_shape_drift_are_retryable() {
    use super::is_retryable_claim_error;
    use crate::agent_v1::AgentV1Procedure;

    let database = AgentV1Error::Database {
        detail: "connection reset".to_owned(),
    };
    assert!(is_retryable_claim_error(&database));

    let invalid_response = AgentV1Error::InvalidResponse {
        procedure: AgentV1Procedure::ClaimRun,
        reason: "unknown field `mcp_ready`".to_owned(),
    };
    assert!(
        is_retryable_claim_error(&invalid_response),
        "response-shape drift self-heals when the deploy completes; \
         the daemon must stay alive and keep retrying"
    );

    let rejected = AgentV1Error::Rejected {
        kind: crate::agent_v1::RejectionKind::InvalidRequest,
        diagnostic_hash: crate::protocol::ContentHash::from_sha256_hex(
            &"0".repeat(64),
        ),
    };
    assert!(!is_retryable_claim_error(&rejected));
}
```

(실제 `AgentV1Error::Database`/`Rejected` 변형의 필드명은 `daemon.rs:1539` 주변와 `crates/persistence/src/agent_v1.rs`를 확인해 맞춘다 — 테스트가 컴파일되도록 필드만 조정하고 단언은 유지.)

- [ ] **Step 2: 테스트 실행 — 실패 확인**

Run: `cargo test -p krw-agent-persistence claim_errors`
Expected: FAIL — `is_retryable_claim_error` 없음.

- [ ] **Step 3: 분류기 + 루프 분기 구현**

daemon.rs:1539 근처에 추가:

```rust
/// Claim-path retry classification. Beyond transport-level `Database`
/// errors, `InvalidResponse` (response-shape drift after a web-side
/// migration moved a procedure before the local binary caught up) is
/// deliberately retryable: the drift self-heals when the deploy
/// completes, and staying alive preserves metrics/logs instead of the
/// 2-second launchd crash loop observed 24,535 times in the 2026-08
/// analysis. `Rejected` stays fatal — a K-code is a contract violation
/// that retrying cannot fix.
fn is_retryable_claim_error(error: &AgentV1Error) -> bool {
    matches!(error, AgentV1Error::Database { .. })
        || matches!(error, AgentV1Error::InvalidResponse { .. })
}
```

claim 루프(daeon.rs:692-706)의 두 Err 분기를 교체:

```rust
Err(error) if is_retryable_claim_error(&error) => {
    let delay = jittered_backoff(
        self.config.error_backoff_min,
        self.config.error_backoff_max,
        error_attempt,
        &self.config.worker_id,
    );
    error_attempt = error_attempt.saturating_add(1);
    warn!(
        diagnostic_hash = %error_diagnostic_hash(&error),
        attempt = error_attempt,
        "retryable claim failure (database or response-shape drift)"
    );
    wait_for_work_or_shutdown(&mut tasks, &shutdown, delay).await?;
}
Err(error) => return Err(SupervisorError::Claim(error)),
```

- [ ] **Step 4: 크레이트 전체 테스트**

Run: `cargo test -p krw-agent-persistence`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/persistence/src/daemon.rs
git commit -m "fix(persistence): retry claim-loop response-shape drift instead of exiting"
```

---

### Task 4: launchd 재시작 정책 + 로그 타임스탬프

**Files:**
- Modify: `packaging/launchd/com.krw.agentd.plist:11-13`
- Modify: `packaging/launchd/install-local-mac-agentd-release.sh:220-230` (installer 생성 plist heredoc)
- Modify: `bins/krw-agentd/src/main.rs:252-257`
- Create: `scripts/test_launchd_restart_policy.py`

**Interfaces:**
- Consumes: 없음 (독립).
- Produces: 모든 agentd plist가 `KeepAlive=true`(exit 0 포함 항상 재시작) + `ThrottleInterval=10`; agentd 로그에 타임스탬프.

- [ ] **Step 1: 실패하는 정책 테스트 작성**

`scripts/test_launchd_restart_policy.py`:

```python
#!/usr/bin/env python3
"""Assert every agentd plist restarts the daemon on ANY exit and throttles."""
import plistlib
import re
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
EXPECTED_THROTTLE = 10

def check_plist_source(text: str, label: str, errors: list[str]) -> None:
    if "<key>KeepAlive</key><true/>" not in text.replace("\n", ""):
        errors.append(f"{label}: KeepAlive must be <true/> (restart on exit 0 too)")
    match = re.search(r"<key>ThrottleInterval</key><integer>(\d+)</integer>", text)
    if not match or int(match.group(1)) < EXPECTED_THROTTLE:
        errors.append(f"{label}: ThrottleInterval must be >= {EXPECTED_THROTTLE}s")

def main() -> int:
    errors: list[str] = []
    checked_in = (ROOT / "packaging/launchd/com.krw.agentd.plist").read_text()
    check_plist_source(checked_in, "com.krw.agentd.plist", errors)
    installer = (
        ROOT / "packaging/launchd/install-local-mac-agentd-release.sh"
    ).read_text()
    check_plist_source(installer, "agentd installer heredoc", errors)
    # plistlib가 체크인 plist를 실제로 파싱하는지 확인 (형식 오류 방지)
    with tempfile.NamedTemporaryFile(suffix=".plist") as tmp:
        tmp.write(checked_in.encode())
        tmp.flush()
        plistlib.load(open(tmp.name, "rb"))
    if errors:
        for e in errors:
            print(f"FAIL: {e}", file=sys.stderr)
        return 1
    print("launchd restart policy: PASS")
    return 0

if __name__ == "__main__":
    sys.exit(main())
```

- [ ] **Step 2: 테스트 실행 — 실패 확인**

Run: `python3 scripts/test_launchd_restart_policy.py`
Expected: FAIL — 현재 `KeepAlive`가 dict이고 ThrottleInterval 2.

- [ ] **Step 3: plist·인스톨러·로깅 수정**

`packaging/launchd/com.krw.agentd.plist` 11-13행:

```xml
  <key>KeepAlive</key><true/>
  <key>ThrottleInterval</key><integer>10</integer>
```

인스톨러가 생성하는 plist heredoc(`install-local-mac-agentd-release.sh`의 plist 생성 블록)에 동일한 두 줄. 주석 추가:

```bash
# KeepAlive=true: a daemon that exits 0 (graceful drain) must still come
# back — SuccessfulExit:false left the engine dead until the next deploy
# (2026-08 analysis). ThrottleInterval=10 bounds crash-loop churn.
```

`bins/krw-agentd/src/main.rs:256` — `.without_time()` 행 삭제(기본 타임스탬프 활성화):

```rust
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
```

- [ ] **Step 4: 테스트 통과 + 빌드**

Run: `python3 scripts/test_launchd_restart_policy.py && cargo build -p krw-agent-agentd`
Expected: PASS / 빌드 성공.

- [ ] **Step 5: Commit**

```bash
git add packaging/launchd/com.krw.agentd.plist packaging/launchd/install-local-mac-agentd-release.sh bins/krw-agentd/src/main.rs scripts/test_launchd_restart_policy.py
git commit -m "fix(launchd): always-restart keepalive, crash-loop throttle, log timestamps"
```

---

## Phase B — 배포 래칭 해소 (Tasks 5-8, Phase A 이후 독립 배포 가능)

### Task 5: 단발성 원격/활성화 명령에 bounded retry

**Files:**
- Modify: `bins/krw-agent-deploy/src/pipeline.rs:82` 근처(상수), `pipeline.rs`의 stage 실행부(`ctx.command(...)` 호출 지점들)
- Test: `bins/krw-agent-deploy/src/pipeline.rs` 테스트 모듈

**Interfaces:**
- Consumes: `ctx.command_with_retries(executor, command, attempts)` (pipeline.rs:3133, scp가 이미 사용), `SCP_MAX_ATTEMPTS: u32 = 3` (pipeline.rs:82).
- Produces: `pub const COMMAND_RETRY_ATTEMPTS: u32 = 3;` 및 `fn command_retry_attempts(id: &str) -> u32` — 재시도 허용: `ship.`/`remote.`/`readiness.` 접두사 전부 + `activation.agentd-stage`, `activation.agentd-start`, `activation.mcp-gateways-prepare`, `activation.mcp-gateways-activate`, `activation.capabilityd-activate`. 단발 유지: `migrations.*`(db-push 부분 적용 위험), `admission.*`(상태 전환), `activation.agentd-activate`(forward-only 심볼릭 전환), `build.`/`seal.`/`frontend-image.`(로컬 순수).

- [ ] **Step 1: 실패하는 분류기 테스트 작성**

pipeline.rs 테스트 모듈에 추가:

```rust
#[test]
fn remote_and_idempotent_activation_commands_get_bounded_retries() {
    assert_eq!(command_retry_attempts("ship.scp-front-archive"), 3);
    assert_eq!(command_retry_attempts("remote.candidate-abi"), 3);
    assert_eq!(command_retry_attempts("readiness.web-deep"), 3);
    assert_eq!(command_retry_attempts("activation.agentd-start"), 3);
    assert_eq!(command_retry_attempts("activation.capabilityd-activate"), 3);
    assert_eq!(command_retry_attempts("activation.mcp-gateways-prepare"), 3);
    // 단발성 유지 — 상태 전환/마이그레이션/순수 로컬 빌드.
    assert_eq!(command_retry_attempts("migrations.db-push"), 1);
    assert_eq!(command_retry_attempts("admission.close"), 1);
    assert_eq!(command_retry_attempts("admission.open"), 1);
    assert_eq!(command_retry_attempts("activation.agentd-activate"), 1);
    assert_eq!(command_retry_attempts("seal.seal-production-candidate"), 1);
    assert_eq!(command_retry_attempts("build.dual-provider-bundles"), 1);
}
```

- [ ] **Step 2: 테스트 실행 — 실패 확인**

Run: `cargo test -p krw-agent-deploy command_retry_attempts`
Expected: FAIL — 함수 없음.

- [ ] **Step 3: 분류기 구현 + 호출부 라우팅**

pipeline.rs 상수부에:

```rust
/// Bounded retry attempts for deploy commands that are safe to re-run:
/// remote transports (transient SSH/curl failures were 10+ deploy
/// failures in the 2026-08 receipts) and idempotent local activation
/// steps. State transitions (admission.*, migrations.*,
/// activation.agentd-activate) stay single-shot.
pub const COMMAND_RETRY_ATTEMPTS: u32 = 3;

pub fn command_retry_attempts(id: &str) -> u32 {
    const RETRIABLE_IDS: &[&str] = &[
        "activation.agentd-stage",
        "activation.agentd-start",
        "activation.mcp-gateways-prepare",
        "activation.mcp-gateways-activate",
        "activation.capabilityd-activate",
    ];
    if id.starts_with("ship.")
        || id.starts_with("remote.")
        || id.starts_with("readiness.")
        || RETRIABLE_IDS.contains(&id)
    {
        COMMAND_RETRY_ATTEMPTS
    } else {
        1
    }
}
```

`StageCtx`에 헬퍼 추가(`command_with_retries`가 이미 존재하므로 래핑만):

```rust
/// Route a command through the retry policy: retryable ids get
/// [`COMMAND_RETRY_ATTEMPTS`] attempts, everything else stays single-shot.
fn command_with_policy(
    &mut self,
    executor: &dyn StageExecutor,
    mut spec: StageCommand,
) -> Result<String, String> {
    let attempts = command_retry_attempts(&spec.id);
    if attempts > 1 {
        let id = spec.id.clone();
        let stdout = self.command_with_retries(executor, spec, attempts)?;
        Ok(stdout)
    } else {
        let id = spec.id.clone();
        self.command(executor, spec)
    }
}
```

(실제 `command_with_retries`/`command`의 `self` borrowing·반환형은 pipeline.rs:3131-3140과 기존 호출부를 그대로 따른다. `id` 변수는 로그가 필요하면 사용, 아니면 제거 — clippy 경고 없게.) 이후 stage 8/9/10의 `ctx.command(executor, spec)` 호출을 `ctx.command_with_policy(executor, spec)`로 치환(`migrations.*` 실행 지점은 그대로 `ctx.command` 유지 — 정책이 어차피 1을 돌려주므로 전부 치환해도 안전하다; 치환 범위는 리뷰 시 일관성으로 결정).

- [ ] **Step 4: 전체 테스트**

Run: `cargo test -p krw-agent-deploy`
Expected: PASS (기존 12-stage 테스트 포함).

- [ ] **Step 5: Commit**

```bash
git add bins/krw-agent-deploy/src/pipeline.rs
git commit -m "fix(deploy): bounded retries for remote and idempotent activation commands"
```

---

### Task 6: `reconcile` 서브커맨드 — closed 봉인을 전체 재배포 없이 복구

**Files:**
- Create: `bins/krw-agent-deploy/src/reconcile.rs`
- Modify: `bins/krw-agent-deploy/src/main.rs` (서브커맨드 추가)
- Modify: `bins/krw-agent-deploy/src/lib.rs` (모듈 등록)
- Modify: `docs/DUAL_PROVIDER_PRODUCTION_RUNBOOK.md` (복구 절차)
- Test: `bins/krw-agent-deploy/src/reconcile.rs` 테스트 모듈

**Interfaces:**
- Consumes: `payload_open_admission(topology, release_id)` (pipeline.rs:1425, pub), `payload_verify_admission` (pipeline.rs:1474, pub), `build_db_heartbeat_command(deps)` (pipeline.rs:2970, pub, `readiness.` 접두사라 실행기가 타임아웃까지 폴링), `receipt_run_dir(operator_root, run_name)` (receipts.rs:267), `DeployConfig::from_json_bytes` (stages.rs:194 사용), `resolve_remote_topology` (pipeline.rs 테스트가 사용).
- Produces: `pub fn classify_recovery(run_dir: &Path) -> RecoveryPlan` 순수 함수(테스트 가능한 핵심)와 `pub fn run_reconcile(...) -> Result<ReconcileOutcome, String>`.

**설계 결정(핵심):** 복구 가능 조건은 "실패한 배포가 스테이지 9(remote_activation)까지 완료" — 이때만 웹 핀이 이미 신규로 반영되어 있어 admission.open이 안전하다. 스테이지 9 이전 실패(핀 미반영)는 재배포 필요로 명확히 거부한다.

```rust
#[derive(Debug, PartialEq, Eq)]
pub enum RecoveryPlan {
    /// 최근 배포가 성공 — 조치 불필요.
    AlreadyOpen,
    /// 스테이지 9+ 완료 후 실패 — 하트비트 확인 후 admission.open 실행.
    Reopen { run_name: String },
    /// 스테이지 9 이전 실패 또는 진단 불가 — 전체 재배포 필요.
    Blocked { reason: String },
}
```

- [ ] **Step 1: classify_recovery 실패 테스트 작성**

reconcile.rs 테스트 모듈:

```rust
#[cfg(test)]
mod classify_tests {
    use super::{RecoveryPlan, classify_recovery};
    use std::fs;

    fn write_run(dir: &std::path::Path, terminal: Option<&str>, highest_stage: Option<u8>) {
        fs::create_dir_all(dir).unwrap();
        if let Some(body) = terminal {
            fs::write(dir.join("terminal.json"), body).unwrap();
        }
        if let Some(index) = highest_stage {
            fs::write(
                dir.join(format!("stage-{index}-dummy.json")),
                r#"{"status":"success"}"#,
            )
            .unwrap();
        }
    }

    #[test]
    fn success_run_means_already_open() {
        let tmp = tempfile::tempdir().unwrap();
        write_run(
            tmp.path(),
            Some(r#"{"outcome":"success","admission":"open"}"#),
            Some(12),
        );
        assert_eq!(
            classify_recovery(tmp.path()),
            RecoveryPlan::AlreadyOpen
        );
    }

    #[test]
    fn stage10_or_11_failure_is_recoverable() {
        for stage in ["deep_readiness", "admission_open"] {
            let tmp = tempfile::tempdir().unwrap();
            write_run(
                tmp.path(),
                Some(&format!(
                    r#"{{"outcome":"failure","admission":"closed","reached_stage":"{stage}"}}"#
                )),
                Some(10),
            );
            let name = tmp.path().file_name().unwrap().to_string_lossy().into_owned();
            assert_eq!(
                classify_recovery(tmp.path()),
                RecoveryPlan::Reopen { run_name: name },
                "stage {stage} failure must be reopenable"
            );
        }
    }

    #[test]
    fn pre_remote_failure_is_blocked() {
        let tmp = tempfile::tempdir().unwrap();
        write_run(
            tmp.path(),
            Some(r#"{"outcome":"failure","admission":"closed","reached_stage":"local_activation"}"#),
            Some(8),
        );
        assert!(matches!(
            classify_recovery(tmp.path()),
            RecoveryPlan::Blocked { .. }
        ));
    }

    #[test]
    fn missing_terminal_receipt_falls_back_to_stage_receipts() {
        let tmp = tempfile::tempdir().unwrap();
        // Ctrl-C로 중단된 배포: terminal.json 없음, stage-9만 성공.
        write_run(tmp.path(), None, Some(9));
        let name = tmp.path().file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(
            classify_recovery(tmp.path()),
            RecoveryPlan::Reopen { run_name: name }
        );
    }
}
```

(tempfile이 Cargo.toml dev-dependency에 없으면 `bins/krw-agent-deploy/Cargo.toml` `[dev-dependencies]`에 `tempfile = "3"` 추가 — 이 단계에 포함.)

- [ ] **Step 2: 테스트 실행 — 실패 확인**

Run: `cargo test -p krw-agent-deploy classify`
Expected: FAIL — 모듈 없음.

- [ ] **Step 3: classify_recovery 구현**

```rust
use std::path::Path;

#[derive(Debug, PartialEq, Eq)]
pub enum RecoveryPlan {
    AlreadyOpen,
    Reopen { run_name: String },
    Blocked { reason: String },
}

/// Decide how to recover the latest deploy run directory.
///
/// `Reopen` is safe only when the failed run completed remote activation
/// (stage 9): that is the point where the web env pins were updated, so
/// flipping admission open cannot strand the gate on stale pins. Runs that
/// died before stage 9 need a full re-deploy.
pub fn classify_recovery(run_dir: &Path) -> RecoveryPlan {
    let run_name = run_dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let terminal = std::fs::read_to_string(run_dir.join("terminal.json"))
        .ok()
        .and_then(|body| serde_json::from_str::<serde_json::Value>(&body).ok());
    let (outcome, admission, reached) = terminal
        .as_ref()
        .map(|t| {
            (
                t.get("outcome").and_then(|v| v.as_str()).unwrap_or(""),
                t.get("admission").and_then(|v| v.as_str()).unwrap_or(""),
                t.get("reached_stage").and_then(|v| v.as_str()).unwrap_or(""),
            )
        })
        .unwrap_or(("", "", ""));

    if outcome == "success" {
        return RecoveryPlan::AlreadyOpen;
    }
    // terminal.json이 없으면(중단된 실행) 성공한 최고 스테이지 영수증으로 판정.
    let highest_stage = if outcome.is_empty() {
        highest_successful_stage(run_dir)
    } else {
        reached
            .parse::<u8>()
            .ok()
            .and_then(|_| stage_index_from_name(reached))
    };
    match highest_stage {
        Some(index) if index >= 9 => RecoveryPlan::Reopen { run_name },
        Some(_) => RecoveryPlan::Blocked {
            reason: "deploy died before remote activation; web pins are stale; \
                     re-run the full deploy"
                .to_owned(),
        },
        None => RecoveryPlan::Blocked {
            reason: "no terminal receipt and no successful stage receipts; \
                     inspect the run directory manually"
                .to_owned(),
        },
    }
}

fn highest_successful_stage(run_dir: &Path) -> Option<u8> {
    let mut best: Option<u8> = None;
    for entry in std::fs::read_dir(run_dir).ok()?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some((index_part, _)) = name.split_once("stage-") else {
            continue;
        };
        let Ok(index) = index_part.parse::<u8>() else {
            continue;
        };
        let Ok(body) = std::fs::read_to_string(entry.path()) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&body) else {
            continue;
        };
        if value.get("status").and_then(|v| v.as_str()) == Some("success") {
            best = Some(best.map_or(index, |b: u8| b.max(index)));
        }
    }
    best
}

fn stage_index_from_name(name: &str) -> Option<u8> {
    match name {
        "preflight" => Some(1),
        "build" => Some(3),
        "seal" => Some(4),
        "frontend_image_prepare" => Some(5),
        "migrations" => Some(6),
        "admission_close" => Some(7),
        "local_activation" => Some(8),
        "remote_activation" => Some(9),
        "deep_readiness" => Some(10),
        "admission_open" => Some(11),
        "terminal_success_receipt" => Some(12),
        _ => None,
    }
}
```

- [ ] **Step 4: classify 테스트 통과 확인**

Run: `cargo test -p krw-agent-deploy classify`
Expected: PASS 4/4.

- [ ] **Step 5: run_reconcile 오케스트레이션 + CLI 연결**

reconcile.rs에 실행부. payload 실행 래핑 방식은 기존 호출부를 그대로 따른다:

```rust
pub struct ReconcileOutcome {
    pub exit_code: i32,
    pub message: String,
}

/// Recover a closed admission latch without a full re-deploy:
/// heartbeat precheck (polling, readiness-prefixed) then the exact
/// stage-11 payload pair. Reuses the deploy config's operator root and
/// remote topology.
pub fn run_reconcile(
    config: &crate::config::DeployConfig,
    executor: &dyn crate::pipeline::StageExecutor,
    now_unix: u64,
) -> Result<ReconcileOutcome, String> {
    let receipts_root = config.operator_root.join("deploy-receipts");
    let latest = latest_run_dir(&receipts_root)?;
    match classify_recovery(&latest) {
        RecoveryPlan::AlreadyOpen => Ok(ReconcileOutcome {
            exit_code: 0,
            message: "latest deploy succeeded; admission is open; nothing to do".into(),
        }),
        RecoveryPlan::Blocked { reason } => Ok(ReconcileOutcome {
            exit_code: 1,
            message: format!("reconcile blocked: {reason}"),
        }),
        RecoveryPlan::Reopen { run_name } => {
            // 1) db-heartbeat 사전조건 (readiness. 접두사 = 실행기가 타임아웃까지 폴링).
            //    PipelineDeps는 stages::run_command와 동일하게 구성한다.
            // 2) payload_open_admission + payload_verify_admission을 스테이지 11과
            //    동일한 ssh 래핑으로 실행한다(기존 래핑 헬퍼를 재사용 — 구현 시
            //    payload_open_admission의 기존 호출 지점을 복사해 조립).
            // 3) reconcile-<timestamp>-<run_name>.json 영수증을 receipts_root에 기록.
            todo!("wire to the stage-11 execution path; see step instructions")
        }
    }
}

fn latest_run_dir(root: &Path) -> Result<std::path::PathBuf, String> {
    let mut best: Option<std::path::PathBuf> = None;
    for entry in std::fs::read_dir(root).map_err(|e| e.to_string())?.flatten() {
        let path = entry.path();
        if path.is_dir()
            && best
                .as_ref()
                .map_or(true, |b: &std::path::PathBuf| path > *b)
        {
            best = Some(path);
        }
    }
    best.ok_or_else(|| "no deploy receipts found".to_owned())
}
```

구현 순서 지침(실행자가 따를 구체 절차):
1. `grep -n "payload_open_admission" bins/krw-agent-deploy/src/pipeline.rs` — stage 11이 이 payload를 어떤 StageCommand/ssh argv로 감싸는지 그 호출부를 찾는다.
2. 그 래핑 코드를 `run_reconcile`의 `Reopen` 분기로 복사해 `run_name`을 release_id로 전달한다(healthz의 deployment_id 형식 `20260823T004704Z-ffbd47c8` = run 디렉터리명과 동일).
3. `build_db_heartbeat_command`이 요구하는 `PipelineDeps`는 stages.rs의 deploy 경로가 구성하는 것과 동일하게 만들어 전달한다(필드가 많으면 config→deps 구성 함수를 stages.rs에서 export해 재사용).
4. main.rs에 서브커맨드 추가:

```rust
#[derive(Debug, Subcommand)]
enum Command {
    // ... 기존 Preflight, DryRun, Deploy ...
    /// Recover a closed admission latch from the latest failed deploy
    /// (stages 9+) without a full re-deploy.
    Reconcile,
}
```

`Command::Reconcile =>` 분기에서 config를 `std::fs::read`→`DeployConfig::from_json_bytes`로 로드 후 `reconcile::run_reconcile(&config, &RealStageExecutor::new(), now)` 호출, 결과 message 출력.

- [ ] **Step 6: 통합 빌드 + 테스트**

Run: `cargo test -p krw-agent-deploy && cargo build -p krw-agent-deploy`
Expected: PASS, 바이너리 빌드. `./target/debug/krw-agent-deploy --config <config> reconcile --help` 동작(실제 복구 실행은 검증 단계에서만).

- [ ] **Step 7: 러북 문서 갱신**

`docs/DUAL_PROVIDER_PRODUCTION_RUNBOOK.md`의 실패 복구 절차에 추가:

```markdown
### Admission 봉인 복구 (2026-08 이후)

스테이지 9 이후에서 실패한 배포는 전체 재배포 없이 복구한다:

```bash
krw-agent-deploy --config <production-config> reconcile
```

스테이지 9 이전 실패(핀 미반영)는 여전히 전체 재배포가 필요하다 — 명령이
이유와 함께 거부한다.
```

- [ ] **Step 8: Commit**

```bash
git add bins/krw-agent-deploy/src/reconcile.rs bins/krw-agent-deploy/src/main.rs bins/krw-agent-deploy/src/lib.rs bins/krw-agent-deploy/Cargo.toml docs/DUAL_PROVIDER_PRODUCTION_RUNBOOK.md
git commit -m "feat(deploy): reconcile subcommand reopens a latched admission without redeploy"
```

---

### Task 7: 스테이지 10 MCP 프로브 중복 제거 (낮은 우선순위)

**Files:**
- Modify: `bins/krw-agent-deploy/src/pipeline.rs` stage-10 빌더(`build_mcp_probe`/`build_mcp_ready_command` 호출 제거, `stage_deep_readiness`의 엔드포인트 루프 삭제)
- Modify: `bins/krw-agent-deploy/src/pipeline.rs` 테스트 `readiness_layer_commands_carry_the_configured_timeouts`(:5344-5394 — `readiness.mcp-tcp-0`/`readiness.mcp-ready-0` 단언 삭제)
- Test: 동일 파일 테스트 모듈

**Interfaces:**
- Consumes: 없음.
- Produces: 스테이지 10 = `readiness.daemon-metrics` + `readiness.web-deep` + `readiness.db-heartbeat` (db-heartbeat가 mcp_ready=true를 이미 검증). MCP 계약 검사는 부팅 프리플라이트 + 30초 하트비트 + 스테이지 8e `sealed-mcp-abi` 3곳에 남는다(TCP/readyz 2곳 제거로 5→3).

- [ ] **Step 1: 실패하는 테스트 수정**

`readiness_layer_commands_carry_the_configured_timeouts`에서 mcp probe/ready 단언(:5353-5364)을 삭제하는 대신, 새 단언을 추가:

```rust
        let stage10 = build_stage10_commands(&deps);
        assert!(stage10.iter().any(|c| c.id == "readiness.daemon-metrics"));
        assert!(stage10.iter().any(|c| c.id == "readiness.web-deep"));
        assert!(stage10
            .iter()
            .any(|c| c.id == "readiness.db-heartbeat"));
        assert!(
            !stage10.iter().any(|c| c.id.starts_with("readiness.mcp-tcp-")),
            "TCP probes are redundant with the mcp_ready db-heartbeat check"
        );
        assert!(
            !stage10
                .iter()
                .any(|c| c.id.starts_with("readiness.mcp-ready-")),
            "readyz probes are redundant with the boot preflight + heartbeat"
        );
```

(빌더 함수명은 실제 stage-10 조립 함수명으로 맞춘다 — `grep -n "fn stage_deep_readiness" bins/krw-agent-deploy/src/pipeline.rs`.)

- [ ] **Step 2: 테스트 실패 확인**

Run: `cargo test -p krw-agent-deploy readiness_layer`
Expected: FAIL — 프로브가 아직 빌드됨.

- [ ] **Step 3: 프로브 생성 제거**

`stage_deep_readiness`(pipeline.rs:3962~)에서 엔드포인트 루프로 `readiness.mcp-tcp-N`/`readiness.mcp-ready-N`을 만드는 블록 삭제. 모듈 doc 주석(:25)의 스테이지 10 설명 갱신: `readiness.daemon-metrics`, `readiness.web-deep`, `readiness.db-heartbeat`만 남긴다는 취지 + "MCP readiness is proven by the sealed-mcp-abi stage, the daemon boot preflight, and the 30s heartbeat receipt; per-endpoint TCP/readyz probes duplicated those contracts and added deploy-failure surface."

- [ ] **Step 4: 테스트 통과 + 전체**

Run: `cargo test -p krw-agent-deploy`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add bins/krw-agent-deploy/src/pipeline.rs
git commit -m "refactor(deploy): drop redundant stage-10 MCP probes (5x contract check -> 3x)"
```

---

### Task 8: 프리플라이트 dirty 프론트 소스 자동 clean worktree화

**Files:**
- Create: `bins/krw-agent-deploy/src/front_worktree.rs`
- Modify: `bins/krw-agent-deploy/src/main.rs` (deploy/dry-run 경로에서 호출)
- Modify: `bins/krw-agent-deploy/src/lib.rs` (모듈 등록)
- Test: `bins/krw-agent-deploy/src/front_worktree.rs` 테스트 모듈

**Interfaces:**
- Consumes: `config.frontend_source_root` (config.rs:40), git CLI.
- Produces: `pub fn materialize_clean_front_worktree(front_root: &Path, dest_root: &Path) -> Result<PathBuf, String>` — dirty면 `git worktree add --detach <dest_root>/<head>` 후 그 경로 반환, clean이면 원본 경로 그대로 반환. main.rs는 반환 경로로 config의 `frontend_source_root`를 치환한 임시 config JSON을 원본 옆(`.reconciled-<ts>.json` 명명 불필요 — `/tmp`에 `<stem>.worktree.json`)으로 써서 run_command에 넘긴다. 실패 원인 1위(11회)를 제거하면서 "clean tree at HEAD" 계약은 그대로 지킨다(실제 빌드는 언제나 clean HEAD에서).

- [ ] **Step 1: 실패하는 테스트 작성**

front_worktree.rs:

```rust
#[cfg(test)]
mod worktree_tests {
    use super::materialize_clean_front_worktree;
    use std::path::Path;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(["-C", dir.to_str().unwrap()])
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }

    fn make_repo(dir: &Path, dirty: bool) {
        std::fs::create_dir_all(dir).unwrap();
        git(dir, &["init", "-q"]);
        git(dir, &["config", "user.email", "t@t"]);
        git(dir, &["config", "user.name", "t"]);
        std::fs::write(dir.join("f.txt"), "v1").unwrap();
        git(dir, &["add", "."]);
        git(dir, &["commit", "-q", "-m", "1"]);
        if dirty {
            std::fs::write(dir.join("f.txt"), "v2 uncommitted").unwrap();
        }
    }

    #[test]
    fn clean_source_is_returned_untouched() {
        let src = tempfile::tempdir().unwrap();
        let dest = tempfile::tempdir().unwrap();
        make_repo(src.path(), false);
        let resolved =
            materialize_clean_front_worktree(src.path(), dest.path()).unwrap();
        assert_eq!(resolved, src.path());
    }

    #[test]
    fn dirty_source_yields_a_clean_detached_worktree() {
        let src = tempfile::tempdir().unwrap();
        let dest = tempfile::tempdir().unwrap();
        make_repo(src.path(), true);
        let resolved =
            materialize_clean_front_worktree(src.path(), dest.path()).unwrap();
        assert_ne!(resolved, src.path(), "must not build from the dirty tree");
        assert!(resolved.starts_with(dest.path()));
        let porcelain = Command::new("git")
            .args(["-C", resolved.to_str().unwrap(), "status", "--porcelain"])
            .output()
            .unwrap();
        assert!(
            porcelain.stdout.is_empty(),
            "worktree must be clean at HEAD"
        );
    }
}
```

(tempfile dev-dependency는 Task 6에서 이미 추가됨.)

- [ ] **Step 2: 테스트 실패 확인**

Run: `cargo test -p krw-agent-deploy front_worktree`
Expected: FAIL — 모듈 없음.

- [ ] **Step 3: 구현**

```rust
use std::path::{Path, PathBuf};
use std::process::Command;

/// Resolve a clean frontend source for the sealed build.
///
/// The preflight's `frontend-source-clean-commit` check failed 11 times
/// in the 2026-08 receipts — every one blocking recovery of an
/// already-closed admission. When the configured source is dirty we now
/// materialize a detached worktree at its HEAD (the same
/// `/tmp/krw-front-sealed` pattern the operator ran manually) instead of
/// failing. Sealed builds always compile a clean committed tree; the
/// operator's working checkout is never touched.
pub fn materialize_clean_front_worktree(
    front_root: &Path,
    dest_root: &Path,
) -> Result<PathBuf, String> {
    let porcelain = Command::new("git")
        .args(["-C", front_root.to_string_lossy().as_ref(), "status", "--porcelain"])
        .output()
        .map_err(|e| format!("cannot spawn git: {e}"))?;
    if !porcelain.status.success() {
        return Err("git status failed on the frontend source".to_owned());
    }
    if porcelain.stdout.is_empty() {
        return Ok(front_root.to_path_buf());
    }
    let head = Command::new("git")
        .args(["-C", front_root.to_string_lossy().as_ref(), "rev-parse", "HEAD"])
        .output()
        .map_err(|e| format!("cannot spawn git: {e}"))?;
    if !head.status.success() {
        return Err("git rev-parse HEAD failed".to_owned());
    }
    let head = String::from_utf8_lossy(&head.stdout).trim().to_owned();
    let worktree = dest_root.join(format!("front-{head}"));
    if worktree.exists() {
        return Ok(worktree);
    }
    std::fs::create_dir_all(dest_root)
        .map_err(|e| format!("cannot create {dest_root:?}: {e}"))?;
    let status = Command::new("git")
        .args([
            "-C",
            front_root.to_string_lossy().as_ref(),
            "worktree",
            "add",
            "--detach",
            worktree.to_string_lossy().as_ref(),
            &head,
        ])
        .status()
        .map_err(|e| format!("cannot spawn git: {e}"))?;
    if !status.success() {
        return Err(format!("git worktree add failed for HEAD {head}"));
    }
    Ok(worktree)
}
```

main.rs `Deploy`/`DryRun` 경로에 추가(config 로드는 run_command 내부에서 일어나므로, 치환은 config 파일 단계에서: `--config`로 받은 경로를 읽어 `frontend_source_root`만 치환한 사본을 `/tmp`에 쓰고 그 경로를 run_command에 전달):

```rust
fn repoint_config_to_clean_front(config_path: &PathBuf) -> Result<PathBuf, String> {
    let bytes = std::fs::read(config_path).map_err(|e| e.to_string())?;
    let mut value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    let front = value
        .get("frontend_source_root")
        .and_then(|v| v.as_str())
        .ok_or("config missing frontend_source_root")?;
    let operator_root = value
        .get("operator_root")
        .and_then(|v| v.as_str())
        .unwrap_or("/tmp");
    let resolved = krw_agent_deploy::front_worktree::materialize_clean_front_worktree(
        Path::new(front),
        Path::new(operator_root).join("front-worktrees"),
    )?;
    if resolved == Path::new(front) {
        return Ok(config_path.clone());
    }
    value["frontend_source_root"] =
        serde_json::Value::String(resolved.to_string_lossy().into_owned());
    let side = std::env::temp_dir().join(format!(
        "{}.worktree.json",
        config_path.file_stem().unwrap_or_default().to_string_lossy()
    ));
    std::fs::write(&side, serde_json::to_vec_pretty(&value).unwrap())
        .map_err(|e| e.to_string())?;
    eprintln!("frontend source was dirty; using clean worktree: {}", side.display());
    Ok(side)
}
```

(serde_json은 krw-agent-deploy 의존성에 이미 있는지 확인 — `grep serde_json bins/krw-agent-deploy/Cargo.toml`; 없으면 추가. `Deploy | DryRun` 매치 분기에서 `let config = repoint_config_to_clean_front(&cli.config)?;` 후 `&config` 전달. `Preflight`는 읽기 전용이므로 원본 그대로.)

- [ ] **Step 4: 테스트 통과 + 전체**

Run: `cargo test -p krw-agent-deploy`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add bins/krw-agent-deploy/src/front_worktree.rs bins/krw-agent-deploy/src/main.rs bins/krw-agent-deploy/src/lib.rs
git commit -m "feat(deploy): auto-materialize a clean frontend worktree for sealed builds"
```

---

## 검증 (전체 계획 완료 후, 로컬 GLM → 프로덕션 DeepSeek)

1. `cargo test --workspace` 전체 녹색.
2. 로컬 스택(GLM)에서 배포 리허설: `scripts/dev-stack.sh` 기동 후 dry-run + 로컬 타깃 배포 — 실패 주입(예: capabilityd 잠시 정지)으로 (a) 데몬이 exit 대신 백오프 재시도하는지 (b) reconcile이 스테이지 9+ 실패에서 admission을 여는지 확인.
3. 프로덕션(DeepSeek) 배포 후 `readiness.db-heartbeat` 녹색, `krwontology.com/api/healthz` deployment_id 갱신 확인.

## Out of Scope (프론트 저장소 변경 필요 — ~/krw-ontology-front 읽기 전용)

- 웹 admission을 env 플래그 대신 DB 하트비트 파생 상태로(`daemon-heartbeat.ts` + `readiness.ts`).
- 웹 핀 3종을 env 대신 DB release registry 추종으로 전환 (split-brain 근본 제거).
- SSE/아웃박스 워커 재시작 관측성.

이 두 가지가 반영되면 Task 6의 reconcile도 필요 없어지는 것이 종착 상태다(게이트가 자동으로 재개되므로). 그 전까지 Task 6이 운영 공백을 메운다.

## Self-Review 결과

- 래칭 5종 → 태스크 매핑: 래치1(admission)=Task 6, 래치2(재시도)=Task 5, 래치3(ABI)=Task 1+2+3, 래치4(핀)=Task 6 범위(9+ 실패만; 근본은 Out of Scope 명시), 래치5(기동)=Task 2+4. 검사 과잉=Task 7+8. 커버 갭 없음.
- 타입 일관성: `RecoveryPlan`은 Task 6 내에서 정의·사용 일치. `command_retry_attempts`/`COMMAND_RETRY_ATTEMPTS` Task 5 내 일치.
- 주의 표기: Task 3 Step 1의 `AgentV1Error` 변형 필드명, Task 5 Step 3의 `StageCtx` borrowing, Task 6 Step 5의 payload 래핑은 구현 시 실제 시그니처 확인 지침을 단계에 명시했다(추측 코드 방지).
