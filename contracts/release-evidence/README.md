# Release evidence receipts

`scripts/collect_release_evidence.py`는 임의 명령이나 외부 Boolean을 신뢰하지 않고 고정된 local
검증 행렬을 직접 실행한다. 각 로그는 content hash로 manifest에 묶이며 credential 모양의 환경 변수는
자식 프로세스에 전달하지 않는다.

전체 authoritative 판정에는 local 검사 외에 다음 세 개의 bounded JSON이 필요하다.

1. daemon이 `schema_version: 3`로 출력한 public release descriptor
2. 운영자가 secret 값을 넣지 않고 서명·보관한 `krw-credential-rotation/v1` receipt
3. 별도 credentialed 환경이 선택한 제공자의 exact physical model
   (`glm-5.2` 또는 `deepseek-v4-flash`)와 production MCP/PostgreSQL/품질 검사를
   통과한 뒤 출력한 `krw-live-acceptance/v1` redacted receipt

필수 rotation 필드:

```json
{
  "schema_version": "krw-credential-rotation/v1",
  "old_credential_revoked": true,
  "new_secret_version_hash": "sha256:...",
  "redacted_repository_scan_passed": true,
  "repository_scan_hash": "sha256:...",
  "redacted_log_scan_passed": true,
  "log_scan_hash": "sha256:...",
  "contains_secret_material": false
}
```

필수 live 필드:

```json
{
  "schema_version": "krw-live-acceptance/v1",
  "status": "pass",
  "redacted": true,
  "provider_id": "deepseek",
  "requested_model": "deepseek-v4-flash",
  "observed_model": "deepseek-v4-flash",
  "release_set_hash": "sha256:...",
  "data_release_hash": "sha256:...",
  "provider_contract_hash": "sha256:...",
  "mcp_contract_hash": "sha256:...",
  "postgres_fault_matrix_hash": "sha256:...",
  "quality_report_hash": "sha256:..."
}
```

Receipt에는 key, token prefix, raw prompt/response, personal tool payload를 넣지 않는다. Collector는
receipt의 구조와 hash binding을 확인하지만 서명·보관 정책은 배포 환경이 소유한다.

Local smoke와 manifest 무결성 검사는 다음처럼 실행한다.

```bash
python3 scripts/collect_release_evidence.py --profile ci
python3 scripts/collect_release_evidence.py --profile production-7d \
  --provider deepseek \
  --release-descriptor /secure/releases/<id>/deepseek/public-release.json \
  --release-authorization /secure/releases/<id>/deepseek/release-authorization.json \
  --release-trust-registry /secure/releases/<id>/deepseek/release-trust-registry.json \
  --credential-rotation-receipt /secure/evidence/<id>/deepseek/rotation.json \
  --live-acceptance-receipt /secure/evidence/<id>/deepseek/live-acceptance.json
python3 scripts/collect_release_evidence.py \
  --verify-manifest target/release-evidence/ci-.../manifest.json
```
