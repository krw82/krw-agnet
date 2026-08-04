# Standalone packaging

이 디렉터리는 설치 템플릿만 제공한다. 현재 컴퓨터에 service를 설치하거나
`krw-ontology-front`에 연결하지 않는다.

`scripts/build_standalone_release.sh /absolute/new/path`는 clean committed source에서 다음을 하나의
read-only bundle로 만든다.

- production-profile `krw-agent`, `krw-agentd` binary
- 직접 작성한 8개 immutable AgentImage
- `agent_v1` PostgreSQL migration
- 환경별 registry/binding template
- 웹앱과 분리된 `host-ts` integration kit
- file별 SHA-256을 가진 `krw-standalone-release/v1` manifest

Bundle은 secret, MCP URL 값, database URL 값, API key를 포함하지 않는다. 실제 값은 service
환경에서 secret-manager가 주입한다. `systemd` unit은 예시 hardening과 2 GiB/128 task 상한을
포함한다. macOS `launchd` plist는 secret을 plist에 넣지 않기 위해 환경별
`/usr/local/libexec/krw-agentd-start` wrapper만 호출한다. 그 wrapper는 배포 환경이 secret-manager와
절대경로를 결합해 별도로 설치해야 한다.

최종 운영 배포에서는 bundle manifest뿐 아니라 resolved public release descriptor, credential rotation
receipt, exact Flash live acceptance, 장기 performance report를
`scripts/collect_release_evidence.py`로 함께 검증해야 한다.

live daemon은 이들 외에 descriptor와 exact release set을 묶은 Ed25519 release authorization 및
public trust registry를 요구한다. 생성/rotation 절차는
[`docs/RELEASE_AUTHORIZATION.md`](../docs/RELEASE_AUTHORIZATION.md)에 있다. 이 template은 실제
key나 authorization을 포함하지 않는다.

설치 전에는 bundle directory를 offline verifier로 다시 검사한다.

```bash
python3 /opt/krw-agent/packaging/verify_standalone_release.py --root /opt/krw-agent
```

verifier는 manifest hash, file inventory/hash/size, symlink, extra file과 physical model policy를
fail-closed로 검사한다. bundle을 설치한 뒤에도 release authorization과 trust registry는 별도로
검증해야 한다.
