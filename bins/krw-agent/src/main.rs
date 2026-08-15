use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
#[cfg(feature = "dev-tools")]
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use clap::{Parser, Subcommand, ValueEnum};
use krw_agent_image::{compile_agent_dir, load_image, validate_spec, write_image};
use krw_agent_protocol::ThinkingMode;
use krw_agent_protocol::{
    ALLOWED_MODEL_IDS, ContentHash, GLM_MODEL_ID, PUBLIC_RELEASE_DESCRIPTOR_SCHEMA_VERSION,
    ProviderKind, PublicReleaseDescriptor,
};
use krw_agent_provider_wire::{
    EpisodeContext, MessagesRequest, ProviderClient, ProviderClientConfig, ProviderFunctionName,
    ProviderMessage, ProviderToolDefinition, ResponseFormat, ThinkingConfig, ToolChoice, WireError,
};
use krw_agent_release_authorization::{
    RELEASE_AUTHORIZATION_SCHEMA_VERSION, ReleaseAuthorizationPayloadV1, VerificationContext,
    canonical_authorization_bytes, generate_private_key_pkcs8, parse_canonical_authorization,
    parse_canonical_trust_registry, public_descriptor_hash, public_key_hex_from_private_key, sign,
    verify_for_descriptor,
};
#[cfg(feature = "dev-tools")]
use krw_agent_research_quality::{
    load_suite, run_fixture_case_with_model, run_recorded_fixture_case,
};
use zeroize::Zeroizing;

mod gateway;

use gateway::{AgentGatewayClient, GatewayRunState};

const MAX_DESCRIPTOR_BYTES: usize = 2 * 1024 * 1024;
const MAX_PRIVATE_KEY_BYTES: usize = 16 * 1024;
const GLM_API_BASE: &str = "https://api.z.ai/api/anthropic";
const DEEPSEEK_API_BASE: &str = "https://api.deepseek.com/anthropic";
const PROVIDER_PROBE_PROMPT: &str = "Reply with only: OK";
const PROVIDER_PROBE_MAX_TOKENS: u32 = 16;

#[derive(Debug, Parser)]
#[command(
    name = "krw-agent",
    version,
    about = "KRW Agent authoring and operator CLI"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Spec {
        #[command(subcommand)]
        command: SpecCommand,
    },
    Image {
        #[command(subcommand)]
        command: ImageCommand,
    },
    /// Offline signing and verification of a standalone release authorization.
    Release {
        #[command(subcommand)]
        command: ReleaseCommand,
    },
    /// Read-only, fixed-prompt GLM-5.3 provider readiness check.
    Provider {
        #[command(subcommand)]
        command: ProviderCommand,
    },
    /// Fixture-backed research-quality acceptance tests using GLM-5.3.
    ///
    /// Dev-only: this subcommand (and the research-quality dependency
    /// subgraph it needs) exists only in `--features dev-tools` builds.
    /// Default release/admin builds do not compile it.
    #[cfg(feature = "dev-tools")]
    Quality {
        #[command(subcommand)]
        command: QualityCommand,
    },
    /// Dev-only vertical-slice smoke fixture.
    ///
    /// Exists only in `--features dev-tools` builds; default builds do not
    /// compile the test-support dependency subgraph.
    #[cfg(feature = "dev-tools")]
    Quickstart {
        #[arg(long)]
        fixture: String,
        #[arg(long, default_value = ".")]
        root: PathBuf,
    },
    /// Submit one company-research turn through the host-owned Agent Gateway.
    ///
    /// This command never reads provider credentials, calls an MCP server, or
    /// connects to the agent database. The same gateway is the future web
    /// host boundary, so CLI and browser turns share one queue/session model.
    Run {
        /// Korean research question for the current turn.
        #[arg(long)]
        question: String,
        /// One exact company ticker for the company-research entrypoint.
        #[arg(long)]
        ticker: String,
        /// Continue this durable session. Omit for a server-created new session.
        #[arg(long)]
        session_id: Option<String>,
        /// Absolute Agent Gateway base URL, normally ending in `/v1/agent`.
        #[arg(long)]
        gateway_url: String,
        /// Environment variable containing a CLI bearer token for the gateway.
        #[arg(long, default_value = "KRW_AGENT_GATEWAY_TOKEN")]
        token_env: String,
        /// Wait for the terminal result and print only final Markdown to stdout.
        #[arg(long)]
        wait: bool,
        /// Upper bound for `--wait`; polling is client-side and creates no daemon task.
        #[arg(long, default_value_t = 300)]
        wait_timeout_seconds: u64,
        /// Print a stable JSON envelope instead of human-oriented output.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
enum SpecCommand {
    Check { agent_dir: PathBuf },
}

#[derive(Debug, Subcommand)]
enum ImageCommand {
    Build {
        agent_dir: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
    Verify {
        image_dir: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
enum ReleaseCommand {
    /// Create a new PKCS#8 Ed25519 private key in a new 0600 file. The public
    /// key is printed only to standard output for trust-registry authoring.
    Keygen {
        #[arg(long)]
        private_key_out: PathBuf,
    },
    /// Print the raw public key corresponding to one private PKCS#8 key.
    PublicKey {
        #[arg(long)]
        private_key: PathBuf,
    },
    /// Create a canonical, signed release authorization. All timestamps are
    /// Unix seconds so an offline signing host does not need a time parser.
    Sign {
        #[arg(long)]
        descriptor: PathBuf,
        #[arg(long)]
        private_key: PathBuf,
        #[arg(long)]
        key_id: String,
        #[arg(long)]
        sequence: u64,
        #[arg(long)]
        issued_at_unix_seconds: u64,
        #[arg(long)]
        expires_at_unix_seconds: u64,
        #[arg(long)]
        runtime_version: String,
        #[arg(long)]
        kernel_version: String,
        /// Exact provider model to bind to this authorization. When omitted,
        /// the CLI infers the single model present in the public descriptor.
        #[arg(long)]
        model_id: Option<String>,
        #[arg(long)]
        out: PathBuf,
    },
    /// Verify a signed authorization against one canonical descriptor and one
    /// canonical public trust registry. No key or network credential is read.
    Verify {
        #[arg(long)]
        descriptor: PathBuf,
        #[arg(long)]
        authorization: PathBuf,
        #[arg(long)]
        trust_registry: PathBuf,
        #[arg(long)]
        runtime_version: String,
        #[arg(long)]
        kernel_version: String,
        #[arg(long)]
        now_unix_seconds: Option<u64>,
    },
}

#[derive(Debug, Subcommand)]
enum ProviderCommand {
    /// Send one fixed, no-tool request and print only redacted wire metadata.
    /// GLM is the default; use `--provider deepseek` for the production lane.
    Probe {
        #[arg(long, value_enum, default_value_t = ProviderProbeArg::Glm)]
        provider: ProviderProbeArg,
    },
    /// Send a provider admission probe for GLM JSON mode and strict transition
    /// tool input. DeepSeek's Anthropic lane intentionally uses its documented
    /// `output_config.effort` path and is checked by the regular `probe` plus
    /// the full dual-provider acceptance runner instead.
    StructuredProbe,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ProviderProbeArg {
    Glm,
    Deepseek,
}

impl ProviderProbeArg {
    /// The provider kind this CLI label selects; the protocol registry owns
    /// the kind-to-model and kind-to-credential mappings.
    const fn kind(self) -> ProviderKind {
        match self {
            Self::Glm => ProviderKind::Glm,
            Self::Deepseek => ProviderKind::Deepseek,
        }
    }

    const fn model_id(self) -> &'static str {
        self.kind().model_id()
    }

    const fn api_base(self) -> &'static str {
        match self {
            Self::Glm => GLM_API_BASE,
            Self::Deepseek => DEEPSEEK_API_BASE,
        }
    }

    const fn api_key_env(self) -> &'static str {
        self.kind().credential_env()
    }
}

/// Dev-only `quality` subcommand surface, compiled only with
/// `--features dev-tools`.
#[cfg(feature = "dev-tools")]
#[derive(Debug, Subcommand)]
enum QualityCommand {
    /// Validate a content-addressed quality suite without calling a provider.
    Check {
        #[arg(long, default_value = "evals/krw-research-quality/v4")]
        suite: PathBuf,
        #[arg(long, default_value = ".")]
        root: PathBuf,
    },
    /// Run one bounded, fixture-backed full-kernel quality acceptance case.
    Run {
        #[arg(long, default_value = "evals/krw-research-quality/v4")]
        suite: PathBuf,
        #[arg(long)]
        case: String,
        #[arg(long, default_value = ".")]
        root: PathBuf,
        /// Optional new 0600 output file for the rendered answer. It is never
        /// included in the standard JSON report.
        #[arg(long)]
        answer_out: Option<PathBuf>,
        /// Optional new 0600 file containing only the redacted quality report.
        /// Unlike `answer_out`, this never contains rendered model text.
        #[arg(long)]
        report_out: Option<PathBuf>,
    },
    /// Replay immutable provider turns through the full kernel without a
    /// network request or a `DeepSeek` credential. This is the CI regression
    /// gate; `run` remains the separate live-model quality measurement.
    Replay {
        #[arg(long, default_value = "evals/krw-research-quality/v4")]
        suite: PathBuf,
        #[arg(long)]
        case: String,
        #[arg(long, default_value = ".")]
        root: PathBuf,
        /// Optional new 0600 output file for the deterministic rendered answer.
        #[arg(long)]
        answer_out: Option<PathBuf>,
        /// Optional new 0600 file containing only the redacted quality report.
        #[arg(long)]
        report_out: Option<PathBuf>,
    },
}

#[derive(Debug, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct ProviderProbeReport {
    schema_version: u16,
    model_id: String,
    observed_model: String,
    finish_reason: String,
    tool_call_count: usize,
    prompt_tokens: u32,
    completion_tokens: u32,
    total_tokens: u32,
    elapsed_ms: u64,
    request_hash: ContentHash,
    replay_hash: ContentHash,
}

#[derive(Debug, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct ProviderStructuredAdmissionReport {
    schema_version: u16,
    model_id: String,
    baseline: ProviderAdmissionResult,
    json_object_output: ProviderAdmissionResult,
    strict_tool_input: ProviderAdmissionResult,
}

#[derive(Debug, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct ProviderAdmissionResult {
    accepted: bool,
    observed_model: Option<String>,
    finish_reason: Option<String>,
    tool_call_count: usize,
    elapsed_ms: u64,
    error_kind: Option<String>,
    request_id_hash: Option<ContentHash>,
    retry_after_ms: Option<u64>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    match Cli::parse().command {
        Command::Spec {
            command: SpecCommand::Check { agent_dir },
        } => {
            let bytes = std::fs::read(agent_dir.join("agent.yaml"))?;
            let spec = krw_agent_image::parse_spec(&bytes)?;
            validate_spec(&spec)?;
            let image = compile_agent_dir(agent_dir)?;
            println!(
                "valid {} {}",
                image.manifest.body.metadata.id, image.manifest.content_hash
            );
        }
        Command::Image {
            command: ImageCommand::Build { agent_dir, out },
        } => {
            let image = compile_agent_dir(agent_dir)?;
            write_image(&image, &out)?;
            println!("built {}", image.manifest.content_hash);
        }
        Command::Image {
            command: ImageCommand::Verify { image_dir },
        } => {
            let image = load_image(image_dir)?;
            println!("verified {}", image.content_hash);
        }
        Command::Release {
            command: ReleaseCommand::Keygen { private_key_out },
        } => {
            let private_key = Zeroizing::new(generate_private_key_pkcs8()?);
            let public_key = public_key_hex_from_private_key(&private_key)?;
            write_new_file(&private_key_out, &private_key, true)?;
            println!("public_key_hex={public_key}");
        }
        Command::Release {
            command: ReleaseCommand::PublicKey { private_key },
        } => {
            let private_key = read_private_key(&private_key)?;
            println!("{}", public_key_hex_from_private_key(&private_key)?);
        }
        Command::Release {
            command:
                ReleaseCommand::Sign {
                    descriptor,
                    private_key,
                    key_id,
                    sequence,
                    issued_at_unix_seconds,
                    expires_at_unix_seconds,
                    runtime_version,
                    kernel_version,
                    model_id,
                    out,
                },
        } => {
            let descriptor = read_canonical_descriptor(&descriptor)?;
            let private_key = read_private_key(&private_key)?;
            let model_id = release_model_id(&descriptor, model_id.as_deref())?;
            let authorization = sign(
                ReleaseAuthorizationPayloadV1 {
                    schema_version: RELEASE_AUTHORIZATION_SCHEMA_VERSION,
                    key_id,
                    sequence,
                    issued_at_unix_seconds,
                    expires_at_unix_seconds,
                    release_descriptor_hash: public_descriptor_hash(&descriptor)?,
                    release_set_hash: descriptor.release_set_hash.clone(),
                    runtime_version,
                    kernel_version,
                    model_id,
                },
                &private_key,
            )?;
            let bytes = canonical_authorization_bytes(&authorization)?;
            write_new_file(&out, &bytes, false)?;
            println!("signed {}", public_descriptor_hash(&descriptor)?);
        }
        Command::Release {
            command:
                ReleaseCommand::Verify {
                    descriptor,
                    authorization,
                    trust_registry,
                    runtime_version,
                    kernel_version,
                    now_unix_seconds,
                },
        } => {
            let descriptor = read_canonical_descriptor(&descriptor)?;
            let authorization =
                parse_canonical_authorization(&read_regular_file(&authorization, 64 * 1024)?)?;
            let trust =
                parse_canonical_trust_registry(&read_regular_file(&trust_registry, 64 * 1024)?)?;
            verify_for_descriptor(
                &authorization,
                &trust,
                &descriptor,
                VerificationContext {
                    runtime_version: &runtime_version,
                    kernel_version: &kernel_version,
                    now_unix_seconds: now_unix_seconds.unwrap_or(current_unix_seconds()?),
                },
            )?;
            println!("verified {}", public_descriptor_hash(&descriptor)?);
        }
        Command::Provider {
            command: ProviderCommand::Probe { provider },
        } => {
            let report = probe_provider(provider).await?;
            println!("{}", serde_json::to_string(&report)?);
        }
        Command::Provider {
            command: ProviderCommand::StructuredProbe,
        } => {
            let report = probe_glm_structured_admission().await?;
            println!("{}", serde_json::to_string(&report)?);
        }
        #[cfg(feature = "dev-tools")]
        Command::Quality {
            command: QualityCommand::Check { suite, root },
        } => {
            let loaded = load_suite(&root, &suite)?;
            println!(
                "{}",
                serde_json::json!({
                    "schema_version": 4,
                    "suite_id": loaded.suite_id,
                    "suite_version": loaded.suite_version,
                    "case_count": loaded.cases.len(),
                    "cases_hash": loaded.cases_hash
                })
            );
        }
        #[cfg(feature = "dev-tools")]
        Command::Quality {
            command:
                QualityCommand::Run {
                    suite,
                    case,
                    root,
                    answer_out,
                    report_out,
                },
        } => {
            let loaded = load_suite(&root, &suite)?;
            let run = run_fixture_case_with_model(
                &root,
                &loaded,
                &case,
                Arc::new(glm_52_client()?),
                answer_out.is_some(),
                GLM_MODEL_ID,
                "glm_high",
            )
            .await?;
            println!("{}", serde_json::to_string_pretty(&run.report)?);
            if let Some(report_out) = report_out {
                let report = serde_json::to_vec_pretty(&run.report)?;
                write_new_file(&report_out, &report, true)?;
            }
            if let Some(answer_out) = answer_out {
                let answer = run.rendered_answer.as_deref().ok_or_else(|| {
                    std::io::Error::other("quality run did not produce a rendered answer")
                })?;
                write_new_file(&answer_out, answer.as_bytes(), true)?;
            }
            if !run.report.passed {
                return Err(std::io::Error::other("research-quality gate failed").into());
            }
        }
        #[cfg(feature = "dev-tools")]
        Command::Quality {
            command:
                QualityCommand::Replay {
                    suite,
                    case,
                    root,
                    answer_out,
                    report_out,
                },
        } => {
            let loaded = load_suite(&root, &suite)?;
            let run =
                run_recorded_fixture_case(&root, &loaded, &case, answer_out.is_some()).await?;
            println!("{}", serde_json::to_string_pretty(&run.report)?);
            if let Some(report_out) = report_out {
                let report = serde_json::to_vec_pretty(&run.report)?;
                write_new_file(&report_out, &report, true)?;
            }
            if let Some(answer_out) = answer_out {
                let answer = run.rendered_answer.as_deref().ok_or_else(|| {
                    std::io::Error::other("quality replay did not produce a rendered answer")
                })?;
                write_new_file(&answer_out, answer.as_bytes(), true)?;
            }
            if !run.report.passed {
                return Err(std::io::Error::other("research-quality replay gate failed").into());
            }
        }
        #[cfg(feature = "dev-tools")]
        Command::Quickstart { fixture, root } => {
            if fixture != "vertical-slice" {
                return Err(format!("unknown fixture: {fixture}").into());
            }
            let report = krw_agent_test_support::run_vertical_slice(
                root.join("agents/krw-ontology"),
                root.join("fixtures/vertical-slice/v1"),
            )?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        Command::Run {
            question,
            ticker,
            session_id,
            gateway_url,
            token_env,
            wait,
            wait_timeout_seconds,
            json,
        } => {
            run_through_gateway(
                &question,
                &ticker,
                session_id.as_deref(),
                &gateway_url,
                &token_env,
                wait,
                Duration::from_secs(wait_timeout_seconds),
                json,
            )
            .await?;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn run_through_gateway(
    question: &str,
    ticker: &str,
    session_id: Option<&str>,
    gateway_url: &str,
    token_env: &str,
    wait: bool,
    wait_timeout: Duration,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if wait_timeout.is_zero() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "wait timeout must be greater than zero",
        )
        .into());
    }
    let token = Zeroizing::new(std::env::var(token_env).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!(
                "{token_env} is not present; set a gateway CLI token in the process environment"
            ),
        )
    })?);
    let client = AgentGatewayClient::new(gateway_url, token.to_string())?;
    let submitted = client
        .submit_company_research(question, ticker, session_id)
        .await?;
    if !wait {
        if json {
            let output = serde_json::json!({
                "schema_version": 1,
                "session_id": submitted.session_id,
                "run_id": submitted.run_id,
                "state": submitted.state,
            });
            println!("{}", serde_json::to_string(&output)?);
        } else {
            println!(
                "session_id={} run_id={} state={}",
                submitted.session_id,
                submitted.run_id,
                submitted.state.as_str()
            );
        }
        return Ok(());
    }

    let terminal = client
        .wait_for_terminal(&submitted.run_id, wait_timeout)
        .await?;
    match terminal.state {
        GatewayRunState::Final => {
            let final_output = terminal.final_output.ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "gateway reported final without final Markdown",
                )
            })?;
            if json {
                let usage = terminal.usage.ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "gateway reported final without credit usage",
                    )
                })?;
                println!(
                    "{}",
                    serde_json::to_string(&serde_json::json!({
                        "schema_version": 1,
                        "session_id": terminal.session_id,
                        "run_id": terminal.run_id,
                        "state": terminal.state,
                        "final_output_hash": final_output.final_output_hash,
                        "markdown": final_output.markdown,
                        "visualizations": final_output.visualizations,
                        "usage": usage,
                    }))?
                );
            } else {
                eprintln!(
                    "session_id={} run_id={} state=final",
                    terminal.session_id, terminal.run_id
                );
                println!("{}", final_output.markdown);
            }
            Ok(())
        }
        GatewayRunState::Failed => {
            let retry = terminal.retry_message.ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "gateway reported failed without retry Markdown",
                )
            })?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string(&serde_json::json!({
                        "schema_version": 1,
                        "session_id": terminal.session_id,
                        "run_id": terminal.run_id,
                        "state": terminal.state,
                        "retry_message": {
                            "markdown": retry.markdown,
                            "category": retry.category,
                            "retry_recommended": retry.retry_recommended,
                        },
                        "usage": terminal.usage,
                    }))?
                );
            } else {
                eprintln!(
                    "session_id={} run_id={} state=failed",
                    terminal.session_id, terminal.run_id
                );
                println!("{}", retry.markdown);
            }
            Ok(())
        }
        GatewayRunState::Cancelled => {
            Err(std::io::Error::other("gateway reported a cancelled terminal run").into())
        }
        GatewayRunState::Queued | GatewayRunState::Deferred | GatewayRunState::Active => {
            Err(std::io::Error::other("gateway returned a non-terminal state after wait").into())
        }
    }
}

async fn probe_provider(
    provider: ProviderProbeArg,
) -> Result<ProviderProbeReport, Box<dyn std::error::Error>> {
    let client = provider_client(provider)?;
    let request = provider_probe_request(provider.model_id());
    let started = Instant::now();
    let episode = client
        .complete_stream(&request, &provider_probe_context())
        .await?;
    episode.verify_model_identity()?;
    if !episode.assistant.tool_calls.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "provider probe unexpectedly returned tool calls",
        )
        .into());
    }

    Ok(ProviderProbeReport {
        schema_version: 1,
        model_id: provider.model_id().to_owned(),
        observed_model: episode.observed_model,
        finish_reason: episode.finish_reason,
        tool_call_count: episode.assistant.tool_calls.len(),
        prompt_tokens: episode.usage.prompt_tokens,
        completion_tokens: episode.usage.completion_tokens,
        total_tokens: episode.usage.total_tokens,
        elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        request_hash: episode.request_hash,
        replay_hash: episode.replay_hash,
    })
}

async fn probe_glm_structured_admission()
-> Result<ProviderStructuredAdmissionReport, Box<dyn std::error::Error>> {
    let client = provider_client(ProviderProbeArg::Glm)?;
    let baseline = probe_glm_admission_case(
        &client,
        provider_probe_request(GLM_MODEL_ID),
        ProviderAdmissionExpectation::NoTools,
    )
    .await;
    let json_object_output = probe_glm_admission_case(
        &client,
        provider_json_object_probe_request(),
        ProviderAdmissionExpectation::JsonStatus,
    )
    .await;
    let strict_tool_input = probe_glm_admission_case(
        &client,
        provider_strict_tool_probe_request()?,
        ProviderAdmissionExpectation::StrictTransition,
    )
    .await;
    Ok(ProviderStructuredAdmissionReport {
        schema_version: 1,
        model_id: GLM_MODEL_ID.to_owned(),
        baseline,
        json_object_output,
        strict_tool_input,
    })
}

#[derive(Debug, Clone, Copy)]
enum ProviderAdmissionExpectation {
    NoTools,
    JsonStatus,
    StrictTransition,
}

async fn probe_glm_admission_case(
    client: &ProviderClient,
    request: MessagesRequest,
    expectation: ProviderAdmissionExpectation,
) -> ProviderAdmissionResult {
    let started = Instant::now();
    match client
        .complete_stream(&request, &provider_probe_context())
        .await
    {
        Ok(episode) => {
            let identity_ok = episode.verify_model_identity().is_ok();
            let accepted = identity_ok
                && match expectation {
                    ProviderAdmissionExpectation::NoTools => {
                        episode.finish_reason == "stop" && episode.assistant.tool_calls.is_empty()
                    }
                    ProviderAdmissionExpectation::JsonStatus => {
                        episode.finish_reason == "stop"
                            && episode.assistant.tool_calls.is_empty()
                            && episode
                                .assistant
                                .content
                                .as_deref()
                                .and_then(|content| {
                                    serde_json::from_str::<serde_json::Value>(content).ok()
                                })
                                .and_then(|value| {
                                    value
                                        .get("status")
                                        .and_then(serde_json::Value::as_str)
                                        .map(|status| status == "ok")
                                })
                                .unwrap_or(false)
                    }
                    ProviderAdmissionExpectation::StrictTransition => {
                        let [call] = episode.assistant.tool_calls.as_slice() else {
                            return ProviderAdmissionResult {
                                accepted: false,
                                observed_model: Some(episode.observed_model),
                                finish_reason: Some(episode.finish_reason),
                                tool_call_count: episode.assistant.tool_calls.len(),
                                elapsed_ms: u64::try_from(started.elapsed().as_millis())
                                    .unwrap_or(u64::MAX),
                                error_kind: Some("strict_tool_shape".into()),
                                request_id_hash: None,
                                retry_after_ms: None,
                            };
                        };
                        episode.finish_reason == "tool_calls"
                            && call.function.name.as_str() == "krw_agent_transition"
                            && serde_json::from_str::<serde_json::Value>(&call.function.arguments)
                                .ok()
                                .and_then(|value| {
                                    value
                                        .get("event")
                                        .and_then(serde_json::Value::as_str)
                                        .map(|event| event == "probe_ok")
                                })
                                .unwrap_or(false)
                    }
                };
            ProviderAdmissionResult {
                accepted,
                observed_model: Some(episode.observed_model),
                finish_reason: Some(episode.finish_reason),
                tool_call_count: episode.assistant.tool_calls.len(),
                elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                error_kind: (!accepted).then_some("provider_contract_rejected".into()),
                request_id_hash: None,
                retry_after_ms: None,
            }
        }
        Err(error) => {
            let (request_id_hash, retry_after_ms) = match &error {
                WireError::ApiStatus {
                    request_id_hash,
                    retry_after_ms,
                    ..
                } => (request_id_hash.clone(), *retry_after_ms),
                _ => (None, None),
            };
            ProviderAdmissionResult {
                accepted: false,
                observed_model: None,
                finish_reason: None,
                tool_call_count: 0,
                elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                error_kind: Some(provider_probe_error_kind(&error).into()),
                request_id_hash,
                retry_after_ms,
            }
        }
    }
}

fn provider_json_object_probe_request() -> MessagesRequest {
    MessagesRequest {
        model: GLM_MODEL_ID.to_owned(),
        messages: vec![ProviderMessage::user(
            "Return exactly {\"status\":\"ok\"} as raw JSON. Do not use Markdown fences.",
        )],
        system: "Return exactly one raw JSON object and nothing else. Do not use Markdown fences, prose, or code blocks.".into(),
        max_tokens: 64,
        tools: Vec::new(),
        tool_choice: None,
        output_config: None,
        response_format: Some(ResponseFormat::json_object()),
        thinking: ThinkingConfig {
            kind: ThinkingMode::Disabled,
            budget_tokens: None,
        },
        stream: true,
        metadata: None,
    }
}

fn provider_strict_tool_probe_request() -> Result<MessagesRequest, Box<dyn std::error::Error>> {
    let tool_name = ProviderFunctionName::parse("krw_agent_transition")?;
    let tool = ProviderToolDefinition::new(
        tool_name.as_str(),
        "Select the requested local transition.",
        serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["event"],
            "properties": {"event": {"type": "string", "enum": ["probe_ok"]}}
        }),
    )?
    .with_strict();
    Ok(MessagesRequest {
        model: GLM_MODEL_ID.to_owned(),
        messages: vec![ProviderMessage::user(
            "Call krw_agent_transition exactly once with event probe_ok.",
        )],
        system: "KRW GLM strict tool-input admission probe".into(),
        max_tokens: 128,
        tools: vec![tool],
        tool_choice: Some(ToolChoice::Tool { name: tool_name }),
        output_config: None,
        response_format: None,
        thinking: ThinkingConfig {
            kind: ThinkingMode::Disabled,
            budget_tokens: None,
        },
        stream: true,
        metadata: None,
    })
}

fn provider_probe_error_kind(error: &WireError) -> &'static str {
    match error {
        WireError::ApiStatus { status, .. } => match status {
            400 | 422 => "provider_contract_rejected",
            401 | 403 => "provider_auth_rejected",
            429 => "provider_rate_limited",
            500 | 503 => "provider_unavailable",
            _ => "provider_http_error",
        },
        WireError::InvalidEndpoint => "invalid_endpoint",
        WireError::InvalidAuthorization => "invalid_authorization",
        WireError::UnknownModel(_) => "unknown_model",
        WireError::UnexpectedContentType => "unexpected_content_type",
        WireError::MissingMessageStop | WireError::MissingStopReason => "incomplete_stream",
        _ => "provider_wire_error",
    }
}

fn provider_client(
    provider: ProviderProbeArg,
) -> Result<ProviderClient, Box<dyn std::error::Error>> {
    let key_env = provider.api_key_env();
    let api_key = Zeroizing::new(std::env::var(key_env).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("{key_env} is not present; inject it through the process environment"),
        )
    })?);
    if api_key.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{key_env} is empty"),
        )
        .into());
    }

    let client = ProviderClient::new(
        ProviderClientConfig::production(
            provider.kind(),
            provider.api_base(),
            [provider.model_id().to_owned()],
            8,
        ),
        api_key.as_str(),
    )?;
    Ok(client)
}

/// The fixture-backed quality command remains intentionally GLM-only. Live
/// DeepSeek quality is exercised through the deployed Gateway acceptance
/// runner, where the exact sealed release descriptor is also verified.
///
/// Dev-only, like the `quality` subcommand that calls it.
#[cfg(feature = "dev-tools")]
fn glm_52_client() -> Result<ProviderClient, Box<dyn std::error::Error>> {
    provider_client(ProviderProbeArg::Glm)
}

fn provider_probe_request(model_id: &str) -> MessagesRequest {
    MessagesRequest {
        model: model_id.to_owned(),
        messages: vec![ProviderMessage::user(PROVIDER_PROBE_PROMPT)],
        system: "KRW provider probe".into(),
        max_tokens: PROVIDER_PROBE_MAX_TOKENS,
        tools: Vec::new(),
        tool_choice: None,
        output_config: None,
        response_format: None,
        thinking: ThinkingConfig {
            kind: ThinkingMode::Disabled,
            budget_tokens: None,
        },
        stream: true,
        metadata: None,
    }
}

fn provider_probe_context() -> EpisodeContext {
    EpisodeContext {
        tool_schema_hash: ContentHash::sha256("krw-agent-provider-probe-tool-schema/v1"),
        agent_image_hash: ContentHash::sha256("krw-agent-provider-probe-image/v1"),
        api_version: "anthropic-messages-v1".into(),
    }
}

fn current_unix_seconds() -> Result<u64, std::io::Error> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|_| std::io::Error::other("system clock is before Unix epoch"))
}

fn release_model_id(
    descriptor: &PublicReleaseDescriptor,
    requested: Option<&str>,
) -> Result<String, Box<dyn std::error::Error>> {
    if let Some(model_id) = requested {
        if !ALLOWED_MODEL_IDS.contains(&model_id) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("model_id is not an admitted provider model: {model_id}"),
            )
            .into());
        }
        return Ok(model_id.to_owned());
    }

    let model_ids = descriptor
        .entries
        .iter()
        .map(|entry| entry.execution.resolved_model.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    if model_ids.is_empty() {
        // Empty descriptors are only useful for offline authorization unit
        // fixtures; keep their historical default while live daemon startup
        // still binds the payload to the selected registry model.
        return Ok(GLM_MODEL_ID.to_owned());
    }
    if model_ids.len() != 1 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "descriptor contains multiple models; pass --model-id explicitly",
        )
        .into());
    }
    Ok((*model_ids.iter().next().expect("one model id")).to_owned())
}

fn read_canonical_descriptor(
    path: &Path,
) -> Result<PublicReleaseDescriptor, Box<dyn std::error::Error>> {
    let bytes = read_regular_file(path, MAX_DESCRIPTOR_BYTES)?;
    let text = std::str::from_utf8(&bytes)?;
    let descriptor: PublicReleaseDescriptor = serde_json::from_str(text)?;
    if descriptor.schema_version != PUBLIC_RELEASE_DESCRIPTOR_SCHEMA_VERSION
        || descriptor.entries.is_empty()
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "release descriptor is not a populated current-version descriptor",
        )
        .into());
    }
    if serde_jcs::to_vec(&descriptor)? != bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "release descriptor is not canonical JCS",
        )
        .into());
    }
    Ok(descriptor)
}

fn read_private_key(path: &Path) -> Result<Zeroizing<Vec<u8>>, Box<dyn std::error::Error>> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "private key must be a regular non-symlink file",
        )
        .into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.mode() & 0o077 != 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "private key must not be readable or writable by group/other",
            )
            .into());
        }
    }
    let bytes = read_regular_file(path, MAX_PRIVATE_KEY_BYTES)?;
    Ok(Zeroizing::new(bytes))
}

fn read_regular_file(path: &Path, max_bytes: usize) -> Result<Vec<u8>, std::io::Error> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "input must be a regular non-symlink file",
        ));
    }
    let length = usize::try_from(metadata.len()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "input size overflows platform",
        )
    })?;
    if length == 0 || length > max_bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "input size is outside the allowed bound",
        ));
    }
    fs::read(path)
}

fn write_new_file(path: &Path, bytes: &[u8], private: bool) -> Result<(), std::io::Error> {
    if bytes.is_empty() || path.as_os_str().is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "output path or content is invalid",
        ));
    }
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "output has no parent directory",
        )
    })?;
    let parent_metadata = fs::metadata(parent)?;
    if !parent_metadata.is_dir() || fs::symlink_metadata(path).is_ok() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "output must be a new file under an existing directory",
        ));
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(if private { 0o600 } else { 0o640 });
    }
    let mut output: File = options.open(path)?;
    output.write_all(bytes)?;
    output.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        GLM_MODEL_ID, PROVIDER_PROBE_MAX_TOKENS, PROVIDER_PROBE_PROMPT, ProviderProbeArg,
        provider_probe_context, provider_probe_request,
    };
    use krw_agent_protocol::ThinkingMode;
    use krw_agent_provider_wire::ProviderMessage;

    #[test]
    fn provider_probe_is_exact_for_each_admitted_provider_and_has_no_tools() {
        for model_id in [GLM_MODEL_ID, ProviderProbeArg::Deepseek.model_id()] {
            let request = provider_probe_request(model_id);
            assert_eq!(request.model, model_id);
            assert!(request.stream);
            assert!(request.tools.is_empty());
            assert_eq!(request.thinking.kind, ThinkingMode::Disabled);
            assert_eq!(request.max_tokens, PROVIDER_PROBE_MAX_TOKENS);
            assert_eq!(
                request.messages,
                vec![ProviderMessage::user(PROVIDER_PROBE_PROMPT)]
            );
        }
        assert_ne!(
            provider_probe_context().tool_schema_hash,
            provider_probe_context().agent_image_hash
        );
    }

    #[test]
    fn provider_probe_selector_uses_closed_provider_endpoints_and_keys() {
        assert_eq!(ProviderProbeArg::Glm.model_id(), GLM_MODEL_ID);
        assert_eq!(
            ProviderProbeArg::Glm.api_base(),
            "https://api.z.ai/api/anthropic"
        );
        assert_eq!(ProviderProbeArg::Glm.api_key_env(), "GLM_API_KEY");
        assert_eq!(
            ProviderProbeArg::Deepseek.model_id(),
            krw_agent_protocol::DEEPSEEK_MODEL_ID
        );
        assert_eq!(
            ProviderProbeArg::Deepseek.api_base(),
            "https://api.deepseek.com/anthropic"
        );
        assert_eq!(ProviderProbeArg::Deepseek.api_key_env(), "DEEPSEEK_API_KEY");
    }
}
