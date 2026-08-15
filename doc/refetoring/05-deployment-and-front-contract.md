# 배포 및 frontend 계약

## 최종 명령

사용자는 frontend에서 기존 명령을 유지한다.

```bash
npm run prod:deploy:full
```

이 명령은 얇은 adapter이며 agent repository의 단일 controller를 호출한다.

```text
krw-agent-deploy --config <absolute production config>
```

provider는 config에 필수다. default나 newest candidate 선택은 없다.

## controller 단계

```text
1. read-only preflight
2. immutable preflight receipt
3. exact source commit build
4. seal one selected runtime release set
5. frontend image prepare
6. forward-only migrations
7. admission close
8. local capability + gateway + daemon activation
9. remote web + outbox activation
10. bounded deep readiness
11. admission open
12. terminal success receipt
```

실패하면 admission을 닫고 terminal failure receipt를 남긴다. 과거 binary나 DB schema로 되돌리지 않는다.

## read-only preflight

build보다 먼저 다음을 확인한다.

- agent/frontend clean commit
- config schema와 absolute paths
- explicit provider/model/registry
- signing key/trust validity
- output directory 미존재
- required commands와 toolchain
- GCP project/instance/zone/instance id
- SSH와 remote disk
- Docker/Compose
- remote env의 required key 존재 여부
- Supabase target/TLS/migration plan
- current DB Agent V1 ABI
- local MCP endpoint/Origin/CA/readiness
- port ownership
- frontend deployment contract hash

preflight는 파일, env, gateway, launchd, DB, remote host를 수정하지 않는다.

receipt는 모든 입력 hash와 target identity를 기록한다. build/activate 단계는 receipt 이후 drift를 거부한다.

## production config

```json
{
  "schema_version": 1,
  "provider": "deepseek",
  "agent_source_root": "/Users/.../krw-agnet",
  "frontend_source_root": "/Users/.../krw-ontology-front",
  "operator_root": "/Users/.../krw-agnet-prod",
  "runtime_env": "/Users/.../runtime/krw-agent-deploy.env",
  "target_file": "/Users/.../ops/production-target.json",
  "frontend_contract": "/Users/.../ops/agent-v1-deployment-contract.json",
  "timeouts": {
    "ssh_ms": 5000,
    "mcp_ms": 5000,
    "daemon_ready_ms": 90000,
    "public_ready_ms": 90000
  }
}
```

secret 값은 config에 넣지 않고 env variable handle만 release descriptor에서 참조한다.

## frontend deployment contract

```json
{
  "schema_version": 1,
  "agent_abi": "agent_v1_v7",
  "required_procedures": ["agent_v1.enqueue_run(jsonb)"],
  "required_columns": ["agent_v1_daemon_heartbeats.mcp_ready"],
  "projection_contract": "agent-final-projection/v1",
  "limits_contract": "runtime-limits/v1",
  "host_package_contract": "krw-agent-host/v1"
}
```

실제 목록은 generated canonical JSON으로 관리한다. agent build는 이 한 파일 hash만 검증한다.

## migration ordering

1. migration plan을 read-only로 확인한다.
2. 새 release의 DB ABI checker가 pending migration과 함께 봉인된다.
3. admission을 닫기 전까지 migration은 적용하지 않는다.
4. migration 적용 직후 새 checker로 exact ABI를 확인한다.
5. 이후 old daemon activation 경로는 없다.
6. activation 실패 시 admission closed + fix-forward receipt다.

## readiness

readiness는 세 층이다.

- process: daemon HTTP endpoint가 응답
- dependency: 모든 remote MCP endpoint의 exact Origin/TLS/build/schema/data pin
- product: DB heartbeat의 provider/descriptor/release/mcp_ready와 web projection

MCP readiness는 LLM을 호출하지 않는다. chart/presentation readiness는 optional이며 research admission을 막지 않는다.

## artifact cleanup

- 성공 release N개만 보존한다.
- failed build stage는 receipt를 남기고 bounded cleanup한다.
- `.deploy` phase graph와 rollback image는 제거한다.
- remote Docker prune은 exact project labels와 age threshold를 사용한다.
- broad recursive delete나 unresolved glob은 금지한다.

