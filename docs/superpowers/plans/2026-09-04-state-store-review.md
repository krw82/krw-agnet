# 상태 저장소 검토: Redis 교체 vs 제거 (2026-09-04)

소유자 요청: "pgsql → redis로 바꾸는 것도 진행해줘. 데이터베이스 이제 필요 없음.
아예 없애는 것도 검토해줘 (DB가 과설계처럼 보여서 실패지점처럼 보임)."

이 문서는 요청된 검토 결과다. 결론부터: **"실패지점 제거"가 목적이라면 Redis가
아니라 SQLite 임베디드(또는 기존 ephemeral 모드)가 그 목적을 달성한다.**
Redis 교체는 성능 이득이 측정되지 않고 ACID 속성 재구현 비용만 있다.

## 1. 현재 저장소가 실제로 하는 일 (측정)

Postgres는 시장 데이터 소스가 아니라 **런 상태 저장소(컨트롤 플레인)**다.

- 규모: persistence + runtime-persistence 13,117줄, 마이그레이션 24개
- PG 전용 SQL 60곳 (FOR UPDATE, ON CONFLICT, LISTEN/NOTIFY, 트리거, jsonb)
- agentd는 `PostgresJsonExecutor`로 고정 (bins/krw-agentd/src/main.rs:350).
  다만 trait 뒤에 `InMemoryPersistence` 구현이 이미 존재한다(테스트용).
- 20개 `agent_store` 테이블이 담당하는 것:
  1. 런 상태머신 + `fencing_token`/`lease_deadline` (크래시 후 이중 실행 방지)
  2. `run_state_checkpoints` (크래시 재개)
  3. `provider_episodes` (모든 LLM/도구 호출 감사 — 2026-09-04 루트코즈
     분석이 바로 이 테이블로 이뤄졌다)
  4. `session_memory_*` 7테이블 (멀티턴 기억)
  5. `fair_queue_clock`/`principal_fair_queue`/`session_claim_locks` (큐 직렬화, 429)
  6. 트랜잭셔널 아웃박스, 게이트웨이 런 레지스트리, 데몬 하트비트

성능은 저장소가 아니다: 라이브 런에서 턴당 13~145초는 전부 LLM 호출이고
체크포인트 커밋은 수 밀리초다. 어떤 저장소로 바꿔도 런 시간 변화는 0에 수렴한다.

## 2. 옵션 분석

### A. Redis 교체 — 기각 권고
- DB 프로세스를 하나 더 얹는 것뿐: 관리할 데몬/포트/영속성 정책(AOF/RDB)이
  그대로 남는다. "실패지점 제거" 목적과 정반대.
- 펜싱 토큰, 리스, 멱등 커밋(`(run_id, checkpoint_seq)` 유니크), FK 감사 체인,
  트랜잭셔널 아웃박스를 Redis 원시 구조로 재구현해야 한다 (MULTI는 롤백 없음,
  락은 Redlock류 필요). 13k줄 재작성 + 전수 재검증.
- 얻는 것: 측정된 성능 이득 0.

### B. SQLite 임베디드 — "프로세스를 없애기"가 목적이면 권고
- postgres 데몬/포트/TLS/initdb/마이그레이션 절차가 사라지고 상태는 파일 하나.
  진짜 실패지점(별도 프로세스) 제거.
- SQL 호환 포팅 범위가 정해져 있다: jsonb→TEXT(JSON), timestamptz→TEXT,
  ON CONFLICT 대부분 호환, FOR UPDATE→BEGIN IMMEDIATE 트랜잭션,
  LISTEN/NOTIFY→폴링 또는 제거, 트리거 2건→애플리케이션 계층 이동.
- ACID/멱등 유지되므로 executor 교체 + 마이그레이션 포팅으로 끝난다.
- 제약: 단일 쓰기 프로세스(WAL로 읽기 병행 가능) — 현재 단일 워커 게이트웨이
  구조와 정합. 멀티 워커 확장 시점에 재검토.

### C. 인메모리 (아예 없애기) — 부분 가능하나 비추천
- `InMemoryPersistence`가 이미 있으므로 agentd에 선택지를 여는 것은 작업이
  작다. 하지만 잃는 것이 크다: 크래시 재개, 감사 에피소드(이번 사고 분석의
  도구), 세션 메모리 영속화, 런 이력. 감사 능력 상실의 비용은 오늘 당일에
  입증되었다.
- 중간지점은 이미 존재한다: `KRW_AGENT_POSTGRES_LIFECYCLE=ephemeral`
  (부팅 시 1회성 DB, 종료 시 폐기 — 운영 부담 제거, 런 중 감사 유지).

## 3. 권고 경로

1. **지금 당장(변경 없음)**: ephemeral lifecycle 사용 — 개발 루프의 DB 운영
   부담을 이미 제거할 수 있다.
2. **단기 작업(권장)**: SQLite 임베디드 executor — 프로세스 실패지점 제거 +
   ACID 유지. 예상 규모: `SqliteJsonExecutor` 신규 + 24 마이그레이션 포팅 +
   스택 스크립트 정리 + 오프라인 테스트. 별도 스펙/브랜치로 진행.
3. **Redis**: 기각. 성능 이득 없음 + 재구현 비용만. (이미 운영 중인 Redis를
   다른 용도로 쓰는 등 별도 운영 이유가 생기면 재론.)

## 4. 결정 필요

- B(SQLite 임베디드)로 진행할지, A(Redis)를 그럼에도 진행할지,
- C(인메모리 모드 노출)를 옵션으로 추가할지.

E2E(자유 문 하드닝)는 Postgres 기반으로 계속 진행되며 이 결정과 독립적이다.
