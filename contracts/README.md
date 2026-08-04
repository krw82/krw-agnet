# Canonical KRW ontology contracts

`krw-ontology`의 Pydantic 모델이 계약 의미의 유일한 원본입니다. 이 디렉터리는 그 모델을
authoring/CI 시점에 내보낸 결과이며 손으로 수정하지 않습니다.

생성물에는 다음이 함께 들어갑니다.

- `SearchPlan`, `ResearchState`, query-context correction의 deterministic JSON Schema
- 별도 Pydantic 모델이 없는 targeted query/trace 입력은 FastMCP가 실제 등록한 tool argument
  schema와 argument model 결과(서버 소유 계약이며 새 의미를 발명하지 않음)
- 각 schema, 전체 bundle, conformance vector의 SHA-256
- `contracts.py`, metric dictionary 코드와 실제 metric dictionary binding을 묶은 authority hash
- JSON Schema가 표현하지 못하는 Pydantic `model_validator` 동작을 고정한 conformance vector
- Rust가 Python 없이 pin/검증할 수 있는 generated binding

동기화:

```bash
/path/to/krw-ontology/.venv/bin/python \
  scripts/export_krw_contracts.py \
  --source-root /path/to/krw-ontology
```

CI 검사(저장소를 변경하지 않음):

```bash
/path/to/krw-ontology/.venv/bin/python \
  scripts/export_krw_contracts.py \
  --source-root /path/to/krw-ontology \
  --check

cargo test -p krw-agent-contracts
```

production runtime은 이 Python 명령을 실행하지 않습니다. Rust binary에는 이미 생성된 schema와
hash만 포함되며, `verify_embedded`는 시작/readiness 시 한 번 사용할 수 있습니다. JSON Schema
검증만으로 canonical 의미 검증을 대체해서는 안 됩니다. 특히 `SearchPlan`의 clause 관계,
ticker scope, metric/qualitative 분리 같은 제약은 Pydantic validator와 conformance vector가
권위입니다.

## `krw-ontology-front` read-only MCP snapshot

`contracts/krw-front/v1`은 별도의 권위 경계를 갖습니다. Feed와 공개 filing MCP의 의미 원본은
Python ontology가 아니라 `krw-ontology-front`의 실제 TypeScript/Zod 입력과 명시적 반환 타입,
serializer입니다. 8개 권위 파일의 정확한 SHA-256을 manifest에 고정하므로 소스가 바뀌면
재검토 없이 export할 수 없습니다.

```bash
python3 scripts/export_front_read_contracts.py \
  --source-root /path/to/krw-ontology-front \
  --output-root contracts/krw-front/v1

python3 scripts/export_front_read_contracts.py \
  --source-root /path/to/krw-ontology-front \
  --output-root contracts/krw-front/v1 \
  --check

cargo test -p krw-agent-contracts
```

이 snapshot은 3개 feed 도구와 8개 공개 filing 도구의 입력/출력 22개 계약을 포함합니다.
개인 filing 도구는 포함하지 않습니다. Rust hot path는 동적 JSON Schema interpreter 대신
bounded typed validator를 사용하며, 입력과 결과를 함께 검증해 filing/issue identity drift를
막습니다. SEC section/document read는 prior list artifact membership도 별도로 검증할 수 있습니다.
출력 계약은 성공 payload 전용입니다. MCP의 `isError` 응답은 이 결과 계약으로 evidence화하지
않고 transport/capability error 경로로 처리해야 합니다.

TypeScript 생산자에서 아직 명시적으로 자르지 않는 일부 배열은 Rust 소비자 상한에서
fail-closed 됩니다. 이 때문에 schema snapshot이 존재한다는 사실만으로 capability가 배포 준비가
된 것은 아닙니다. AgentSpec pin, DeploymentBinding, MCP normalization, statechart exchange guard가
모두 연결되어야 합니다.

## Default sealed Guru workflow snapshot

`contracts/krw-guru/v1`은 기본 Guru 회사 조사 경로의 14개 계약을 고정합니다.

```text
krw_guru_query_context
  -> immutable GuruResearchPack
  -> krw_guru_company_brief (one sealed main_tension)
  -> krw_ontology_query_context/query/trace
  -> kernel-built company research context
  -> krw_guru_review_company_evidence
```

권위는 두 저장소에 나뉩니다. `krw-ontology-front`의
`guru-investigation-contracts.ts`, `runner.ts`, `ontology/catalog.ts`가 host/runtime projection을
소유하고, `krw-ontology`의 Guru models, company bridge, MCP server/tool이 pack·seal·correction·review
의미를 소유합니다. Manifest가 이 8개 파일의 정확한 SHA-256을 고정하므로 어느 쪽이든 바뀌면
review 없이 export되지 않습니다.

```bash
python3 scripts/export_guru_workflow_contracts.py \
  --front-root /path/to/krw-ontology-front \
  --ontology-root /path/to/krw-ontology \
  --output-root contracts/krw-guru/v1

python3 scripts/export_guru_workflow_contracts.py \
  --front-root /path/to/krw-ontology-front \
  --ontology-root /path/to/krw-ontology \
  --output-root contracts/krw-guru/v1 \
  --check

cargo test -p krw-agent-contracts
```

Schema는 query/brief/review 호출 입력과 결과뿐 아니라 `GuruResearchPack`,
`GuruLightCompanyContext`, draft, sealed brief, correction, runtime research context, private agent
analysis, validated review를 각각 독립적으로 pin합니다. Rust validator는 다음 교차 관계도 검증합니다.

- selected pack ID/author/principle과 sealed brief의 일치
- front camel-case light context의 deterministic snake-case/Pydantic normalization
- light context hash와 server seal hash
- brief ticker/question ID와 runtime filing context의 일치
- assessment evidence ID가 runtime `source_object_ids`의 부분집합인지 여부
- contextual default path의 verdict가 `mixed|unresolved`인지 여부
- unresolved assessment가 evidence ID를 인용하지 않는지 여부
- returned review의 brief/context hash와 decision frame 일치

이 snapshot은 현재 기본 경로만 표현합니다. Legacy `krw_guru_company_pack`은 제외하며,
`krw_ontology_verify_evidence`를 새 단계로 만들지 않습니다. Schema와 validator가 registry에 있다는
사실만으로 Guru workflow가 배포 준비된 것은 아닙니다. AgentSpec pin, capability binding,
state-artifact persistence, statechart의 cross-artifact guard 호출은 별도로 연결되어야 합니다.
