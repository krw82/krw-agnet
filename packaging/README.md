# Standalone packaging

이 디렉터리는 설치 템플릿만 제공한다. 현재 컴퓨터에 service를 설치하거나
`krw-ontology-front`에 연결하지 않는다.

`scripts/build_standalone_release.sh --provider glm|deepseek --output /absolute/new/path`는 clean
committed source에서 선택한 provider의 unsigned production candidate를 만든다. 두 provider를 같은
공통 컴파일에서 만들려면 다음을 사용한다.

```bash
scripts/build_dual_provider_release.sh --output-root /absolute/new/dual-release
```

두 candidate는 같은 Rust binary와 AgentImage bytes를 공유하지만 provider별 registry와 manifest를
분리한다. `deployment-binding.example.yaml`과 `endpoint-registry.example.yaml`은 production
placeholder template이므로, 실제 descriptor와 서명을 만든 뒤에만 운영에 설치할 수 있다.

실제 후보 봉인은 provider별로 별도로 수행한다. 먼저 운영자가 채운 binding/endpoint 파일로
descriptor를 만들고, 그 다음 별도 signing host에서 만든 authorization과 trust registry를
넣어 검증한다.

```bash
scripts/prepare_production_candidate.sh \
  --provider deepseek \
  --bundle /secure/releases/<release-id>/deepseek \
  --binding /secure/config/deployment-binding.yaml \
  --endpoints /secure/config/endpoint-registry.yaml

scripts/seal_production_candidate.sh \
  --provider deepseek \
  --candidate /secure/releases/<release-id>/deepseek \
  --authorization /secure/signing/release-authorization.json \
  --trust-registry /secure/signing/release-trust-registry.json
```

GLM도 같은 두 명령을 `--provider glm`으로 한 번 더 수행한다. 두 후보가 모두 봉인된 뒤에만
`scripts/finalize_dual_provider_release.sh --release-root /secure/releases/<release-id>`를
실행해 두 후보의 commit/tree와 descriptor pin을 함께 기록한다. 이 과정은 API key를 파일에
복사하지 않으며, `--check`가 읽는 secret은 signing/배포 환경에서만 주입한다.

각 bundle은 다음을 포함한다.

- production-profile `krw-agent`, `krw-agentd` binary
- 직접 작성한 8개 immutable AgentImage
- `agent_v1` PostgreSQL migration
- 환경별 registry/binding template
- 웹앱과 분리된 `host-ts` integration kit
- provider/model이 고정된 file별 SHA-256 `krw-standalone-release/v2` manifest

Bundle은 secret, MCP URL 값, database URL 값, API key를 포함하지 않는다. 실제 값은 service
환경에서만 주입한다. `systemd` unit은 self-hosted Linux용 예시다. 현재 운영 topology는 기존
**이 Mac의 user-level launchd**다. release 안의
`packaging/launchd/install-local-mac-agentd-release.sh`가 bundle을
`~/.local/share/krw-agent/releases/<id>`에 offline 검증 후 설치하고, `current` symlink와
`com.krw.agentd`만 원자적으로 전환한다. provider key와 완전한 Rust release는 Mac에 남고,
GCP에는 `public-release.json` 한 파일만 전달한다.

최종 운영 배포에서는 bundle manifest뿐 아니라 resolved public release descriptor, credential rotation
receipt, 선택한 제공자(GLM 또는 DeepSeek)의 exact live acceptance, 장기 performance report를
`scripts/collect_release_evidence.py`로 함께 검증해야 한다.

전체 생성·봉인·provider 전환 순서는
[`docs/DUAL_PROVIDER_PRODUCTION_RUNBOOK.md`](../docs/DUAL_PROVIDER_PRODUCTION_RUNBOOK.md)에 고정해 두었다.

front가 사용하는 `@krw-agent/host` tgz도 agent source commit/tree와 함께 고정한다.
깨끗한 agent release worktree에서 다음 명령을 실행하면 front vendor archive, provenance,
lockfile, 설치된 package를 한 번에 갱신하고 마지막에 hash를 재검증한다.

```bash
node ~/krw-ontology-front/scripts/sync-krw-agent-host-package.mjs \
  --agent-repo ~/krw-agent-release
```

live daemon은 이들 외에 descriptor와 exact release set을 묶은 Ed25519 release authorization 및
public trust registry를 요구한다. 생성/rotation 절차는
[`docs/RELEASE_AUTHORIZATION.md`](../docs/RELEASE_AUTHORIZATION.md)에 있다. 이 template은 실제
key나 authorization을 포함하지 않는다.

설치 전에는 bundle directory를 offline verifier로 다시 검사한다.

```bash
python3 /absolute/sealed-dual-release/deepseek/packaging/verify_standalone_release.py \
  --root /absolute/sealed-dual-release/deepseek
```

verifier는 manifest hash, file inventory/hash/size, symlink, extra file과 physical model policy를
fail-closed로 검사한다. bundle을 설치한 뒤에도 release authorization과 trust registry는 별도로
검증해야 한다.

local activation은 front deployer가 호출하는 다음 두 단계로 제한한다. `activate`가 loopback
`/metrics`에서 daemon 시작을 확인하지 못하면 이전 local release를 복구한다.

```bash
/absolute/sealed-dual-release/deepseek/packaging/launchd/install-local-mac-agentd-release.sh \
  --mode stage --release-root /absolute/sealed-dual-release \
  --release-id <id> --provider deepseek

~/.local/share/krw-agent/releases/<id>/deepseek/packaging/launchd/install-local-mac-agentd-release.sh \
  --mode activate --release-id <id> --provider deepseek \
  --env-file /absolute/.env.mac-worker.production
```
