# Local GLM secret

현재 live 검증에 필요한 GLM-5.2 credential은 저장소 밖의 secret manager가 가장 좋다. 개발 중에는
이 저장소의 git-ignored [`../.env.local`](../.env.local) 파일을 제한적으로 사용할 수 있다.

```bash
cd ~/krw-agnet
cp .env.example .env.local
chmod 600 .env.local
```

`.env.local`에는 GLM 키와, 현재가·PER/PBR 사전 문맥을 쓰려면 선택적으로 FMP 키를 넣는다. 이미
FMP 결과를 ticker별 valuation store에 적재한다면 그 읽기 전용 경로를 함께 넣을 수 있다. 값을 채팅,
명령행 인자, AgentSpec/Image, fixture, log에 넣지 않는다.

```text
GLM_API_KEY=<new-secret>
FMP_API_KEY=<optional-market-data-secret>
KRW_MARKET_SNAPSHOT_STORE_URL=https://your-project.supabase.co
KRW_MARKET_SNAPSHOT_STORE_SERVICE_ROLE_KEY=<optional-store-secret>
```

이 파일은 자동으로 source되지 않는다. [`../scripts/with_local_env.sh`](../scripts/with_local_env.sh)가
regular-file/0600/허용 목록 형식을 확인한 뒤, 자식 프로세스에만 전달한다. 따라서 환경 파일 안의
shell code, 다른 변수, 명령 치환은 허용하지 않는다.

기본 실행은 GLM 키만 agent 프로세스에 전달한다. FMP와 valuation-store 키는
`--market-sidecar` 범위로 capability sidecar에만 전달되므로 agentd나 provider 프로세스에
불필요하게 상속되지 않는다.

현재 live wire 확인 명령은 다음과 같다.

```bash
cd ~/krw-agnet
./scripts/with_local_env.sh cargo run -p krw-agent -- provider probe
```

`provider probe`는 GLM-5.2에 고정된 16-token, no-tool, non-thinking 요청 한 번만 보낸다.
`provider structured-probe`는 같은 GLM endpoint에 baseline, Z.AI JSON mode
(`response_format.type=json_object`), strict transition tool input을 각각 보내고 수락 여부만
redacted JSON으로 출력한다. 이 두 CLI 명령은 GLM credential과 endpoint만
읽고 호출한다.
터미널 `PATH`에 `cargo`가 없어도 macOS Homebrew Rust 또는 표준 `CARGO_HOME` 설치를 자동으로 찾는다.
실제 daemon은 `.env.local`을 읽지 않으며 OS keychain 또는 배포 secret manager가
`GLM_API_KEY`를 process environment로 주입해야 한다.
