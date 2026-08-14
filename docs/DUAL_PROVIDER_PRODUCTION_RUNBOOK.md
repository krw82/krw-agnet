# GLM·DeepSeek 이중 배포 런북

이 런북은 `krw-agent`의 한 소스 커밋에서 GLM과 DeepSeek 산출물을 모두 만들고,
그중 하나만 서비스 queue에 열어 두는 절차다. 두 provider를 같은 실행 queue에서
동시에 열지 않는다. API key, DB URL, 서명 개인키는 이 문서나 bundle에 넣지 않고
이 Mac의 운영 환경 파일과 user-level `launchd`에만 주입한다.

## 1. 산출물 만들기

반드시 깨끗하게 commit된 `krw-agent` release worktree에서 실행한다. 현재 개발
worktree가 dirty하면 빌더가 시작 전에 중단한다.

```bash
RELEASE_ID=20260812-01
RELEASE_ROOT=/secure/krw-agent/releases/$RELEASE_ID

scripts/build_dual_provider_release.sh --output-root "$RELEASE_ROOT"
```

이 명령은 Rust binary와 AgentImage를 한 번만 만들고, 같은 bytes를 공유하는
`$RELEASE_ROOT/glm`과 `$RELEASE_ROOT/deepseek` unsigned candidate를 만든다.
registry와 manifest만 provider별로 다르다.

## 2. 실제 운영 설정으로 후보 준비·봉인

각 provider에 실제 endpoint/binding을 넣어 descriptor를 만들고, 별도 signing host에서
만든 authorization/trust registry를 넣는다. 두 provider 모두 수행해야 한다.

```bash
for provider in glm deepseek; do
  scripts/prepare_production_candidate.sh \
    --provider "$provider" \
    --bundle "$RELEASE_ROOT/$provider" \
    --binding "/secure/config/$provider/deployment-binding.yaml" \
    --endpoints "/secure/config/$provider/endpoint-registry.yaml"

  scripts/seal_production_candidate.sh \
    --provider "$provider" \
    --candidate "$RELEASE_ROOT/$provider" \
    --authorization "/secure/signing/$provider/release-authorization.json" \
    --trust-registry "/secure/signing/$provider/release-trust-registry.json"
done

scripts/finalize_dual_provider_release.sh --release-root "$RELEASE_ROOT"
```

봉인 단계는 provider와 물리 모델을 정확히 맞춘다.

- `glm` → `glm-5.3`, `GLM_API_KEY`
- `deepseek` → `deepseek-v4-flash`, `DEEPSEEK_API_KEY`

서로 다른 model registry, descriptor, authorization, trust registry, manifest를
갖지만 binary/image와 source commit/tree는 동일해야 한다.

## 3. 오프라인 및 provider별 품질 gate

먼저 두 bundle을 다시 검증한다. 이후 daemon/Gateway를 선택한 provider로 실행한 뒤
9개 provider-release smoke corpus를 각 lane에서 실행한다. 이 단계는 release·전송·세션
경계를 빠르게 확인하는 gate이며, 짧음/보통/복잡 각 6개 이상의 사람이 보는 품질 판정은
별도의 24-case corpus로 추가 실행한다. 다음 명령은 provider API를 자동으로 대체하지
않는다. 실패하면 해당 lane만 열지 않는다.

```bash
python3 scripts/run_dual_provider_acceptance.py \
  --provider glm \
  --release "$RELEASE_ROOT/glm" \
  --require-sealed \
  --report-dir "/secure/evidence/$RELEASE_ID/glm"

python3 scripts/run_dual_provider_acceptance.py \
  --provider deepseek \
  --release "$RELEASE_ROOT/deepseek" \
  --require-sealed \
  --report-dir "/secure/evidence/$RELEASE_ID/deepseek"

# 사람 품질 검토용: 각 bucket 6개(총 18개)를 각 provider의 장기 Gateway에 보낸다.
# runner가 실제 Gateway descriptor/model과 질문 결과의 provider 라벨을 대조하므로,
# 선택한 sealed bundle의 공개 pin을 함께 넘긴다.
for provider in glm deepseek; do
  bundle="$RELEASE_ROOT/$provider"
  descriptor_hash=$(python3 - "$bundle/public-release.json" <<'PY'
import hashlib, sys
with open(sys.argv[1], "rb") as stream:
    print("sha256:" + hashlib.sha256(stream.read()).hexdigest())
PY
  )
  release_set_hash=$(python3 - "$bundle/public-release.json" <<'PY'
import json, sys
print(json.load(open(sys.argv[1], encoding="utf-8"))["release_set_hash"])
PY
  )
  KRW_AGENT_PROVIDER="$provider" \
  KRW_AGENT_RELEASE_DESCRIPTOR_PATH="$bundle/public-release.json" \
  KRW_AGENT_RELEASE_ARTIFACT_HASH="$descriptor_hash" \
  KRW_AGENT_RELEASE_SET_HASH="$release_set_hash" \
  python3 scripts/run_live_quality_matrix.py \
    --provider "$provider" \
    --corpus fixtures/live-quality/v1/supabase-company-research-24-case-corpus.json \
    --per-bucket 6 \
    --report-dir "/secure/evidence/$RELEASE_ID/$provider/full-quality"
done

# 같은 채팅방 후속질문·재개·다른 방 격리는 별도 6-chain suite로도 확인한다.
for provider in glm deepseek; do
  bundle="$RELEASE_ROOT/$provider"
  descriptor_hash=$(python3 - "$bundle/public-release.json" <<'PY'
import hashlib, sys
with open(sys.argv[1], "rb") as stream:
    print("sha256:" + hashlib.sha256(stream.read()).hexdigest())
PY
  )
  release_set_hash=$(python3 - "$bundle/public-release.json" <<'PY'
import json, sys
print(json.load(open(sys.argv[1], encoding="utf-8"))["release_set_hash"])
PY
  )
  KRW_AGENT_PROVIDER="$provider" \
  KRW_AGENT_RELEASE_DESCRIPTOR_PATH="$bundle/public-release.json" \
  KRW_AGENT_RELEASE_ARTIFACT_HASH="$descriptor_hash" \
  KRW_AGENT_RELEASE_SET_HASH="$release_set_hash" \
  python3 scripts/run_session_followup_quality.py \
    --provider "$provider" \
    --report-dir "/secure/evidence/$RELEASE_ID/$provider/follow-up"
done
```

GLM과 DeepSeek acceptance는 각각 정확한 provider/model, descriptor hash, release-set
hash, transport completion을 기록한다. 답변의 의미 품질은 자동 pass로 위조하지 않고
원문 질문·답변·trace를 사람이 확인한다. `short/normal/complex`와 같은 채팅방의
후속질문은 동일 `session_id`로 실행된다.

## 4. 이 Mac의 활성 provider 설치

운영 queue에는 한 provider만 연결한다. `krw-ontology-front`의 fast deployer가 기존
이 Mac의 user-level `launchd`로 full Rust release를 설치한다. 기존 실행을
먼저 `closed` 또는 `drain`으로 막고, bounded drain을 확인한 다음 현재 symlink를
원자적으로 바꾸고 `com.krw.agentd` launchd job을 재시작한다. GCP에는 Rust bundle을
설치하지 않는다.

```bash
# 기존 Mac worker env에만 있는 secret (overlay/bundle/GCP에는 복사하지 않음)
DEEPSEEK_API_KEY=<existing-Mac-worker-env>

# ~/.local/share/krw-agent/runtime/agentd-overlay.env — installer가 생성
KRW_AGENT_CURRENT_DIR=$HOME/.local/share/krw-agent/current
KRW_AGENT_PROVIDER=deepseek
KRW_AGENT_DATABASE_CA_PEM_ENV=KRW_AGENT_DATABASE_CA_PEM

KRW_AGENT_RELEASE_ROOT="$RELEASE_ROOT" \
KRW_AGENT_PROVIDER=deepseek \
  npm --prefix ~/krw-ontology-front run prod:deploy:full -- \
    --release-id "$RELEASE_ID"
```

`packaging/launchd/krw-agentd-start-local`과 `packaging/systemd/krw-agentd-start`가
symlink, model registry, image, auth/trust를
다시 확인한다. GLM을 staging/canary로 바꿀 때도 같은 절차에서 `provider=glm`만
선택한다. live DB는 원격이면 TLS와 CA를 반드시 설정한다.

## 5. front 연결과 admission

`krw-ontology-front`는 사용자·채팅방·메시지·outbox projection을 소유하고,
`krw-agentd`는 실행/checkpoint/evidence와 같은 `session_id`의 후속질문 context를
소유한다. front의 production runtime env에는 선택 bundle의 공개 descriptor와
해시만 넣고 provider key는 넣지 않는다.

```dotenv
KRW_AGENT_BACKEND_MODE=rust
KRW_AGENT_PROVIDER=deepseek
KRW_AGENT_ADMISSION_MODE=closed
KRW_RUNTIME_ENVIRONMENT=prod
KRW_AGENT_RELEASE_DESCRIPTOR_PATH=/run/krw-agent/public-release.json
KRW_AGENT_RELEASE_ARTIFACT_HASH=sha256:<sealed-descriptor-bytes>
KRW_AGENT_RELEASE_SET_HASH=sha256:<descriptor-release-set>
KRW_AGENT_DAEMON_HEARTBEAT_REQUIRED=1
AGENT_V1_DATABASE_URL=<secret-manager-injected>
AGENT_V1_DATABASE_CA_FILE=/run/secrets/agent-v1-ca.pem
KRW_AGENT_TENANT_ID=<non-secret-tenant-id>
```

front image와 `agent-v1-outbox`를 GCP에 배포하고 DB compatibility/migration gate와 deep
health를 확인한다. GCP docker bind mount는 public descriptor 파일 하나뿐이며 full release,
provider key, AgentImage는 Mac 밖으로 나가지 않는다. health가 통과한 뒤에만 `closed → open`으로 바꾼다. admission이
닫혀도 질문은 저장되고 사용자에게 terminal 안내가 남으므로 무기한 pending이 되지
않는다.

`KRW_AGENT_DAEMON_HEARTBEAT_REQUIRED=1`인 release는 Rust daemon이 직접 기록한
release-bound DB receipt를 deep health와 enqueue gate에 요구한다. Mac이 잠들거나
daemon/provider hash가 바뀌면 GCP는 새 run을 queue에 넣지 않는다.

최종적으로 front commit, host package provenance, DB compatibility, 두 acceptance,
DeepSeek durable canary를 하나의 hash-bound evidence index로 묶는다.

```bash
python3 scripts/write_dual_provider_evidence_index.py \
  --release-root "$RELEASE_ROOT" \
  --front-commit "<front-commit>" \
  --host-provenance /secure/front/vendor/krw-agent-host/provenance.json \
  --db-compatibility /secure/evidence/$RELEASE_ID/db-compatibility.json \
  --glm-evidence /secure/evidence/$RELEASE_ID/glm/provider-release-evidence.json \
  --deepseek-evidence /secure/evidence/$RELEASE_ID/deepseek/provider-release-evidence.json \
  --deepseek-canary /secure/evidence/$RELEASE_ID/deepseek/product-canary.json \
  --output /secure/evidence/$RELEASE_ID/production-evidence-index.json
```

## 6. 전환·롤백

provider 전환은 hot-swap하지 않는다.

1. admission을 `drain`으로 바꾸고 신규 run을 받지 않는다.
2. 활성 run/queue/outbox가 bounded drain 조건을 만족하는지 확인한다.
3. 이 Mac의 이전에 봉인된 bundle symlink와 GCP front 공개 hash를 함께 바꾼다.
4. Mac launchd daemon, GCP web, outbox를 재시작하고 provider/model/descriptor/TLS deep health를 확인한다.
5. private canary와 durable final projection을 확인한 뒤 `open`으로 전환한다.

새 provider가 실패하면 같은 순서로 `closed`에 머문 뒤 이전 signed bundle로 되돌린다.
DB migration은 additive forward-only이므로 rollback은 이전 compatible binary/web image로
수행하며 destructive down migration은 하지 않는다.

## Go/No-Go

다음이 모두 pass일 때만 DeepSeek production admission을 연다.

- 두 provider bundle offline verifier 통과
- 두 provider authorization/trust 검증 통과
- 동일 source commit/tree와 provider별 exact model 확인
- DB migration/compatibility와 TLS readiness 통과
- GLM 및 DeepSeek acceptance report 존재
- DeepSeek private canary가 durable non-empty answer를 남김
- 같은 채팅방 후속질문과 다른 채팅방 격리 확인
- outbox terminal projection과 rollback dry-run 통과

하나라도 빠지면 provider를 바꾸거나 연구 예산/도구/추론 수준을 줄여 통과시키지
말고 admission을 닫은 상태로 유지한다.
