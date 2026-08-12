# Dual-Provider Production Cutover Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** GLM과 DeepSeek 각각에 대해 독립적으로 검증·서명·배포 가능한 `krw-agent` 산출물을 만들고, DeepSeek를 실제 서비스 lane으로 사용하면서 `krw-ontology-front`의 채팅방·후속질문·outbox 흐름을 중단 없이 안전하게 전환한다.

**Architecture:** Rust binary와 AgentImage는 한 번만 컴파일하고, provider별 model registry·public descriptor·release authorization·manifest를 분리해 `glm`과 `deepseek` 두 개의 immutable bundle을 만든다. 한 제품 queue에는 한 provider만 새 실행을 받으며, provider 전환은 `closed → drain → verify → switch → open`의 한 명령으로 수행한다. 제품 DB와 채팅방은 front가 소유하고, `krw-agentd`는 동일한 `tenant_id + principal_id + session_id + run_id`로 실행·checkpoint·세션 컨텍스트만 소유한다.

**Tech Stack:** Rust 1.97.1, Tokio, PostgreSQL/Supabase, TypeScript/Node.js 20, Next.js, Docker Compose, systemd/launchd, Anthropic Messages API-compatible GLM-5.2 and DeepSeek Flash endpoints.

## Implementation status (2026-08-12)

The provider-exact manifest, one-compile dual candidate builder, prepare/seal/finalize
scripts, provider-aware quality runner, same-session follow-up check, front readiness/TLS/
closed-admission wiring, wide-research route, budget parity check, and production runbook
are implemented in the two mutable repositories. The remaining gate is intentionally
operator-owned: clean committed source, real binding/endpoint/authorization files, live
GLM and DeepSeek acceptance, DB migration against the target, and the actual production
symlink/canary. No live provider call or production activation is claimed by the local
implementation checks.

## Global Constraints

- 수정 가능한 저장소는 `~/krw-agnet`와 `~/krw-ontology-front`뿐이다.
- `~/krw-ontology`의 코드, 스키마, 릴리스 데이터와 MCP 입출력 계약은 변경하지 않는다.
- GLM과 DeepSeek 모두 같은 AgentImage, workflow, skill, capability 허용 목록과 품질 gate를 사용한다.
- 모델 호출 횟수, 추론 수준, capability 수, tool budget을 성능 개선 명목으로 줄이지 않는다.
- provider별 물리 모델은 `glm-5.2`와 `deepseek-v4-flash`만 허용한다. 임의 모델 문자열이나 자동 fallback은 허용하지 않는다.
- 실행 중 provider hot-swap은 하지 않는다. provider 전환은 drain 후 bounded restart로만 수행한다.
- GLM과 DeepSeek daemon을 동일 queue에 동시에 열어 두지 않는다.
- Claude Agent SDK나 기존 agent worker로 조용히 fallback하지 않는다.
- 장애·점검·enqueue 실패·terminal 실패에서도 assistant 메시지는 반드시 terminal 상태로 남긴다. 사용자를 무기한 `pending`에 두지 않는다.
- 채팅 후속질문은 선택된 정확한 채팅방 `session_id` 안에서만 이어진다. 회원 전체 메모리나 다른 채팅방 자동 검색을 추가하지 않는다.
- DB migration은 forward-only additive 방식으로 적용한다. rollback은 이전 compatible binary/web image로 수행하며 destructive down migration은 사용하지 않는다.
- production 배포는 clean committed tree와 exact Git SHA에서만 생성한다. 현재 dirty working tree를 직접 tar로 배포하지 않는다.
- secret은 환경변수나 secret manager로만 주입한다. API key, DB URL, signing private key를 manifest, descriptor, log, repository에 넣지 않는다.

---

## Target Release Layout

한 release ID는 다음 두 provider bundle을 갖는다. 두 bundle은 독립적으로 offline verification이 가능해야 한다.

```text
/opt/krw-agent/releases/${KRW_RELEASE_ID}/
├── glm/
│   ├── bin/krw-agent
│   ├── bin/krw-agentd
│   ├── images/
│   ├── migrations/
│   ├── deployments/model-registry.yaml
│   ├── deployments/budget-registry.yaml
│   ├── deployments/deployment-binding.yaml
│   ├── deployments/endpoint-registry.yaml
│   ├── public-release.json
│   ├── release-authorization.json
│   ├── release-trust-registry.json
│   ├── frontend-runtime.env
│   └── release-manifest.json
├── deepseek/
│   ├── bin/krw-agent
│   ├── bin/krw-agentd
│   ├── images/
│   ├── migrations/
│   ├── deployments/model-registry.yaml
│   ├── deployments/budget-registry.yaml
│   ├── deployments/deployment-binding.yaml
│   ├── deployments/endpoint-registry.yaml
│   ├── public-release.json
│   ├── release-authorization.json
│   ├── release-trust-registry.json
│   ├── frontend-runtime.env
│   └── release-manifest.json
└── dual-release-index.json
```

Provider mapping is closed and exact:

| Provider | Physical model | Registry source | Secret env | Intended lane |
|---|---|---|---|---|
| `glm` | `glm-5.2` | `deployments/prod/model-registry.glm.yaml` | `GLM_API_KEY` | staging/quality/canary |
| `deepseek` | `deepseek-v4-flash` | `deployments/prod/model-registry.deepseek.yaml` | `DEEPSEEK_API_KEY` | production service |

`required_model_profile`의 현재 logical ID인 `glm_high`, `glm_max`, `glm_direct`는 이번 전환에서 이름을 바꾸지 않는다. 두 registry가 같은 logical profile을 각 물리 모델에 매핑한다. 지금 rename하면 모든 AgentImage와 fixture를 건드려 실패 지점만 늘어난다.

---

### Task 1: Freeze and Separate the Two Repository Change Sets

**Files:**
- Create: `~/krw-agnet/docs/release/dual-provider-cutover-file-inventory.md`
- Inspect only: `~/krw-ontology`
- Stage selectively: only files explicitly accepted by later tasks

**Interfaces:**
- Produces: one clean `krw-agent` source commit and one clean `krw-ontology-front` source commit.
- Produces: an inventory mapping every included file to `research-runtime`, `product-host`, `database`, or `deployment`.

- [ ] **Step 1: Record immutable baselines without changing any repository**

```bash
git -C ~/krw-agnet rev-parse HEAD
git -C ~/krw-ontology-front rev-parse HEAD
git -C ~/krw-ontology rev-parse HEAD
git -C ~/krw-agnet status --short
git -C ~/krw-ontology-front status --short
git -C ~/krw-ontology status --short
```

Expected: all three SHAs and dirty-file inventories are captured; no cleanup command is run against `krw-ontology`.

- [ ] **Step 2: Write the explicit included-file inventory**

The inventory must list these research cutover paths and exclude unrelated billing, refund, news, notification, valuation, screenshots, `.tmp`, `.lazyweb`, and `.playwright-mcp` files:

```text
krw-agnet:
  agents/krw-ontology/**
  crates/research-planner/src/initial_plan.rs
  crates/run-engine/src/lib.rs
  crates/runtime-persistence/tests/live_provider_episode_audit.rs
  deployments/{local,prod}/**
  migrations/**
  packaging/**
  scripts/{apply_migrations,build_standalone_release,write_release_manifest,verify_standalone_release}.*

krw-ontology-front:
  src/app/api/chat/run/**
  src/lib/agent-v1/**
  src/worker/agent-v1-outbox-worker.ts
  src/hooks/useChat.ts
  Dockerfile.web
  docker-compose.yml
  scripts/verify-agent-v1-compatibility.mjs
  scripts/deploy-production-fast.sh
  supabase/migrations/20260812090000_agent_v1_contract_v7_compatibility.sql
  supabase/migrations/20260812090150_agent_v1_runtime_schema_compatibility.sql
  supabase/migrations/20260812090400_agent_v1_commit_final_memory_v7_compatibility.sql
```

- [ ] **Step 3: Review every selected diff before staging**

```bash
git -C ~/krw-agnet diff -- \
  agents/krw-ontology \
  crates/research-planner/src/initial_plan.rs \
  crates/run-engine/src/lib.rs \
  crates/runtime-persistence/tests/live_provider_episode_audit.rs \
  deployments/local deployments/prod migrations packaging scripts
git -C ~/krw-ontology-front diff -- \
  src/app/api/chat/run src/lib/agent-v1 \
  src/worker/agent-v1-outbox-worker.ts src/hooks/useChat.ts \
  Dockerfile.web docker-compose.yml scripts/verify-agent-v1-compatibility.mjs \
  scripts/deploy-production-fast.sh supabase/migrations/2026081209*.sql
```

Expected: no ontology source edit, unrelated product feature, secret, generated screenshot, or local runtime state appears.

- [ ] **Step 4: Commit only independently passing logical groups**

Use explicit paths with `git add`; never use `git add .` in either dirty repository. Commit groups are:

```text
krw-agent:
  1. research workflow/prompt behavior
  2. runtime/provider behavior
  3. release/deployment tooling

krw-ontology-front:
  1. product host/outbox/session behavior
  2. Agent V1 database compatibility
  3. production container/deployment wiring
```

- [ ] **Step 5: Create clean release worktrees from the accepted commits**

```bash
git -C ~/krw-agnet worktree add \
  ~/krw-agent-release codex/dual-provider-production
git -C ~/krw-ontology-front worktree add \
  ~/krw-front-release codex/dual-provider-production
```

Expected: both release worktrees report an empty `git status --porcelain`; the original dirty worktrees remain untouched.

---

### Task 2: Define a Provider-Exact Release Manifest Contract

**Files:**
- Modify: `~/krw-agnet/scripts/write_release_manifest.py`
- Modify: `~/krw-agnet/scripts/verify_standalone_release.py`
- Modify: `~/krw-agnet/scripts/test_standalone_release_manifest.py`
- Create: `~/krw-agnet/scripts/test_dual_provider_release.py`

**Interfaces:**
- Produces: `provider_id: "glm" | "deepseek"` in `krw-standalone-release/v2`.
- Produces: exactly one `physical_models` value per bundle.
- Rejects: mixed-model bundles, unknown providers, provider/model mismatch, extra files and symlinks.

- [ ] **Step 1: Add failing provider/model contract tests**

```python
VALID_PROVIDER_MODELS = {
    "glm": "glm-5.2",
    "deepseek": "deepseek-v4-flash",
}

def test_glm_manifest_has_exact_physical_model():
    manifest = write_fixture_manifest(provider="glm", model="glm-5.2")
    assert manifest["provider_id"] == "glm"
    assert manifest["physical_models"] == ["glm-5.2"]

def test_deepseek_manifest_has_exact_physical_model():
    manifest = write_fixture_manifest(provider="deepseek", model="deepseek-v4-flash")
    assert manifest["provider_id"] == "deepseek"
    assert manifest["physical_models"] == ["deepseek-v4-flash"]

def test_provider_model_mismatch_is_rejected():
    with pytest.raises(ValueError):
        verify_fixture_manifest(provider="deepseek", model="glm-5.2")
```

- [ ] **Step 2: Run the tests and observe the current GLM-only rejection**

```bash
python3 scripts/test_standalone_release_manifest.py
python3 scripts/test_dual_provider_release.py
```

Expected: DeepSeek cases fail because the current writer/verifier hard-code `glm-5.2`.

- [ ] **Step 3: Implement the closed mapping**

`write_release_manifest.py` must accept `--provider` and derive the model internally. Remove the free-form `--model` authority.

```python
PROVIDER_MODELS = {
    "glm": "glm-5.2",
    "deepseek": "deepseek-v4-flash",
}
```

The verifier reads `provider_id`, resolves the exact expected model from the same constant, and requires `physical_models == [expected_model]`.

- [ ] **Step 4: Bind the manifest to source and runtime artifacts**

Keep `git_commit`, `git_tree`, per-file hash/size, manifest hash, no-symlink and no-extra-file checks. Add exact hashes for:

```text
deployments/model-registry.yaml
deployments/budget-registry.yaml
public-release.json
release-authorization.json
```

- [ ] **Step 5: Run both manifest suites**

```bash
python3 scripts/test_standalone_release_manifest.py
python3 scripts/test_dual_provider_release.py
```

Expected: GLM and DeepSeek valid fixtures pass; swapped registry, model, descriptor or authorization fixtures fail.

- [ ] **Step 6: Commit**

```bash
git add scripts/write_release_manifest.py scripts/verify_standalone_release.py \
  scripts/test_standalone_release_manifest.py scripts/test_dual_provider_release.py
git commit -m "feat(release): verify exact GLM and DeepSeek bundles"
```

---

### Task 3: Build Both Provider Bundles from One Compilation

**Files:**
- Modify: `~/krw-agnet/scripts/build_standalone_release.sh`
- Create: `~/krw-agnet/scripts/build_dual_provider_release.sh`
- Test: `~/krw-agnet/scripts/test_dual_provider_release.py`

**Interfaces:**
- Produces: `build_standalone_release.sh --provider glm|deepseek --from-common DIR --output DIR`.
- Produces: `build_dual_provider_release.sh --output-root ABSOLUTE_NEW_DIR`.
- Guarantees: binaries and AgentImages are built once; provider-specific registries and unsigned inventories remain separate until Task 4 seals them.
- Produces unsigned staging bundles only. Task 4 adds resolved deployment files, descriptor and authorization, then writes the final `release-manifest.json`.

- [ ] **Step 1: Add a shell-interface test**

Assert all of these cases without making live provider calls:

```text
--provider glm       -> model-registry.glm.yaml copied as model-registry.yaml
--provider deepseek  -> model-registry.deepseek.yaml copied as model-registry.yaml
--provider other     -> exit 2
dirty source tree    -> exit 1
existing output path -> exit 2
```

- [ ] **Step 2: Replace local deployment templates in release bundles**

The bundle builder must no longer run:

```bash
install -m 0644 deployments/local/*.yaml "$release/deployments/"
```

It must install:

```bash
install -m 0644 deployments/prod/budget-registry.yaml \
  "$release/deployments/budget-registry.yaml"
install -m 0644 "deployments/prod/model-registry.$provider.yaml" \
  "$release/deployments/model-registry.yaml"
```

Resolved binding and endpoint files are added by Task 4; `REPLACE_WITH_*` templates are never admitted into the final sealed candidate.

- [ ] **Step 3: Build common artifacts once**

`build_dual_provider_release.sh` performs exactly one production compilation and one image build pass:

```bash
cargo build --locked --profile production -p krw-agent -p krw-agentd
while IFS= read -r krw_agent_package; do
  target/production/krw-agent image build \
    "agents/$krw_agent_package" --out "$common/images/$krw_agent_package"
  target/production/krw-agent image verify \
    "$common/images/$krw_agent_package"
done < <(awk -F '\t' '!/^#/ && NF { print $1 }' agents/fixtures/entrypoints.tsv | LC_ALL=C sort -u)
```

Then it materializes two complete regular-file staging bundles from the common directory. Do not use symlinks inside either bundle. It writes `build-inventory.json` with the source commit/tree and common binary/image hashes; it does not claim that the unsigned staging directory is a deployable release.

- [ ] **Step 4: Write the unsigned build inventory**

`build-inventory.json` must contain only:

```json
{
  "schema_version": 1,
  "git_commit": "40-or-64-character-lowercase-git-object-id",
  "git_tree": "40-or-64-character-lowercase-git-object-id",
  "bundles": {
    "glm": {"physical_model": "glm-5.2", "sealed": false},
    "deepseek": {"physical_model": "deepseek-v4-flash", "sealed": false}
  }
}
```

- [ ] **Step 5: Verify both staging bundles**

```bash
target/production/krw-agent image verify "$output_root/glm/images/krw-ontology"
target/production/krw-agent image verify "$output_root/deepseek/images/krw-ontology"
python3 scripts/test_dual_provider_release.py \
  --staging-root "$output_root" --expect-sealed false
```

- [ ] **Step 6: Commit**

```bash
git add scripts/build_standalone_release.sh scripts/build_dual_provider_release.sh \
  scripts/test_dual_provider_release.py
git commit -m "feat(release): build GLM and DeepSeek artifacts together"
```

---

### Task 4: Assemble and Sign Real Environment-Specific Candidates

**Files:**
- Create: `~/krw-agnet/scripts/prepare_production_candidate.sh`
- Create: `~/krw-agnet/scripts/seal_production_candidate.sh`
- Modify: `~/krw-agnet/deployments/prod/README.md`
- Modify: `~/krw-agnet/docs/RELEASE_AUTHORIZATION.md`
- Test: `~/krw-agnet/scripts/test_dual_provider_release.py`

**Interfaces:**
- `prepare_production_candidate.sh --provider P --bundle DIR --binding FILE --endpoints FILE --descriptor-output FILE`.
- `seal_production_candidate.sh --provider P --candidate DIR --authorization FILE --trust-registry FILE`.
- Both commands reject placeholders, fixture fingerprints, zero hashes, non-HTTPS endpoints and provider mismatch.
- Produces final provider manifests and `/opt/krw-agent/releases/${KRW_RELEASE_ID}/dual-release-index.json` only after both candidates are sealed.

- [ ] **Step 1: Add negative fixtures**

Test exact rejection for:

```text
REPLACE_WITH_* in any resolved YAML
server_build: fixture
sha256:000...000
http:// non-loopback production endpoint
DeepSeek registry with --provider glm
authorization physical model mismatch
expired/revoked/low-sequence authorization
```

- [ ] **Step 2: Prepare each public descriptor without claiming work**

Define the complete immutable image argument array once, then use it for both providers:

```bash
krw_image_args=(
  --image-dir images/krw-ontology
  --image-dir images/krw-ontology-en
  --image-dir images/krw-feed
  --image-dir images/krw-source-filing
  --image-dir images/krw-guru-advisor
  --image-dir images/krw-router
  --image-dir images/krw-display
  --image-dir images/krw-notebook
)
bin/krw-agentd "${krw_image_args[@]}" \
  --deployment-binding deployments/deployment-binding.yaml \
  --model-registry deployments/model-registry.yaml \
  --provider glm \
  --budget-registry deployments/budget-registry.yaml \
  --endpoint-registry deployments/endpoint-registry.yaml \
  --public-release-descriptor-output public-release.json \
  --check
```

DeepSeek uses the identical command with `--provider deepseek`. No provider key is required for `--check`.

- [ ] **Step 3: Sign each descriptor independently**

On the signing host, run `krw-agent release sign` twice. Each authorization must bind the exact descriptor, runtime/kernel version, provider physical model and monotonic sequence. The signing private key never enters the candidate directory.

- [ ] **Step 4: Seal and verify the candidates**

`seal_production_candidate.sh` copies the public trust registry and authorization, runs:

```bash
bin/krw-agent release verify \
  --descriptor public-release.json \
  --authorization release-authorization.json \
  --trust-registry release-trust-registry.json
bin/krw-agentd "${krw_image_args[@]}" \
  --deployment-binding deployments/deployment-binding.yaml \
  --model-registry deployments/model-registry.yaml \
  --provider "$KRW_AGENT_PROVIDER" \
  --budget-registry deployments/budget-registry.yaml \
  --endpoint-registry deployments/endpoint-registry.yaml \
  --release-authorization release-authorization.json \
  --release-trust-registry release-trust-registry.json \
  --check
python3 packaging/verify_standalone_release.py --root "$candidate"
```

After both provider candidates pass, write `dual-release-index.json` with the exact Git commit/tree and the two final manifest hashes. Also emit a non-secret `frontend-runtime.env` per provider containing `KRW_AGENT_PROVIDER`, `KRW_AGENT_RELEASE_ARTIFACT_HASH`, and `KRW_AGENT_RELEASE_SET_HASH`; the deployer supplies the mounted descriptor path.

- [ ] **Step 5: Commit**

```bash
git add scripts/prepare_production_candidate.sh scripts/seal_production_candidate.sh \
  scripts/test_dual_provider_release.py deployments/prod/README.md \
  docs/RELEASE_AUTHORIZATION.md
git commit -m "feat(release): seal provider-specific production candidates"
```

---

### Task 5: Make Provider Selection One Safe Operational Switch

**Files:**
- Create: `~/krw-agnet/packaging/systemd/krw-agentd-start`
- Modify: `~/krw-agnet/packaging/systemd/krw-agentd.service`
- Create: `~/krw-agnet/scripts/switch_production_provider.sh`
- Create: `~/krw-agnet/scripts/test_provider_switch.sh`

**Interfaces:**
- Environment: `KRW_AGENT_PROVIDER=glm|deepseek` only.
- Active bundle: `/opt/krw-agent/current` atomic symlink to one sealed provider directory.
- Admission state: `open|drain|closed` stored in the product deployment environment.

- [ ] **Step 1: Write fail-closed wrapper tests**

Test:

```text
missing provider -> refuses startup
unknown provider -> refuses startup
glm + DeepSeek manifest -> refuses startup
deepseek + GLM registry -> refuses startup
missing matching API key -> refuses live startup
valid provider/bundle -> execs krw-agentd with exact --provider
```

- [ ] **Step 2: Implement a closed provider mapping in the wrapper**

```bash
case "$KRW_AGENT_PROVIDER" in
  glm) expected_model=glm-5.2; key_env=GLM_API_KEY ;;
  deepseek) expected_model=deepseek-v4-flash; key_env=DEEPSEEK_API_KEY ;;
  *) exit 2 ;;
esac
```

The wrapper must pass `--provider`, `--database-tls-mode verify-full`, `--release-authorization`, `--release-trust-registry`, `--metrics-bind 127.0.0.1:9464`, and every immutable image path. Secrets remain in `/etc/krw-agent/krw-agentd.env` or secret manager.

- [ ] **Step 3: Change systemd to call the wrapper**

`ExecStart` becomes:

```ini
ExecStart=/opt/krw-agent/current/packaging/krw-agentd-start
```

Keep the existing hardening, memory/task limits and restart policy.

- [ ] **Step 4: Implement the switch state machine**

`switch_production_provider.sh --provider deepseek --release-id ID` performs:

```text
1. set product admission to drain
2. wait until queued=0 and active=0 with a bounded timeout
3. stop krw-agentd
4. verify target bundle offline
5. run krw-agentd --check --database-check for the target
6. atomically replace /opt/krw-agent/current symlink
7. atomically update KRW_AGENT_PROVIDER
8. start krw-agentd
9. verify process, metrics/readiness and exact provider/model/release hash
10. publish the matching public descriptor pins to the web runtime
11. set product admission to open
```

On failure before step 11, restore the previous symlink/env, start the previous daemon, keep admission closed, and exit non-zero.

- [ ] **Step 5: Run the lifecycle test with fake services**

```bash
bash scripts/test_provider_switch.sh
```

Expected: ordering, timeout, rollback and exact provider arguments pass without starting real provider or DB services.

- [ ] **Step 6: Commit**

```bash
git add packaging/systemd/krw-agentd-start packaging/systemd/krw-agentd.service \
  scripts/switch_production_provider.sh scripts/test_provider_switch.sh
git commit -m "feat(ops): switch GLM and DeepSeek with bounded drain"
```

---

### Task 6: Synchronize Local and Production Quality Budgets

**Files:**
- Modify: `~/krw-agnet/deployments/prod/budget-registry.yaml`
- Modify: `~/krw-agnet/deployments/local/budget-registry.yaml`
- Create: `~/krw-agnet/scripts/check_active_budget_parity.py`
- Test: `~/krw-agnet/crates/runtime-config/src/lib.rs`

**Interfaces:**
- Produces: parity for every `required_budget_profile` referenced by shipped AgentSpecs.
- Does not reduce: provider turns, capability calls, reasoning, output tokens or deadlines.

- [ ] **Step 1: Add a parity check for active profiles**

The script reads every `agents/*/agent.yaml`, collects `required_budget_profile`, and compares local/prod limit values excluding only `registry_id` and comments.

```python
assert prod_profiles[profile_id]["limits"] == local_profiles[profile_id]["limits"]
```

- [ ] **Step 2: Observe the current failure**

```bash
python3 scripts/check_active_budget_parity.py
```

Expected: at minimum `company_research_glm` and `scenario_sensitivity` differ.

- [ ] **Step 3: Apply the already validated scenario envelope to production**

Set `scenario_sensitivity` to:

```yaml
max_provider_turns: 16
max_capability_calls: 5
max_replans: 1
max_repairs: 1
max_input_tokens: 120000
max_output_tokens: 28000
max_evidence_bytes: 8388608
deadline_ms: 600000
```

Set `company_research_glm.max_output_tokens` to `28000`. Align every other active profile reported by the script without lowering the larger value.

- [ ] **Step 4: Run registry validation and parity tests**

```bash
python3 scripts/check_active_budget_parity.py
cargo test -p krw-agent-runtime-config --lib
```

- [ ] **Step 5: Commit**

```bash
git add deployments/local/budget-registry.yaml deployments/prod/budget-registry.yaml \
  scripts/check_active_budget_parity.py crates/runtime-config/src/lib.rs
git commit -m "fix(budgets): keep production research quality envelopes"
```

---

### Task 7: Make Agent and Product Database Migrations a Single Gate

**Files:**
- Modify: `~/krw-agnet/scripts/apply_migrations.sh`
- Modify: `~/krw-ontology-front/supabase/migrations/20260812090150_agent_v1_runtime_schema_compatibility.sql`
- Modify: `~/krw-ontology-front/supabase/migrations/20260812090400_agent_v1_commit_final_memory_v7_compatibility.sql`
- Modify: `~/krw-ontology-front/scripts/verify-agent-v1-compatibility.mjs`
- Create: `~/krw-ontology-front/scripts/apply-agent-v1-production-migrations.sh`

**Interfaces:**
- Order: agent-store migrations `0001..latest` → product compatibility migrations → compatibility verifier.
- Produces: a non-secret JSON report containing schema versions and boolean compatibility checks.

- [ ] **Step 1: Track the two currently untracked compatibility migrations**

Before staging, verify they only modify `agent_store` and product-owned Agent V1 functions; they must not modify ontology schemas.

- [ ] **Step 2: Make `final_output_hash` migration lock-safe**

Split the operation into additive phases:

```text
1. add nullable final_output_hash
2. backfill in bounded batches from rendered_message_hash
3. add CHECK constraint NOT VALID
4. validate constraint
5. assert zero NULL rows
6. set NOT NULL
```

The deploy script uses batches of 1,000 rows and logs counts only, never answer bodies or hashes.

- [ ] **Step 3: Extend the compatibility verifier**

Add exact checks for:

```sql
to_regclass('agent_store.session_memory_frontiers') is not null
to_regclass('agent_store.session_memory_deltas') is not null
to_regprocedure('agent_v1.commit_final(jsonb)') is not null
answer_bundles.final_output_hash exists and has zero nulls
public.enqueue_agent_v1_product_run_v7(jsonb) exists
public.apply_agent_v1_product_outbox(jsonb) exists
```

- [ ] **Step 4: Implement the one production migration command**

```bash
scripts/apply-agent-v1-production-migrations.sh \
  --agent-release "/opt/krw-agent/releases/$KRW_RELEASE_ID/deepseek" \
  --front-root /srv/krw-ontology-front-candidate
```

It runs `krw-agnet/scripts/apply_migrations.sh`, then `supabase db push`, then `node scripts/verify-agent-v1-compatibility.mjs`. Any stage failure stops before daemon/web activation.

- [ ] **Step 5: Test against a disposable PostgreSQL database**

Run the full migration sequence twice. Expected: first pass applies changes; second pass is idempotent and the verifier reports every required item as `true`.

- [ ] **Step 6: Commit in each repository**

```bash
git -C ~/krw-agnet add scripts/apply_migrations.sh
git -C ~/krw-ontology-front add \
  supabase/migrations/20260812090150_agent_v1_runtime_schema_compatibility.sql \
  supabase/migrations/20260812090400_agent_v1_commit_final_memory_v7_compatibility.sql \
  scripts/verify-agent-v1-compatibility.mjs \
  scripts/apply-agent-v1-production-migrations.sh
```

---

### Task 8: Pin the Exact Host Package Across Repositories

**Files:**
- Modify: `~/krw-agnet/scripts/package-host-ts.mjs`
- Create: `~/krw-ontology-front/scripts/sync-krw-agent-host-package.mjs`
- Create: `~/krw-ontology-front/vendor/krw-agent-host/provenance.json`
- Modify: `~/krw-ontology-front/package-lock.json`

**Interfaces:**
- Provenance fields: agent Git commit/tree, package name/version, tgz SHA-256.
- Front build fails if installed package bytes do not match provenance.

- [ ] **Step 1: Make package output include provenance**

`package-host-ts.mjs` prints canonical JSON:

```json
{
  "package": "@krw-agent/host",
  "version": "0.1.0",
  "agent_git_commit": "40-or-64-character-lowercase-git-object-id",
  "agent_git_tree": "40-or-64-character-lowercase-git-object-id",
  "archive": "krw-agent-host-0.1.0.tgz",
  "sha256": "sha256:..."
}
```

- [ ] **Step 2: Implement an exact sync command in the front repository**

```bash
node scripts/sync-krw-agent-host-package.mjs \
  --agent-repo ~/krw-agent-release
```

It builds the package, replaces only the vendor tgz, writes `provenance.json`, runs `npm install --package-lock-only`, installs, and verifies the installed package hash.

- [ ] **Step 3: Add a build preflight**

Before `next build`, compare:

```text
vendor tgz SHA == provenance SHA
package-lock resolved path == vendor tgz path
provenance agent commit == coordinated release commit
```

- [ ] **Step 4: Run host and front contract tests**

```bash
npm --prefix ~/krw-agent-release/packages/host-ts test
npm --prefix ~/krw-front-release test -- \
  src/lib/agent-v1
```

- [ ] **Step 5: Commit the package and provenance together**

Never commit a new tgz without its matching `provenance.json` and lockfile.

---

### Task 9: Wire the Production Web Container to the Rust Runtime

**Files:**
- Modify: `~/krw-ontology-front/Dockerfile.web`
- Modify: `~/krw-ontology-front/docker-compose.yml`
- Modify: `~/krw-ontology-front/.env.example`
- Modify: `~/krw-ontology-front/src/lib/agent-v1/runtime.ts`
- Modify: `~/krw-ontology-front/src/worker/agent-v1-outbox-worker.ts`
- Test: `~/krw-ontology-front/src/lib/agent-v1/runtime.test.ts`

**Interfaces:**
- Web runtime requires exact tenant, DB URL, descriptor path/hash and release-set hash.
- Descriptor is mounted read-only at `/run/krw-agent/public-release.json`.
- Provider secret never enters the web or outbox container.

- [ ] **Step 1: Add failing container-contract tests**

Assert the production compose model exposes to `web`:

```text
KRW_AGENT_BACKEND_MODE=rust
KRW_RUNTIME_ENVIRONMENT=prod
KRW_AGENT_TENANT_ID
KRW_AGENT_RELEASE_DESCRIPTOR_PATH=/run/krw-agent/public-release.json
KRW_AGENT_RELEASE_ARTIFACT_HASH
KRW_AGENT_RELEASE_SET_HASH
AGENT_V1_DATABASE_URL
```

Assert `/run/krw-agent` is read-only and neither `GLM_API_KEY` nor `DEEPSEEK_API_KEY` is present in `web` or `agent-v1-outbox`.

- [ ] **Step 2: Copy the missing outbox artifact into the production image**

Add:

```dockerfile
COPY --from=builder --chown=nextjs:nodejs \
  /app/.worker/agent-v1-outbox-worker.mjs \
  ./.worker/agent-v1-outbox-worker.mjs
```

- [ ] **Step 3: Add exact runtime environment and descriptor volume**

Use an operator-provided `KRW_AGENT_RELEASE_DIR` and mount only the public release directory. Do not mount the signing key or provider credential directory.

- [ ] **Step 4: Fail startup before serving chat when release pins are missing**

Production initialization must validate the descriptor path/hash/release-set and DB compatibility once. Development retains its request-time generated `frontend.env` reload behavior.

- [ ] **Step 5: Build and inspect the candidate image**

```bash
npm run build
docker compose build web
docker run --rm "krw-ontology-front-web:$FRONT_GIT_COMMIT" \
  node -e "require('fs').accessSync('.worker/agent-v1-outbox-worker.mjs')"
```

Expected: build succeeds and the worker bundle exists.

- [ ] **Step 6: Commit**

```bash
git add Dockerfile.web docker-compose.yml .env.example \
  src/lib/agent-v1/runtime.ts src/lib/agent-v1/runtime.test.ts \
  src/worker/agent-v1-outbox-worker.ts
git commit -m "feat(agent-v1): wire production web to pinned Rust runtime"
```

---

### Task 10: Require Verified PostgreSQL TLS in Production

**Files:**
- Modify: `~/krw-ontology-front/src/lib/postgres/listen-config.ts`
- Modify: `~/krw-ontology-front/src/lib/agent-v1/runtime.ts`
- Modify: `~/krw-ontology-front/src/worker/agent-v1-outbox-worker.ts`
- Modify: `~/krw-ontology-front/scripts/verify-agent-v1-compatibility.mjs`
- Test: `~/krw-ontology-front/src/lib/postgres/listen-config.test.ts`

**Interfaces:**
- Local loopback: TLS disabled.
- Production remote DB: CA required and `rejectUnauthorized: true`.
- Environment: `AGENT_V1_DATABASE_CA_PEM` or `AGENT_V1_DATABASE_CA_FILE`, never both.

- [ ] **Step 1: Add TLS policy tests**

```ts
expect(resolveAgentV1Tls("postgres://127.0.0.1:54322/db", prodEnv)).toBeUndefined();
expect(() => resolveAgentV1Tls("postgres://db.example.com/db", prodWithoutCa))
  .toThrow("agent_v1_database_ca_missing");
expect(resolveAgentV1Tls("postgres://db.example.com/db", prodWithCa))
  .toEqual({ ca: TEST_CA, rejectUnauthorized: true });
```

- [ ] **Step 2: Replace every Agent V1 `rejectUnauthorized: false` call site**

Use one shared resolver for runtime pool, outbox pool, compatibility gate and LISTEN clients. Preserve local Supabase plain TCP behavior.

- [ ] **Step 3: Run focused tests**

```bash
npm test -- src/lib/postgres/listen-config.test.ts \
  src/lib/agent-v1/runtime.test.ts \
  src/lib/agent/run-event-listener.test.ts
```

- [ ] **Step 4: Commit**

```bash
git add src/lib/postgres/listen-config.ts src/lib/postgres/listen-config.test.ts \
  src/lib/agent-v1/runtime.ts src/worker/agent-v1-outbox-worker.ts \
  scripts/verify-agent-v1-compatibility.mjs
git commit -m "fix(agent-v1): verify production database TLS"
```

---

### Task 11: Add Readiness, Admission Control, and Guaranteed Terminal Responses

**Files:**
- Create: `~/krw-ontology-front/src/lib/agent-v1/readiness.ts`
- Modify: `~/krw-ontology-front/src/app/api/healthz/deep/route.ts`
- Modify: `~/krw-ontology-front/src/app/api/chat/run/route.ts`
- Modify: `~/krw-ontology-front/src/lib/agent-v1/product-outbox.ts`
- Test: matching `*.test.ts` files

**Interfaces:**
- `ResearchAdmissionMode = "open" | "drain" | "closed"`.
- Production default when missing: `closed`.
- `checkAgentV1Readiness()` returns non-secret provider/model/release/DB/outbox booleans.

- [ ] **Step 1: Add admission tests before implementation**

Assert:

```text
open   -> normal enqueue
drain  -> no budget reservation, no enqueue, saved terminal assistant message
closed -> no budget reservation, no enqueue, saved terminal assistant message
enqueue failure -> reserved budget released and terminal assistant message saved
runtime terminal failure -> outbox projects terminal assistant message
duplicate event -> exactly one assistant terminal message
```

- [ ] **Step 2: Implement one simple admission gate**

The gate runs after authentication/session ownership/safety checks and before budget reservation. Closed/drain copy is:

```text
현재 리서치 엔진을 안전하게 전환 중입니다. 질문은 이 채팅방에 저장했으며, 잠시 후 다시 요청해 주세요.
```

It must use the ordinary durable immediate-assistant path so the UI never remains pending.

- [ ] **Step 3: Extend deep health with Agent V1 checks**

Report only:

```json
{
  "ok": true,
  "provider": "deepseek",
  "model": "deepseek-v4-flash",
  "release_set_hash": "sha256:...",
  "descriptor_valid": true,
  "database_compatible": true,
  "outbox_worker_fresh": true,
  "admission": "open"
}
```

Do not expose file paths, DB endpoints, API keys, prompt bodies or raw errors.

- [ ] **Step 4: Make public activation depend on deep health**

`deploy-production-fast.sh` may start a candidate with admission `closed`, but reverse-proxy activation and the later `open` transition require the Agent V1 deep check to pass.

- [ ] **Step 5: Run focused tests**

```bash
npm test -- src/app/api/chat/run/route.test.ts \
  src/app/api/healthz/deep/route.test.ts \
  src/lib/agent-v1/product-outbox.test.ts
```

- [ ] **Step 6: Commit**

```bash
git add src/lib/agent-v1/readiness.ts src/app/api/healthz/deep/route.ts \
  src/app/api/healthz/deep/route.test.ts src/app/api/chat/run/route.ts \
  src/app/api/chat/run/route.test.ts src/lib/agent-v1/product-outbox.ts \
  src/lib/agent-v1/product-outbox.test.ts scripts/deploy-production-fast.sh
git commit -m "feat(agent-v1): gate cutover on durable readiness"
```

---

### Task 12: Verify Multi-User Chat-Room Follow-Up End to End

**Files:**
- Modify: `~/krw-agnet/fixtures/live-quality/v1/session-followup-v1.json`
- Modify: `~/krw-agnet/scripts/run_session_followup_quality.py`
- Modify: `~/krw-ontology-front/src/hooks/useChat.test.ts`
- Modify: `~/krw-ontology-front/src/lib/agent/run-event-listener.test.ts`

**Interfaces:**
- Session identity: exact `tenant_id + principal_id + session_id`.
- Same room: context continues.
- Different room/user: context cannot cross.
- Reconnect: durable session endpoint restores the final answer when terminal event carries hashes only.

- [ ] **Step 1: Fix a four-chain acceptance corpus**

```text
user A / room 1: AVGO initial question
user A / room 1: pronoun-only follow-up
user A / room 2: unrelated company question
user B / room 1: same wording as user A follow-up
```

- [ ] **Step 2: Assert identity isolation in non-live tests**

The accepted run tuple must exactly match the authenticated product tuple. Cross-principal and cross-session memory attachment must fail before a provider call.

- [ ] **Step 3: Assert reconnect behavior**

Simulate SSE disconnect after enqueue, terminal outbox projection, and browser reconnect. Expected: the final Markdown is fetched from the durable session, displayed once, and no duplicate assistant row is created.

- [ ] **Step 4: Run deterministic tests**

```bash
cargo test -p krw-session-memory
cargo test -p krw-agent-runtime-persistence session
npm test -- src/hooks/useChat.test.ts src/lib/agent/run-event-listener.test.ts
```

- [ ] **Step 5: Commit**

```bash
git add fixtures/live-quality/v1/session-followup-v1.json \
  scripts/run_session_followup_quality.py
git -C ~/krw-ontology-front add \
  src/hooks/useChat.test.ts src/lib/agent/run-event-listener.test.ts
```

---

### Task 13: Restore True Tickerless Wide Research

**Files:**
- Modify: `~/krw-agnet/agents/krw-ontology/agent.yaml`
- Create: `~/krw-agnet/agents/krw-ontology/prompts/wide-research-analysis.md`
- Modify: `~/krw-agnet/crates/protocol/src/lib.rs`
- Test: `~/krw-agnet/crates/agent-image/src/lib.rs`
- Test: `~/krw-agnet/crates/run-engine/src/lib.rs`
- Modify: `~/krw-ontology-front/src/types/api-contracts.ts`
- Modify: `~/krw-ontology-front/src/lib/run-kind.ts`
- Modify: `~/krw-ontology-front/src/app/api/chat/run/route.ts`
- Test: `~/krw-ontology-front/src/lib/run-kind.test.ts`
- Test: `~/krw-ontology-front/src/app/api/chat/run/route.test.ts`

**Interfaces:**
- New run kind: `wide_research`.
- Scope: `{ kind: "covered_universe", universe: "covered" }`.
- `idea_generation` remains candidate screening; `wide_research` explains sectors, cohorts, macro channels and cross-company effects without requiring a ticker.

- [ ] **Step 1: Add routing contract tests**

```text
"AI 인프라 투자 증가가 반도체 업종에 어떤 영향을 줘?" -> wide_research
"AI 수혜주 후보 5개 찾아줘" -> idea_generation
"AVGO 현금흐름은?" -> company_research + AVGO
```

- [ ] **Step 2: Add a covered-universe research entrypoint**

The entrypoint uses existing `ontology.query_context_universe`, `ontology.query_universe`, and `ontology.trace_universe` capabilities. It must not add or change an ontology capability or schema.

The workflow phases are:

```text
plan → query_context_universe → ingest → assess gaps
→ optional query_universe/trace_universe → compose → verify → commit
```

- [ ] **Step 3: Keep screening and analysis prompts separate**

`wide-research-analysis.md` instructs the model to explain transmission paths, winners/losers, counterevidence and uncertainty. It must not force a stock shortlist unless the user asks for one.

- [ ] **Step 4: Update the product route**

A global session with no ticker and ordinary research intent maps to `wide_research`; `/discover` continues to send `idea_generation`. Remove the current rule that rejects every tickerless non-idea request.

- [ ] **Step 5: Run both sides' contract tests**

```bash
cargo test -p krw-agent-protocol wide_research
cargo run -p krw-agent -- spec check agents/krw-ontology
npm test -- src/lib/run-kind.test.ts src/app/api/chat/run/route.test.ts
```

- [ ] **Step 6: Commit in both repositories**

Use separate commits named `feat(research): add tickerless wide research` and `feat(chat): route global research to wide workflow`.

---

### Task 14: Close the Current Rust Regression and Add Provider-Parity Tests

**Files:**
- Modify: `~/krw-agnet/crates/run-engine/src/lib.rs`
- Modify: `~/krw-agnet/crates/runtime-persistence/tests/live_provider_episode_audit.rs`
- Modify: `~/krw-agnet/.github/workflows/ci.yml`

**Interfaces:**
- Deterministic fixtures include the current `orient_company` stage.
- Same logical AgentImage runs against GLM and DeepSeek registry fixtures.
- CI remains keyless; live provider calls run only in credentialed staging gates.

- [ ] **Step 1: Update the stale scripted trace fixture**

The reference script must begin with the `company_orienter` decision and `ontology.company_context` result before planner/query-context stages. Update expected provider turns and trace events from the current workflow rather than deleting assertions.

- [ ] **Step 2: Add registry-parameterized engine tests**

Run the same fixture twice:

```rust
for provider in [FixtureProvider::Glm, FixtureProvider::DeepSeek] {
    let outcome = run_company_fixture(provider).await.unwrap();
    assert!(outcome.answer_bundle.rendered_markdown.len() > 40);
    assert_eq!(outcome.answer_bundle.output_contract.id, "final-markdown/v1");
}
```

- [ ] **Step 3: Assert final-turn reservation for every research entrypoint**

Cover `company_research`, `scenario_sensitivity`, `guru_advisor`, `idea_generation`, and `wide_research`. A run at its provider-turn boundary must reserve one composer turn or emit the bounded terminal response; it must not die pending.

- [ ] **Step 4: Run focused and workspace gates**

```bash
cargo test -p krw-agent-run-engine scripted_engine_matches_reference_trace_and_output
cargo test -p krw-agent-run-engine final_provider_turn_is_reserved
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

- [ ] **Step 5: Commit**

```bash
git add crates/run-engine/src/lib.rs \
  crates/runtime-persistence/tests/live_provider_episode_audit.rs \
  .github/workflows/ci.yml
git commit -m "test(runtime): gate current workflows across both providers"
```

---

### Task 15: Run an Identical Live Quality Matrix on GLM and DeepSeek

**Files:**
- Create: `~/krw-agnet/fixtures/live-quality/v1/dual-provider-production-v1.json`
- Create: `~/krw-agnet/scripts/run_dual_provider_acceptance.py`
- Modify: `~/krw-agnet/scripts/collect_release_evidence.py`

**Interfaces:**
- Executes the exact same corpus against `glm` and `deepseek` bundles.
- Stores original question, original answer, sanitized execution trace, usage, timing and verdict.
- Never stores API keys, DB URLs, hidden reasoning or private prompt bodies.

- [ ] **Step 1: Fix the acceptance corpus**

At minimum include:

```text
1. short company question: AVGO recent financial condition and risks
2. ordinary company question: AVGO growth drivers and cash-flow quality
3. complex scenario: AI demand slowdown sensitivity for AVGO
4. Guru: Buffett-style AVGO assessment
5. tickerless wide research: AI infrastructure capex transmission
6. same-session follow-up: "그중 현금흐름에 가장 큰 영향은?"
7. structured-output recovery case: malformed first proposal repaired to typed proposal
8. provider timeout/retry case ending in a terminal user response
```

- [ ] **Step 2: Define non-negotiable verdict rules**

```text
terminal_response_rate = 100%
pending_after_deadline = 0
cross_session_context_leak = 0
provider/model/release pin mismatch = 0
unrecovered typed-output violation = 0
unsupported factual claim in selected audit sample = 0
follow-up preserves exact session/ticker = true
all shipped run kinds produce non-empty investor-facing Markdown = true
```

Do not require identical wording between providers. Judge evidence coverage, correct scope, useful insight, uncertainty and completion.

- [ ] **Step 3: Run GLM staging acceptance**

```bash
python3 scripts/run_dual_provider_acceptance.py \
  --provider glm \
  --release "/opt/krw-agent/releases/$KRW_RELEASE_ID/glm" \
  --corpus fixtures/live-quality/v1/dual-provider-production-v1.json \
  --output "/secure/release-evidence/$KRW_RELEASE_ID/glm"
```

- [ ] **Step 4: Run DeepSeek staging acceptance**

Run the identical command with `--provider deepseek` and the DeepSeek bundle. This is required because DeepSeek is the service lane; a GLM-only pass cannot authorize production.

- [ ] **Step 5: Hash and collect evidence**

`collect_release_evidence.py` verifies each evidence directory belongs to the exact manifest hash and produces one dual-provider summary. Raw answers remain in the secured evidence store; repository fixtures contain questions and rubrics only.

- [ ] **Step 6: Commit**

```bash
git add fixtures/live-quality/v1/dual-provider-production-v1.json \
  scripts/run_dual_provider_acceptance.py scripts/collect_release_evidence.py
git commit -m "test(release): require live GLM and DeepSeek acceptance"
```

---

### Task 16: Make the Front Deployment Reproducible and Ordered

**Files:**
- Modify: `~/krw-ontology-front/scripts/deploy-production-fast.sh`
- Modify: `~/krw-ontology-front/scripts/deploy-vm-compose.sh`
- Create: `~/krw-ontology-front/scripts/deploy-research-engine-production.sh`
- Test: `~/krw-ontology-front/src/lib/deploy-scripts.test.ts`

**Interfaces:**
- Source archive comes from an exact clean commit, not the current directory.
- Requires: sealed DeepSeek candidate, matching front host provenance, successful DB compatibility, closed-admission deep health.
- Activates: DeepSeek service lane; GLM bundle remains installed and verified but inactive.

- [ ] **Step 1: Replace dirty-worktree tar packaging**

Use an exact source commit:

```bash
git archive --format=tar "$FRONT_GIT_COMMIT" | gzip -n > "$FRONT_ARCHIVE"
```

Fail if `git status --porcelain` is non-empty in the release worktree or if the requested commit is not `HEAD`.

- [ ] **Step 2: Require Agent V1 migrations for this cutover**

`deploy-research-engine-production.sh` has no mode that activates Rust chat without running the Task 7 migration/compatibility gate.

- [ ] **Step 3: Implement the exact first-cutover order**

```text
1. verify clean agent/front commits and coordinated provenance
2. verify both provider bundles offline
3. verify production backup/PITR status
4. apply agent-store migrations
5. apply product compatibility migrations
6. run DB compatibility verifier
7. install DeepSeek candidate with admission closed
8. run krw-agentd --check --database-check
9. start DeepSeek krw-agentd and verify exact model/release
10. deploy web and agent-v1-outbox candidate with DeepSeek descriptor pins
11. run internal deep health and one private synthetic run
12. activate reverse proxy while admission remains closed
13. set admission open and restart only web
14. run one product-level canary and verify durable final projection
```

- [ ] **Step 4: Encode rollback boundaries**

Before admission opens, rollback restores the prior web image and agent symlink without touching DB. After admission opens, set closed, drain bounded work, restore prior compatible web/agent release, verify deep health, then reopen. Additive migrations remain in place.

- [ ] **Step 5: Add deployment-script tests**

Assert the script refuses:

```text
dirty source
untracked migration
missing DeepSeek live evidence
GLM descriptor while provider=deepseek
missing outbox worker artifact
failed database compatibility
failed deep health
attempt to open admission before canary success
```

- [ ] **Step 6: Run deployment tests and dry run**

```bash
npm test -- src/lib/deploy-scripts.test.ts
scripts/deploy-research-engine-production.sh --dry-run \
  --agent-release "/opt/krw-agent/releases/$KRW_RELEASE_ID/deepseek" \
  --front-commit "$(git rev-parse HEAD)"
```

- [ ] **Step 7: Commit**

```bash
git add scripts/deploy-production-fast.sh scripts/deploy-vm-compose.sh \
  scripts/deploy-research-engine-production.sh src/lib/deploy-scripts.test.ts
git commit -m "feat(deploy): activate DeepSeek research engine safely"
```

---

### Task 17: Add Performance, Memory, and Operational Gates Without Reducing Research

**Files:**
- Modify: `~/krw-agnet/crates/perf-harness/src/main.rs`
- Modify: `~/krw-agnet/scripts/collect_release_evidence.py`
- Create: `~/krw-agnet/fixtures/performance/v1/dual-provider-production-load-v1.json`
- Modify: `~/krw-agnet/docs/PERFORMANCE_RELEASE_GATES.md`

**Interfaces:**
- Measures queue wait, provider time, MCP time, DB/checkpoint time, RSS and terminal completion separately.
- Reuses compiled images, HTTP pools, DB pools and MCP pools.
- Does not lower calls, tools, reasoning or output budgets.

- [ ] **Step 1: Add release-gate metrics**

Collect per provider:

```text
queue_wait_ms p50/p95/p99
provider_total_ms p50/p95/p99
mcp_total_ms p50/p95/p99
checkpoint_total_ms p50/p95/p99
terminal_projection_ms p50/p95/p99
process_rss_bytes steady/peak
active_runs, queued_runs, DB pool wait, provider pool wait
terminal success/safe-failure/pending counts
```

- [ ] **Step 2: Add invariant checks for unchanged research capacity**

Compare the resolved execution snapshot used by performance runs with the signed release:

```text
same required model profile
same reasoning mode
same provider-turn budget
same capability-call budget
same capability allowlist
same output-token budget
```

- [ ] **Step 3: Run deterministic load first**

Use multi-user/multi-room fixtures without provider HTTP to prove session isolation, fair queueing, bounded resident tasks and outbox idempotence.

- [ ] **Step 4: Run credentialed canary stages**

```text
Stage A: 5 concurrent runs per provider
Stage B: 20 concurrent runs on DeepSeek staging
Stage C: 6-hour low-rate DeepSeek production canary
Stage D: 7-day monitored production observation before raising concurrency
```

No stage is passed by reducing research behavior. If latency is high, optimize serialization, pool reuse, process lifetime and DB indexes; keep the semantic budgets intact.

- [ ] **Step 5: Commit**

```bash
git add crates/perf-harness/src/main.rs scripts/collect_release_evidence.py \
  fixtures/performance/v1/dual-provider-production-load-v1.json \
  docs/PERFORMANCE_RELEASE_GATES.md
git commit -m "perf(release): gate dual-provider production capacity"
```

---

### Task 18: Final Go/No-Go Evidence and Runbook

**Files:**
- Create: `~/krw-agnet/docs/DUAL_PROVIDER_PRODUCTION_RUNBOOK.md`
- Modify: `~/krw-agnet/README.md`
- Create: `~/krw-ontology-front/docs/RESEARCH_ENGINE_CUTOVER.md`

**Interfaces:**
- Produces one release evidence index binding agent commit, front commit, host package hash, provider manifests, descriptors, authorizations, DB versions and acceptance reports.

- [ ] **Step 1: Document routine provider switching**

Include exact commands for:

```text
DeepSeek -> GLM staging switch
GLM -> DeepSeek production switch
closed/drain/open admission
readiness inspection
queue drain inspection
rollback to previous signed release
key rotation and authorization sequence bump
```

- [ ] **Step 2: Document ownership and incident behavior**

State explicitly:

```text
front owns users, rooms, messages and UI history
krw-agent owns execution, checkpoint, evidence and same-room context
outbox owns final product projection
ontology is read-only external semantic authority
no SDK fallback exists
closed/failed paths still create terminal assistant messages
```

- [ ] **Step 3: Generate the final evidence index**

It must contain hashes, not secrets or hidden reasoning:

```json
{
  "agent_commit": "...",
  "front_commit": "...",
  "host_package_sha256": "sha256:...",
  "glm_manifest_hash": "sha256:...",
  "deepseek_manifest_hash": "sha256:...",
  "database_compatibility": "passed",
  "glm_live_acceptance": "passed",
  "deepseek_live_acceptance": "passed",
  "deepseek_product_canary": "passed"
}
```

- [ ] **Step 4: Apply the final Go/No-Go rule**

Production admission may be set to `open` only when all are true:

```text
both source worktrees clean
both provider bundles offline-verified
both provider authorizations valid
DB migration and compatibility gate passed
front descriptor/host package hashes match the DeepSeek release
outbox worker healthy
GLM live acceptance passed
DeepSeek live acceptance passed
DeepSeek product canary produced a durable non-empty answer
same-session follow-up passed
cross-session isolation passed
no known test failure remains
rollback command dry-run passed
```

- [ ] **Step 5: Update README status only after the gate passes**

Do not remove the current “production-ready가 아님” statement until the evidence index exists and every condition above is true.

---

## Execution Order and Checkpoints

The tasks execute in this order:

```text
Checkpoint A — Clean source boundary
  Task 1

Checkpoint B — Reproducible dual provider releases
  Tasks 2 → 3 → 4 → 5 → 6

Checkpoint C — Product contract and runtime readiness
  Tasks 7 → 8 → 9 → 10 → 11 → 12 → 13

Checkpoint D — Full validation
  Tasks 14 → 15 → 17

Checkpoint E — Production cutover
  Task 16 → Task 18
```

Do not start Checkpoint E if any earlier checkpoint has a failing test or missing evidence. GLM and DeepSeek live acceptance can run independently after their sealed candidates exist, but both reports are required before the DeepSeek service lane opens.

## Expected Final Operating State

- DeepSeek is the active production provider.
- GLM is a separately sealed and verified staging/quality provider that can be selected with one bounded switch command.
- Each provider has its own model registry, descriptor, authorization and manifest.
- The web container receives only public release pins and DB credentials; provider keys remain with `krw-agentd`.
- A selected chat room continues its own context across follow-up questions and cannot see another room or user.
- Company, scenario, Guru, idea-screen and tickerless wide research all terminate with a user-visible answer or safe terminal failure message.
- Builds reuse one Rust compilation and one AgentImage build pass while retaining two independently verifiable deployable artifacts.
- Production deploys from exact clean commits; unrelated dirty front changes cannot enter the archive accidentally.
