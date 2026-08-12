# Dual-provider cutover file inventory

이 문서는 dirty developer worktree에서 release commit을 만들 때 포함할 범위를
고정한다. `krw-ontology`는 read-only external authority이며 이 inventory에 포함하지
않는다. 아래 목록 밖의 변경은 별도 기능 release로 분리하고 이번 research cutover
commit에 넣지 않는다.

## krw-agent에 포함

```text
agents/fixtures/entrypoints.tsv
agents/krw-ontology/**
agents/krw-guru-advisor/**
contracts/release-evidence/README.md
crates/research-planner/src/initial_plan.rs
crates/run-engine/src/lib.rs
crates/runtime-persistence/tests/live_provider_episode_audit.rs
deployments/local/**
deployments/prod/**
docs/DUAL_PROVIDER_PRODUCTION_RUNBOOK.md
docs/IMPLEMENTATION_STATUS.md
docs/LOCAL_FEEDBACK_LOOP.md
docs/PERFORMANCE_RELEASE_GATES.md
docs/POSTGRES_RUNTIME.md
docs/RELEASE_AUTHORIZATION.md
fixtures/live-quality/v1/dual-provider-production-v1.json
migrations/**
packaging/**
scripts/apply_migrations.sh
scripts/build_dual_provider_release.sh
scripts/build_standalone_release.sh
scripts/check_active_budget_parity.py
scripts/collect_release_evidence.py
scripts/dev-stack.sh
scripts/finalize_dual_provider_release.sh
scripts/package-host-ts.mjs
scripts/prepare_production_candidate.sh
scripts/release_provider.py
scripts/run_dual_provider_acceptance.py
scripts/run_live_quality_matrix.py
scripts/run_session_followup_quality.py
scripts/seal_production_candidate.sh
scripts/start_local_agent_gateway_stack.sh
scripts/test_collect_release_evidence.py
scripts/test_dual_provider_acceptance.py
scripts/test_dual_provider_release.py
scripts/test_production_evidence_index.py
scripts/test_standalone_release_manifest.py
scripts/verify_standalone_release.py
scripts/write_dual_provider_evidence_index.py
scripts/write_release_manifest.py
scripts/with_local_env.sh
fixtures/live-quality/v1/session-followup-v1.json
```

## krw-ontology-front에 포함

```text
.env.example
Dockerfile.web
docker-compose.yml
docs/RESEARCH_ENGINE_CUTOVER.md
scripts/apply-agent-v1-production-migrations.sh
scripts/deploy-production-fast.sh
scripts/dev-with-research-engine.mjs
scripts/sync-krw-agent-host-package.mjs
scripts/verify-agent-v1-compatibility.mjs
scripts/verify-krw-agent-host-package.mjs
src/app/api/chat/run/route.ts
src/app/api/chat/run/route.test.ts
src/app/api/healthz/deep/route.ts
src/lib/agent-v1/**
src/lib/agent/runner.ts
src/lib/postgres/listen-config.ts
src/lib/question-router/chat-question-router.ts
src/lib/question-router/chat-question-router.test.ts
src/lib/run-kind.ts
src/types/api-contracts.ts
src/worker/agent-v1-outbox-worker.ts
supabase/migrations/20260812090000_agent_v1_contract_v7_compatibility.sql
supabase/migrations/20260812090150_agent_v1_runtime_schema_compatibility.sql
supabase/migrations/20260812090400_agent_v1_commit_final_memory_v7_compatibility.sql
supabase/migrations/20260812100000_agent_v1_research_run_kinds.sql
vendor/krw-agent-host/krw-agent-host-0.1.0.tgz
vendor/krw-agent-host/provenance.json
```

## 의도적으로 제외

```text
~/krw-ontology/**
billing/refund/valuation/news/notification 기능 변경
스크린샷, .tmp, .lazyweb, .playwright-mcp, node_modules, target
개발자 개인 secret, DB URL, provider API key, signing private key
```

`git archive <accepted-commit>`로 front와 agent를 각각 생성하고, release builder는
그 archive에서만 실행한다. `git add .`나 dirty directory tar는 사용하지 않는다.
