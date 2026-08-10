# KRW Agent Runtime

현재 GLM-5.2를 live 검증 대상으로 사용하는 KRW 리서치 에이전트 런타임입니다. DeepSeek 호환
계약은 정적 fixture/replay 범위에만 남겨 두고, 실행마다 Claude Code나 별도 플러그인
프로세스를 띄우지 않고, 하나의 Rust daemon이 많은 세션을 제한된 메모리 안에서 처리하도록
만들고 있습니다.

## 아주 쉽게 설명하면

이 시스템은 **GLM-5.2 분석가 + Rust 팀장 + Python 자료실 + PostgreSQL 업무일지**로 동작합니다.

- GLM-5.2는 질문을 이해하고 조사 순서와 최종 설명을 만듭니다.
- Rust는 모델이 마음대로 건너뛰지 못하도록 근거, 도구 순서, 호출 횟수, 시간과 메모리를
  강제합니다.
- 기존 Python `krw-ontology`는 공시와 온톨로지에서 실제 근거를 찾아 줍니다.
- PostgreSQL은 매 단계의 영수증을 기록해, 서버가 중간에 죽어도 같은 일을 중복하지 않고
  안전하게 이어가게 합니다.

여기서 GLM-5.2가 "분석한다"는 말은 첫 계획과 최종 문장만 만든다는 뜻이 아닙니다. 모델은
도구 결과를 받을 때마다 새 근거의 의미, 남은 의문, 결론을 바꿀 가능성을 다시 판단하고 다음 조회나
중단을 제안합니다. Rust는 그 제안이 실제 서버가 보고한 근거 공백과 일치하는지, 중복인지, 비용 대비
가치가 양수인지 확인한 뒤 실행합니다. 즉 조사 방향은 결과에 따라 동적으로 바뀌지만, 네트워크·DB·예산
권한은 모델이 직접 갖지 않습니다.

`AgentSpec`은 KRW 분석가의 정식 규정집이고 `AgentImage`는 그 규정집을 실행용으로 봉인한
파일입니다. 규칙을 수정할 때만 한 번 새 이미지를 만들며, 사용자 질문마다 `SKILL.md`를 읽거나
컴파일하지 않습니다.

사용자가 보는 흐름은 단순합니다.

```text
질문
 → GLM-5.2가 조사 계획 작성
 → Rust가 계획·권한·예산 검사
 → Python 온톨로지에서 근거 수집
 → GLM-5.2가 새 근거를 해석하고 후속 조사 후보 제안
 → Rust가 실제 근거 공백·중복·비용을 평가해 필요한 조회만 실행
 → 근거가 달라질 때마다 위 판단을 반복
 → GLM-5.2가 실제 근거를 바탕으로 일반 한국어 Markdown 답변 작성
 → Rust가 수집된 EvidenceLedger를 답변과 함께 고정하고 출력 경계·예산을 검증
 → 답변·과금·알림 상태를 한 번에 저장
```

대기 중인 세션은 무거운 프로세스나 Tokio task로 만들지 않고 DB의 작은 행으로만 유지합니다.
실제로 실행 중인 제한된 세션만 메모리에 올라오며, 8개 AgentImage의 정적 release catalog,
content-interned prompt, provider HTTP client와 MCP 연결 pool은 컴퓨터 전체에서 공유합니다.

현행 구현 기준과 완료 범위는
[`docs/IMPLEMENTATION_STATUS.md`](docs/IMPLEMENTATION_STATUS.md)입니다. 과거 설계 문서와
상충하면 이 문서와 실제 코드가 우선합니다.

CLI·미래 웹 호스트가 하나의 queue/session 실행 경로를 공유하는 방식은
[`docs/AGENT_GATEWAY.md`](docs/AGENT_GATEWAY.md)에 정리되어 있습니다. 이 repository는 아직
웹앱 route를 mount하지 않았으므로, `krw-agent run`은 준비된 Gateway endpoint가 있을 때만 질문을
제출합니다.

## 최종 형태

```text
직접 작성한 KRW AgentSpec + prompt
        │ 변경할 때만 검증·컴파일
        ▼
immutable AgentImage release set (1..=64)
        │ 시작 시 한 번 검증·부분집합 resolve·catalog precompile
        ├── DeploymentBinding ── MCP 주소·인증·데이터 버전
        ├── Model Registry ───── 정확한 provider/model
        └── RunRequest ───────── 사용자·세션·질문·예산
                    │
                    ▼
              Rust Agent Kernel
                 ├── Anthropic-compatible provider API (현재 live는 GLM-5.2)
                 └── pooled MCP ── 기존 Python krw-ontology
                    │
                    ▼
        EvidenceLedger (kernel-owned) + final Markdown → 원자적 완료
```

`SKILL.md`나 기존 플러그인은 runtime 입력이 아닙니다. KRW의 의미를
[`agents/krw-ontology/agent.yaml`](agents/krw-ontology/agent.yaml), prompt, typed Rust 계약으로
직접 구현했습니다. 이후 규칙을 바꿀 때도 이 소스를 수정하고 새 `AgentImage`를 한 번 만드는
방식입니다. 세션마다 Markdown을 읽거나 컴파일하지 않으므로 실행 속도와 메모리에 영향을 주지
않습니다.

## 확정된 경계

- provider wire는 Anthropic Messages 계약을 사용합니다. GLM TypedJson은 Z.AI JSON mode
  (`response_format.type=json_object`)를 사용하고 canonical schema를 kernel에서 검증합니다.
  현재 provider-network 검증은 GLM-5.2만 허용하며, DeepSeek은 live credential/endpoint를 사용하지
  않는 정적 호환 fixture로만 유지합니다.
- 물리 모델과 profile은 immutable registry에서 exact-match로 고정하며 alias/fallback을 허용하지 않습니다.
- production 실행기는 machine-wide Rust daemon 하나입니다.
- daemon은 모든 direct-authored 이미지를 한 정적 release set으로 서비스하며 receipt image hash 외의
  선택/fallback 경로를 허용하지 않습니다.
- live daemon은 exact public release descriptor, release-set hash, runtime/kernel version, sequence,
  expiry를 Ed25519으로 묶은 signed authorization과 revocation/downgrade trust registry를 검증한 뒤에만
  claim을 받습니다.
- queued/idle session은 DB에 두고 active run만 메모리를 사용합니다.
- AgentImage에는 workflow, 근거 규칙, validator, prompt가 들어갑니다.
- MCP URL, credential, 실제 데이터 release와 모델 선택은 배포 설정에만 둡니다.
- 온톨로지 검색 의미와 `SearchPlan v2`/`ResearchState v2`의 canonical authority는 기존 Python
  `krw-ontology`입니다. Rust는 이를 다시 구현하지 않고 검증된 MCP 계약으로 소비합니다.
- Claude Agent SDK, Claude/Codex plugin importer, legacy executor와 legacy rollback 경로는 만들지
  않습니다.
- 잘못된 변경은 이전 실행기를 되살리지 않고 새 검증 이미지로 수정합니다. 시작한 run은 시작 시
  고정된 이미지 hash를 끝까지 사용합니다.

## 현재 구현 상태

현재는 **keyless vertical slice가 실행되는 개발 중 상태**입니다.

구현됨:

- Rust workspace와 `krw-agent`/`krw-agentd` binary
- 직접 작성한 KRW workflow AgentSpec들과 fixed-author sealed Guru AgentSpec
- deterministic AgentImage compiler, canonical contract ID/hash 검증, content hash와 loader 검증
- 제한된 validator instruction set과 workflow state machine
- EvidenceLedger, strong-claim/숫자 lineage 검증, direct Markdown final-output receipt
- 실행 fence, provider episode/action receipt, cancel/final 단일화의 in-memory 계약 모델
- 다중 세션용 bounded scheduler 기반
- provider-native wire/SSE/reasoning replay 기반 (현재 live admission은 GLM-5.2)
- 기존 `ResearchState v2`를 EvidenceLedger로 옮기는 KRW adapter
- TLS PostgreSQL `agent_v1` claim/lease/fencing supervisor
- standalone release의 Ed25519 authorization, key validity/revoke, expiry, sequence downgrade 차단
- standalone bundle manifest의 file/hash/size/symlink/extra-file offline verifier
- tenant/principal/run 범위로 암호화된 versioned-key recovery CAS
- immutable claim → direct GLM-5.2 → pooled MCP → atomic final의 live daemon 연결
- API key가 필요 없는 vertical-slice fixture

아직 production-ready가 아님:

- 교체한 실제 credential과 production ontology release를 사용한 live acceptance 결과
- 전체 crash/fault matrix와 장기 soak/load/memory gate
- Guru 30Q 회귀 평가, credentialed Guru MCP acceptance, crash/soak 검증
- 배포·운영 hardening

서명된 release authorization은 구현됐지만, 실제 production public key, 신규 credential, live acceptance
receipt와 6시간/7일 성능 evidence는 운영 환경에서 생성해야 합니다. 절차는
[`docs/RELEASE_AUTHORIZATION.md`](docs/RELEASE_AUTHORIZATION.md)에 있습니다.

`krw-agentd`의 live path에는 fixture/fallback 모드가 없습니다. Production image, binding, model,
budget registry, endpoint registry, TLS PostgreSQL, 암호화 artifact key가 모두 검증된 뒤에만 claim을
시작합니다. 실제 credential을 쓰는 smoke/soak gate를 통과하기 전에는 production-ready로 간주하지
않습니다.

## 로컬 확인

Rust 1.97.1이 필요합니다.

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings

cargo run -p krw-agent -- \
  spec check agents/krw-ontology

cargo run -p krw-agent -- \
  image build agents/krw-ontology --out /tmp/krw-agent-image

cargo run -p krw-agent -- \
  image verify /tmp/krw-agent-image

cargo run -p krw-agent -- \
  quickstart --fixture vertical-slice --root .

python3 scripts/test_standalone_release_manifest.py
python3 scripts/collect_release_evidence.py --profile ci

```

`quickstart`는 고정 fixture로 compiler → provider episode → action receipt → evidence ingest →
direct Markdown output boundary → EvidenceLedger receipt → final commit 계약을 확인합니다. live GLM 품질이나 production MCP 품질을 주장하는
명령은 아닙니다.

매번 전체 스택을 다시 띄우지 않는 빠른 개발 루프와 변경 계층별 명령은
[`docs/LOCAL_FEEDBACK_LOOP.md`](docs/LOCAL_FEEDBACK_LOOP.md)에 정리되어 있습니다. 요지는
`./scripts/dev-stack.sh up`으로 스택을 유지하고, Rust는 `test core/adapter`, Python·TypeScript만
바꾼 경우에는 `reload --skip-prepare`, 실제 GLM 검증은 `test smoke`로 분리하는 것입니다.

Production daemon의 설정 검증과 실제 실행 인자는
[`docs/POSTGRES_RUNTIME.md`](docs/POSTGRES_RUNTIME.md)에 정리되어 있습니다.

개발 중 GLM-5.2 wire와 구조화 출력 admission만 확인할 때의 로컬 secret 주입과 probe는
[`docs/LOCAL_SECRETS.md`](docs/LOCAL_SECRETS.md)를 따릅니다.

## 소스 위치

- canonical AgentSpec: [`agents/krw-ontology/agent.yaml`](agents/krw-ontology/agent.yaml)
- prompt source: [`agents/krw-ontology/prompts`](agents/krw-ontology/prompts)
- 환경별 binding/model/budget 예시: [`deployments/local`](deployments/local)
- immutable image와 compiler: [`crates/agent-image`](crates/agent-image)
- kernel/state machine: [`crates/agent-kernel`](crates/agent-kernel)
- evidence와 typed product contracts: [`crates/evidence`](crates/evidence)
- provider wire/SSE and structured-output projection: [`crates/provider-wire`](crates/provider-wire)
- MCP transport/pool: [`crates/tool-mcp`](crates/tool-mcp)
- Python ontology output adapter: [`crates/krw-ontology-adapter`](crates/krw-ontology-adapter)
- persistence/scheduler: [`crates/persistence`](crates/persistence),
  [`crates/scheduler`](crates/scheduler)

## 자격 증명 주의

GLM credential은 로컬 환경 변수에서만 읽습니다. secret 값이나 그 일부를 repository, test fixture,
log, AgentImage에 넣지 않습니다. production release 전에는 별도 credential rotation과 redacted scan을
수행합니다.

## 문서 상태

- 현행 구현 권한: [`docs/IMPLEMENTATION_STATUS.md`](docs/IMPLEMENTATION_STATUS.md)
- 과거 아키텍처 검토: [`docs/EMBEDDED_AGENT_ARCHITECTURE.md`](docs/EMBEDDED_AGENT_ARCHITECTURE.md)
- 과거 시뮬레이션 근거: [`docs/SIMULATION_REPORT.md`](docs/SIMULATION_REPORT.md)
- 과거 검증 초안: [`docs/TEST_PLAN.md`](docs/TEST_PLAN.md)
- 과거 DX 초안: [`docs/DEVELOPER_EXPERIENCE.md`](docs/DEVELOPER_EXPERIENCE.md)
- 폐기된 Node migration 계획: [`docs/IMPLEMENTATION_PLAN.md`](docs/IMPLEMENTATION_PLAN.md)
- operator/deferred action: [`TODOS.md`](TODOS.md)
