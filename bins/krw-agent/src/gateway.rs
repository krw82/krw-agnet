//! Small, typed client for the host-owned Agent Gateway.
//!
//! This module intentionally does not know how to resolve an agent image,
//! call `DeepSeek`, invoke an MCP capability, or read the agent database.
//! Those are respectively host, daemon, capability, and persistence concerns.
//! Keeping the operator CLI on this narrow API prevents it from becoming a
//! second, divergent execution path.

use std::time::{Duration, Instant};

use krw_agent_protocol::{ContentHash, is_canonical_ticker};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use zeroize::Zeroizing;

const SCHEMA_VERSION: u16 = 1;
const MAX_QUESTION_BYTES: usize = 64 * 1024;
const MAX_GATEWAY_RESPONSE_BYTES: usize = 256 * 1024;
const DEFAULT_HTTP_TIMEOUT: Duration = Duration::from_secs(15);
const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SubmittedRun {
    pub session_id: String,
    pub run_id: String,
    pub state: GatewayRunState,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RunStatus {
    pub session_id: String,
    pub run_id: String,
    pub state: GatewayRunState,
    pub final_output: Option<FinalOutput>,
    pub usage: Option<GatewayUsage>,
    pub retry_message: Option<RetryMessage>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct FinalOutput {
    pub markdown: String,
    pub final_output_hash: ContentHash,
    /// Deterministic visualization artifacts compiled by the run engine.
    /// Empty for pre-cutover (schema v3) bundles.
    pub visualizations: Vec<Value>,
    /// Cited evidence record ids for the committed answer, in rendered
    /// footnote order (E1). Empty for pre-cutover bundles and
    /// direct-Markdown lanes without typed linkage.
    pub evidence_ids: Vec<String>,
}

/// Public, credit-oriented usage counters returned for a committed final or
/// terminal failed run. Provider prompts, tool arguments, and raw episode data
/// remain private to the kernel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct GatewayUsage {
    pub provider_turns: u16,
    pub capability_calls: u16,
    pub repairs: u8,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub total_tokens: u32,
    pub token_usage_status: TokenUsageStatus,
    pub billable_tokens: u32,
    pub provider_total_ms: u64,
    pub capability_total_ms: u64,
    /// Diagnostic phase timers added by the gateway alongside the credit
    /// counters (2026-09-07 optimization loop). Purely informational; the
    /// credit validation above never reads them.
    #[serde(default)]
    pub compact_total_ms: u64,
    #[serde(default)]
    pub provider_queue_wait_ms: u64,
    #[serde(default)]
    pub session_memory_total_ms: u64,
    #[serde(default)]
    pub market_preflight_ms: u64,
    #[serde(default)]
    pub prompt_build_total_ms: u64,
    #[serde(default)]
    pub checkpoint_total_ms: u64,
}

/// Whether the provider reported both sides of the token ledger.  When GLM's
/// Anthropic-compatible stream reports zero input tokens for a non-empty
/// request, the gateway exposes only output tokens as credit-eligible rather
/// than fabricating an input count.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TokenUsageStatus {
    Complete,
    OutputOnly,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RetryMessage {
    pub markdown: String,
    pub category: RetryCategory,
    pub retry_recommended: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RetryCategory {
    ModelResponse,
    DataConnection,
    ProcessingLimit,
    ServiceSetup,
    TemporaryProcessing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GatewayRunState {
    Queued,
    Deferred,
    Active,
    Final,
    Cancelled,
    Failed,
}

impl GatewayRunState {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Deferred => "deferred",
            Self::Active => "active",
            Self::Final => "final",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
        }
    }

    pub(crate) const fn is_terminal(self) -> bool {
        matches!(self, Self::Final | Self::Cancelled | Self::Failed)
    }
}

#[derive(Debug)]
pub(crate) struct AgentGatewayClient {
    client: reqwest::Client,
    base_url: reqwest::Url,
    token: Zeroizing<String>,
}

impl AgentGatewayClient {
    pub(crate) fn new(gateway_url: &str, token: String) -> Result<Self, GatewayClientError> {
        let mut base_url =
            reqwest::Url::parse(gateway_url).map_err(|_| GatewayClientError::InvalidGatewayUrl)?;
        if !matches!(base_url.scheme(), "http" | "https")
            || !base_url.username().is_empty()
            || base_url.password().is_some()
            || base_url.query().is_some()
            || base_url.fragment().is_some()
        {
            return Err(GatewayClientError::InvalidGatewayUrl);
        }
        if token.is_empty() || token.len() > 16 * 1024 || token.contains(['\r', '\n']) {
            return Err(GatewayClientError::InvalidGatewayToken);
        }
        let normalized_path = base_url.path().trim_end_matches('/').to_owned();
        base_url.set_path(&normalized_path);
        // The workspace intentionally opts out of an implicit Rustls crypto
        // provider. Match the direct DeepSeek and MCP clients: install the
        // process-wide ring provider once, then let later callers observe the
        // already-installed result.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let client = reqwest::Client::builder()
            .timeout(DEFAULT_HTTP_TIMEOUT)
            .build()
            .map_err(|_| GatewayClientError::ClientUnavailable)?;
        Ok(Self {
            client,
            base_url,
            token: Zeroizing::new(token),
        })
    }

    pub(crate) async fn submit_company_research(
        &self,
        question: &str,
        ticker: &str,
        session_id: Option<&str>,
    ) -> Result<SubmittedRun, GatewayClientError> {
        validate_question(question)?;
        if !is_canonical_ticker(ticker) {
            return Err(GatewayClientError::InvalidTicker);
        }
        if let Some(value) = session_id
            && !is_gateway_id(value)
        {
            return Err(GatewayClientError::InvalidSessionId);
        }
        let suffix = match session_id {
            Some(value) => format!("sessions/{value}/runs"),
            None => "runs".into(),
        };
        let response = self
            .client
            .post(self.endpoint(&suffix))
            .bearer_auth(self.token.as_str())
            .json(&CompanyResearchRequest {
                schema_version: SCHEMA_VERSION,
                question,
                ticker,
            })
            .send()
            .await
            .map_err(map_request_error)?;
        parse_submit_response(response, session_id).await
    }

    pub(crate) async fn submit_open_research(
        &self,
        question: &str,
        session_id: Option<&str>,
    ) -> Result<SubmittedRun, GatewayClientError> {
        validate_question(question)?;
        if let Some(value) = session_id
            && !is_gateway_id(value)
        {
            return Err(GatewayClientError::InvalidSessionId);
        }
        let suffix = match session_id {
            Some(value) => format!("sessions/{value}/runs"),
            None => "runs".into(),
        };
        let response = self
            .client
            .post(self.endpoint(&suffix))
            .bearer_auth(self.token.as_str())
            .json(&OpenResearchRequest {
                schema_version: SCHEMA_VERSION,
                question,
            })
            .send()
            .await
            .map_err(map_request_error)?;
        parse_submit_response(response, session_id).await
    }

    pub(crate) async fn read_run(&self, run_id: &str) -> Result<RunStatus, GatewayClientError> {
        if !is_gateway_id(run_id) {
            return Err(GatewayClientError::InvalidRunId);
        }
        let response = self
            .client
            .get(self.endpoint(&format!("runs/{run_id}")))
            .bearer_auth(self.token.as_str())
            .send()
            .await
            .map_err(map_request_error)?;
        parse_status_response(response, run_id).await
    }

    pub(crate) async fn wait_for_terminal(
        &self,
        run_id: &str,
        timeout: Duration,
    ) -> Result<RunStatus, GatewayClientError> {
        let started = Instant::now();
        loop {
            let status = self.read_run(run_id).await?;
            if status.state.is_terminal() {
                return Ok(status);
            }
            if started.elapsed() >= timeout {
                return Err(GatewayClientError::WaitTimedOut);
            }
            let remaining = timeout.saturating_sub(started.elapsed());
            tokio::time::sleep(DEFAULT_POLL_INTERVAL.min(remaining)).await;
        }
    }

    fn endpoint(&self, suffix: &str) -> reqwest::Url {
        let mut url = self.base_url.clone();
        let base_path = url.path().trim_end_matches('/');
        url.set_path(&format!("{base_path}/{suffix}"));
        url
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum GatewayClientError {
    #[error("gateway URL must be an absolute http(s) URL without credentials, query, or fragment")]
    InvalidGatewayUrl,
    #[error("gateway token is absent or malformed")]
    InvalidGatewayToken,
    #[error("question must be non-empty, NUL-free, and at most 64 KiB UTF-8")]
    InvalidQuestion,
    #[error("ticker must be a canonical uppercase ticker")]
    InvalidTicker,
    #[error("session id is malformed")]
    InvalidSessionId,
    #[error("run id is malformed")]
    InvalidRunId,
    #[error("gateway HTTP client could not be initialized")]
    ClientUnavailable,
    #[error("gateway request failed before a response was received")]
    RequestFailed,
    #[error("gateway returned HTTP status {0}")]
    HttpStatus(u16),
    #[error("gateway response is too large or does not match the v1 contract")]
    InvalidResponse,
    #[error("run did not reach a terminal state before the requested wait timeout")]
    WaitTimedOut,
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct CompanyResearchRequest<'a> {
    schema_version: u16,
    question: &'a str,
    ticker: &'a str,
}

/// The ticker-less free door: the gateway discriminates on the `ticker` key's
/// absence, so the body must not carry the field at all.
#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct OpenResearchRequest<'a> {
    schema_version: u16,
    question: &'a str,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SubmitResponse {
    schema_version: u16,
    session_id: String,
    run_id: String,
    state: GatewayRunState,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StatusResponse {
    schema_version: u16,
    session_id: String,
    run_id: String,
    state: GatewayRunState,
    final_output: Option<FinalOutputResponse>,
    usage: Option<GatewayUsage>,
    retry_message: Option<RetryMessageResponse>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FinalOutputResponse {
    markdown: String,
    final_output_hash: ContentHash,
    #[serde(default)]
    visualizations: Vec<Value>,
    /// Grounded evidence record ids for the committed answer (empty for
    /// pre-cutover bundles and clarification-style outputs).
    #[serde(default)]
    evidence_ids: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RetryMessageResponse {
    markdown: String,
    category: RetryCategory,
    retry_recommended: bool,
}

async fn parse_submit_response(
    response: reqwest::Response,
    requested_session_id: Option<&str>,
) -> Result<SubmittedRun, GatewayClientError> {
    let response = parse_response::<SubmitResponse>(response).await?;
    if response.schema_version != SCHEMA_VERSION
        || !is_gateway_id(&response.session_id)
        || !is_gateway_id(&response.run_id)
        || response.state.is_terminal()
        || requested_session_id.is_some_and(|value| value != response.session_id)
    {
        return Err(GatewayClientError::InvalidResponse);
    }
    Ok(SubmittedRun {
        session_id: response.session_id,
        run_id: response.run_id,
        state: response.state,
    })
}

async fn parse_status_response(
    response: reqwest::Response,
    requested_run_id: &str,
) -> Result<RunStatus, GatewayClientError> {
    let response = parse_response::<StatusResponse>(response).await?;
    if response.schema_version != SCHEMA_VERSION
        || !is_gateway_id(&response.session_id)
        || response.run_id != requested_run_id
    {
        return Err(GatewayClientError::InvalidResponse);
    }
    let final_output = match (response.state, response.final_output) {
        (GatewayRunState::Final, Some(final_output)) if valid_markdown(&final_output.markdown) => {
            Some(FinalOutput {
                markdown: final_output.markdown,
                final_output_hash: final_output.final_output_hash,
                visualizations: final_output.visualizations,
                evidence_ids: final_output.evidence_ids,
            })
        }
        (GatewayRunState::Final, None | Some(_)) => {
            return Err(GatewayClientError::InvalidResponse);
        }
        (_, Some(_)) => return Err(GatewayClientError::InvalidResponse),
        (_, None) => None,
    };
    let retry_message = match (response.state, response.retry_message) {
        (GatewayRunState::Failed, Some(message))
            if valid_markdown(&message.markdown) && message.retry_recommended =>
        {
            Some(RetryMessage {
                markdown: message.markdown,
                category: message.category,
                retry_recommended: message.retry_recommended,
            })
        }
        (GatewayRunState::Failed, None | Some(_)) => {
            return Err(GatewayClientError::InvalidResponse);
        }
        (_, Some(_)) => return Err(GatewayClientError::InvalidResponse),
        (_, None) => None,
    };
    let usage = match (response.state, response.usage) {
        (GatewayRunState::Final | GatewayRunState::Failed, Some(usage)) if valid_usage(&usage) => {
            Some(usage)
        }
        (GatewayRunState::Final, None | Some(_)) => {
            return Err(GatewayClientError::InvalidResponse);
        }
        (GatewayRunState::Failed, None) => None,
        (GatewayRunState::Failed, Some(_)) => return Err(GatewayClientError::InvalidResponse),
        (_, Some(_)) => return Err(GatewayClientError::InvalidResponse),
        (_, None) => None,
    };
    Ok(RunStatus {
        session_id: response.session_id,
        run_id: response.run_id,
        state: response.state,
        final_output,
        usage,
        retry_message,
    })
}

fn valid_markdown(markdown: &str) -> bool {
    !markdown.is_empty() && !markdown.contains('\0') && markdown.len() <= MAX_QUESTION_BYTES
}

fn valid_usage(usage: &GatewayUsage) -> bool {
    usage.total_tokens == usage.input_tokens.saturating_add(usage.output_tokens)
        && match usage.token_usage_status {
            TokenUsageStatus::Complete => {
                usage.billable_tokens == usage.total_tokens
                    && !(usage.provider_turns > 0
                        && usage.input_tokens == 0
                        && usage.output_tokens > 0)
            }
            TokenUsageStatus::OutputOnly => {
                usage.input_tokens == 0 && usage.billable_tokens == usage.output_tokens
            }
        }
}

async fn parse_response<T: for<'de> Deserialize<'de>>(
    response: reqwest::Response,
) -> Result<T, GatewayClientError> {
    if !response.status().is_success() {
        return Err(GatewayClientError::HttpStatus(response.status().as_u16()));
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|_| GatewayClientError::RequestFailed)?;
    if bytes.is_empty() || bytes.len() > MAX_GATEWAY_RESPONSE_BYTES {
        return Err(GatewayClientError::InvalidResponse);
    }
    serde_json::from_slice(&bytes).map_err(|_| GatewayClientError::InvalidResponse)
}

fn validate_question(question: &str) -> Result<(), GatewayClientError> {
    if question.is_empty() || question.contains('\0') || question.len() > MAX_QUESTION_BYTES {
        return Err(GatewayClientError::InvalidQuestion);
    }
    Ok(())
}

fn is_gateway_id(value: &str) -> bool {
    (1..=128).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn map_request_error(_: reqwest::Error) -> GatewayClientError {
    GatewayClientError::RequestFailed
}

#[cfg(test)]
mod tests {
    use super::{
        AgentGatewayClient, GatewayClientError, GatewayRunState, GatewayUsage, TokenUsageStatus,
        is_gateway_id, valid_usage,
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    #[test]
    fn gateway_client_rejects_unsafe_base_and_token_values() {
        assert!(matches!(
            AgentGatewayClient::new("https://user:pass@example.test/v1/agent", "token".into()),
            Err(GatewayClientError::InvalidGatewayUrl)
        ));
        assert!(matches!(
            AgentGatewayClient::new("https://example.test/v1/agent?x=1", "token".into()),
            Err(GatewayClientError::InvalidGatewayUrl)
        ));
        assert!(matches!(
            AgentGatewayClient::new("https://example.test/v1/agent", "bad\ntoken".into()),
            Err(GatewayClientError::InvalidGatewayToken)
        ));
    }

    #[test]
    fn gateway_path_keeps_a_deployment_prefix() {
        let client =
            AgentGatewayClient::new("https://example.test/internal/agent/", "token".into())
                .expect("safe client");
        assert_eq!(
            client.endpoint("sessions/ses_123/runs").as_str(),
            "https://example.test/internal/agent/sessions/ses_123/runs"
        );
    }

    #[test]
    fn opaque_ids_are_path_safe_and_states_are_terminal_only_when_expected() {
        assert!(is_gateway_id("ses_ABC-123"));
        assert!(!is_gateway_id("../not-a-session"));
        assert!(!is_gateway_id("with/slash"));
        assert!(!GatewayRunState::Active.is_terminal());
        assert!(GatewayRunState::Final.is_terminal());
        assert!(GatewayRunState::Cancelled.is_terminal());
        assert!(GatewayRunState::Failed.is_terminal());
    }

    #[test]
    fn credit_usage_never_bills_an_unreported_glm_input_count() {
        let output_only = GatewayUsage {
            provider_turns: 3,
            capability_calls: 1,
            repairs: 0,
            input_tokens: 0,
            output_tokens: 800,
            total_tokens: 800,
            token_usage_status: TokenUsageStatus::OutputOnly,
            billable_tokens: 800,
            provider_total_ms: 5_000,
            capability_total_ms: 120,
            compact_total_ms: 0,
            provider_queue_wait_ms: 0,
            session_memory_total_ms: 0,
            market_preflight_ms: 0,
            prompt_build_total_ms: 0,
            checkpoint_total_ms: 0,
        };
        assert!(valid_usage(&output_only));

        let fabricated_input = GatewayUsage {
            token_usage_status: TokenUsageStatus::Complete,
            billable_tokens: 800,
            ..output_only
        };
        assert!(!valid_usage(&fabricated_input));
    }

    #[tokio::test]
    async fn submit_sends_only_safe_company_research_intent_to_the_gateway() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind isolated test gateway");
        let address = listener.local_addr().expect("test gateway address");
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept one client");
            let request = read_http_request(&mut socket).await;
            let request_text = String::from_utf8(request).expect("UTF-8 request");
            let (headers, body) = request_text
                .split_once("\r\n\r\n")
                .expect("HTTP header boundary");
            assert!(headers.starts_with("POST /v1/agent/runs HTTP/1.1\r\n"));
            assert!(
                headers
                    .to_ascii_lowercase()
                    .contains("authorization: bearer unit-token\r\n")
            );
            let body: serde_json::Value = serde_json::from_str(body).expect("request JSON");
            assert_eq!(
                body,
                serde_json::json!({
                    "schema_version": 1,
                    "question": "최근 매출 추이를 알려줘",
                    "ticker": "AAPL"
                })
            );
            let response = r#"{"schema_version":1,"session_id":"ses_unit","run_id":"run_unit","state":"queued"}"#;
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 202 Accepted\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",
                        response.len()
                    )
                    .as_bytes(),
                )
                .await
                .expect("write gateway response");
        });

        let client =
            AgentGatewayClient::new(&format!("http://{address}/v1/agent"), "unit-token".into())
                .expect("gateway client");
        let submitted = client
            .submit_company_research("최근 매출 추이를 알려줘", "AAPL", None)
            .await
            .expect("submit safe intent");
        assert_eq!(submitted.session_id, "ses_unit");
        assert_eq!(submitted.run_id, "run_unit");
        assert_eq!(submitted.state, GatewayRunState::Queued);
        server.await.expect("gateway server task");
    }

    #[tokio::test]
    async fn failed_status_preserves_bounded_usage_for_credit_settlement() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind isolated test gateway");
        let address = listener.local_addr().expect("test gateway address");
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept one client");
            let _ = read_http_request(&mut socket).await;
            let response = serde_json::json!({
                "schema_version": 1,
                "session_id": "ses_failed",
                "run_id": "run_failed",
                "state": "failed",
                "final_output": null,
                "usage": {
                    "provider_turns": 2,
                    "capability_calls": 1,
                    "repairs": 1,
                    "input_tokens": 0,
                    "output_tokens": 700,
                    "total_tokens": 700,
                    "token_usage_status": "output_only",
                    "billable_tokens": 700,
                    "provider_total_ms": 12,
                    "capability_total_ms": 4
                },
                "retry_message": {
                    "markdown": "## 재시도 필요",
                    "category": "model_response",
                    "retry_recommended": true
                }
            })
            .to_string();
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",
                        response.len()
                    )
                    .as_bytes(),
                )
                .await
                .expect("write failed status");
        });

        let client =
            AgentGatewayClient::new(&format!("http://{address}/v1/agent"), "unit-token".into())
                .expect("gateway client");
        let status = client
            .read_run("run_failed")
            .await
            .expect("read failed status");
        assert_eq!(status.state, GatewayRunState::Failed);
        assert_eq!(
            status.usage.as_ref().map(|usage| usage.billable_tokens),
            Some(700)
        );
        server.await.expect("gateway server task");
    }

    async fn read_http_request(socket: &mut TcpStream) -> Vec<u8> {
        const MAX_REQUEST_BYTES: usize = 16 * 1024;
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 2048];
        loop {
            let count = socket.read(&mut buffer).await.expect("read HTTP request");
            assert!(count > 0, "unexpected EOF before complete request");
            bytes.extend_from_slice(&buffer[..count]);
            assert!(
                bytes.len() <= MAX_REQUEST_BYTES,
                "test request exceeded bound"
            );
            let Some(header_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") else {
                continue;
            };
            let header_text = std::str::from_utf8(&bytes[..header_end]).expect("UTF-8 headers");
            let content_length = header_text
                .lines()
                .find_map(|line| {
                    line.split_once(':').and_then(|(name, value)| {
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().ok())
                            .flatten()
                    })
                })
                .unwrap_or(0);
            if bytes.len() >= header_end + 4 + content_length {
                return bytes;
            }
        }
    }
}
