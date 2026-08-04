# Local DeepSeek secret

로컬 실행에만 필요한 DeepSeek credential은 저장소 밖의 secret manager가 가장 좋다. 개발 중에는
이 저장소의 git-ignored [`../.env.local`](../.env.local) 파일을 제한적으로 사용할 수 있다.

```bash
cd ~/krw-agnet
cp .env.example .env.local
chmod 600 .env.local
```

`.env.local`에는 아래 한 줄만 넣는다. 값을 채팅, 명령행 인자, AgentSpec/Image, fixture, log에 넣지
않는다.

```text
DEEPSEEK_API_KEY=<new-secret>
```

이 파일은 자동으로 source되지 않는다. [`../scripts/with_local_env.sh`](../scripts/with_local_env.sh)가
regular-file/0600/단일 변수 형식을 확인한 뒤, 자식 프로세스에만 전달한다. 따라서 환경 파일 안의
shell code, 다른 변수, 명령 치환은 허용하지 않는다.

현재 live wire 확인 명령은 다음과 같다.

```bash
cd ~/krw-agnet
./scripts/with_local_env.sh cargo run -p krw-agent -- provider probe
```

probe는 `deepseek-v4-flash`에 고정된 16-token, no-tool, non-thinking 요청 한 번만 보낸다. 응답 문장과
reasoning은 출력·파일 저장하지 않고, 모델 identity·usage·elapsed time·hash만 JSON으로 출력한다.
터미널 `PATH`에 `cargo`가 없어도 macOS Homebrew Rust 또는 표준 `CARGO_HOME` 설치를 자동으로 찾는다.
실제 daemon은 `.env.local`을 읽지 않으며 OS keychain 또는 배포 secret manager가
`DEEPSEEK_API_KEY`를 process environment로 주입해야 한다.
