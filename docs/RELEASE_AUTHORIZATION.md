# Signed release authorization

`AgentImage`와 public release descriptor의 SHA-256은 bytes 무결성만 증명합니다. 어떤 hash를
운영 daemon이 신뢰해야 하는지는 별도 서명으로 승인해야 합니다. standalone runtime은 live claim 전에
canonical JCS `SignedReleaseAuthorizationV1`을 Ed25519로 검증합니다.

서명은 다음 값을 하나의 원자적인 승인으로 묶습니다.

- daemon이 실제로 resolve한 public descriptor hash와 release-set hash
- runtime/kernel version
- 정확한 물리 모델 `glm-5.2` 또는 `deepseek-v4-flash` (provider bundle과 일치해야 함)
- positive sequence, 발급 시각, 만료 시각
- key ID

Trust registry는 public key, key validity, revoke bit, 최소 sequence를 가집니다. 따라서 서명 파일을
바꾸거나, 다른 release descriptor를 붙이거나, 만료된 키를 쓰거나, 폐기된 키를 쓰거나, 승인된 순번보다
낮은 release로 내리면 daemon은 database 연결과 claim 전에 시작을 거절합니다.

## 운영 절차

1. 새 immutable image와 registry를 배포 후보 위치에 두고 `krw-agentd --check`와
   `--public-release-descriptor-output /secure/staging/public-release.json`으로 descriptor를 생성한다.
   이 단계는 claim을 받지 않는다.
2. air-gapped 또는 별도 signing host에서 private key를 한 번 생성한다.

   ```bash
   krw-agent release keygen --private-key-out /secure/signing/release-2026-a.pk8
   ```

   private key 파일은 새 `0600` regular file만 허용한다. 출력된 public key를 trust registry에 넣는다.
   registry는 secret이 아니지만 canonical JCS여야 하며, 예시는
   [`packaging/release-trust-registry.example.json`](../packaging/release-trust-registry.example.json)에
   있다. placeholder를 그대로 배포하면 안 된다.
3. descriptor의 hash를 직접 재계산하지 말고 CLI가 sealed payload를 만들게 한다.

   ```bash
   krw-agent release sign \
     --descriptor /secure/staging/public-release.json \
     --private-key /secure/signing/release-2026-a.pk8 \
     --key-id release-2026-a \
     --sequence 42 \
     --issued-at-unix-seconds 1785600000 \
     --expires-at-unix-seconds 1786204800 \
     --runtime-version 0.1.0 \
     --kernel-version 0.1.0 \
     --out /secure/staging/release-authorization.json
   ```

4. daemon account이 읽을 수 있는 immutable location에 authorization과 trust registry를 설치하고
   `krw-agent release verify` 및 `krw-agentd --check`를 둘 다 실행한다.
5. systemd/launchd wrapper에는 두 `--release-*` path를 넣는다. live `krw-agentd`는 둘 중 하나만
   있거나 둘 다 없으면 시작하지 않는다. `--check`와 `--database-check`는 artifact 없이 authoring
   진단을 할 수 있지만, artifact를 제공한 경우 항상 검증한다.

## Key rotation and rollback protection

새 key를 registry에 추가하고 새 sequence로 authorize한 다음 daemon을 교체한다. 충분한 drain 기간 뒤
이전 key를 `revoked: true`로 표시한다. 긴급 revoke는 registry의 `revoked`를 바꾸고 daemon을
restart한다. `minimum_sequence`을 마지막 안전 release보다 높게 올리면 이전 signed artifact의
rollback도 차단한다.

private key, API key, endpoint, prompt, credential handle은 authorization, trust registry, AgentImage,
public descriptor, release evidence, systemd unit, repository log에 넣지 않는다.
