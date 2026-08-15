//! Offline performance and soak gates for the bounded KRW agent runtime.
//!
//! The binary is intentionally not linked into `krw-agentd`. Its process-wide
//! allocator instrumentation exists only to measure live allocations in this
//! disposable harness.

use std::alloc::System;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use clap::{Parser, ValueEnum};
use krw_agent_bounded_child::{
    CancelChildMutation, ChildExecutionError, ChildExecutionReceipt, CompleteChildMutation,
    InvokeChildMutation, ReserveChildMutation,
};
use krw_agent_evidence::{
    Answerability, Directness, EvidenceGrade, EvidenceRecord, EvidenceScope, EvidenceSource,
    NormalizedFact, PublicCitation,
};
use krw_agent_image::{AgentImageManifest, LoadedImage, compile_agent_dir};
use krw_agent_persistence::{
    ActionDisposition, ActionFinalizationReceipt, ActionReceipt, ActionStage,
    FinalizeActionMutation,
};
use krw_agent_protocol::{
    AuthScope, BudgetLimits, CapabilityBinding, ContentHash, DeploymentBinding,
    McpToolSessionReuse, PROTOCOL_VERSION, ProviderWireCapabilities, ReasoningEffort,
    ResolvedExecutionSnapshot, RunContextV1, RunRequest, ThinkingMode, provider_tool_name,
};
#[cfg(test)]
use krw_agent_provider_wire::ToolResultMessage;
use krw_agent_provider_wire::{
    AssistantMessage, ContentBlock, EpisodeContext, FunctionCall, MessagesRequest,
    ProviderEpisodeV1, ProviderFunctionName, ProviderMessage, TokenUsage, ToolCall, ToolCallKind,
    ToolChoice,
};
use krw_agent_run_engine::{
    ActionIntent, CapabilityInvocation, CapabilityResult, CapabilityRuntime, DeliveryCertainty,
    DependencyFailure, DurableActionObservation, DurableEpisode, DurableFinal, DurableRunState,
    EngineConfig, FinalStatus, MarkActionAmbiguous, Persistence, Provider, RecoverySnapshot,
    RunControl, RunEngine, RunIdentity, RunInput,
};
use krw_agent_scheduler::{AdmissionItem, FairScheduler};
use krw_agent_test_support::run_vertical_slice;
use krw_agent_tool_mcp::{McpClientPool, McpError, McpHttpConfig, PoolKey};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use stats_alloc::{INSTRUMENTED_SYSTEM, StatsAlloc};
use thiserror::Error;
use tokio::sync::{Barrier, Mutex as AsyncMutex, Semaphore};

const MAX_RUN_QUESTION_BYTES: usize = 64 * 1024;
const ACTIVE_MEASUREMENT_PHASE: ActiveMeasurementPhase =
    ActiveMeasurementPhase::SecondProviderWaitAfterAcceptedCapability;

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

#[derive(Debug, Parser)]
#[command(name = "krw-agent-perf-harness")]
#[command(about = "Offline bounded-memory, load and soak release gates")]
struct Args {
    #[arg(long, default_value = "perf/release-gates.json")]
    thresholds: PathBuf,
    #[arg(long, default_value = "ci")]
    profile: String,
    #[arg(long, default_value = ".")]
    root: PathBuf,
    #[arg(long, value_enum, default_value_t = Scenario::All)]
    scenario: Scenario,
    #[arg(long)]
    output: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Serialize)]
#[serde(rename_all = "kebab-case")]
enum Scenario {
    All,
    Scheduler,
    ActiveRuns,
    McpPool,
    Soak,
    ChatMatrix,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum ActiveMeasurementPhase {
    SecondProviderWaitAfterAcceptedCapability,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GateConfig {
    schema_version: u16,
    profiles: BTreeMap<String, GateProfile>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct GateProfile {
    performance_authority: bool,
    resident_window: usize,
    scheduler_operations: usize,
    scheduler_p95_ns_max: u64,
    queued_10000_live_bytes_max: u64,
    queued_100000_live_bytes_max: u64,
    queued_10000_rss_bytes_max: u64,
    queued_100000_rss_bytes_max: u64,
    active_counts: Vec<usize>,
    active_retained_payload_bytes: usize,
    active_heap_slope_bytes_per_run_max: u64,
    active_working_set_bytes_per_run_max: u64,
    active_cleanup_live_bytes_max: u64,
    active_cleanup_fd_growth_max: i64,
    mcp_pool_entries_max: usize,
    mcp_singleflight_parallelism: usize,
    soak_warmup_iterations: usize,
    soak_min_iterations: usize,
    soak_min_duration_seconds: u64,
    soak_max_duration_seconds: u64,
    soak_pause_millis: u64,
    soak_active_wave_every_iterations: usize,
    soak_active_wave_runs: usize,
    soak_live_growth_bytes_max: u64,
    soak_rss_growth_bytes_max: u64,
    soak_rss_growth_percent_max: f64,
    soak_fd_growth_max: i64,
    soak_thread_growth_max: i64,
}

impl GateProfile {
    fn validate(&self) -> Result<(), HarnessError> {
        if self.resident_window == 0
            || self.resident_window > 4_096
            || self.scheduler_operations == 0
            || self.scheduler_operations > 10_000_000
            || self.active_counts.is_empty()
            || self.active_counts.contains(&0)
            || self.active_counts.iter().any(|count| *count > 64)
            || self.active_retained_payload_bytes == 0
            || self.active_retained_payload_bytes > 16 * 1024 * 1024
            || self.mcp_pool_entries_max == 0
            || self.mcp_pool_entries_max > 1_024
            || self.mcp_singleflight_parallelism == 0
            || self.mcp_singleflight_parallelism > 4_096
            || self.soak_warmup_iterations == 0
            || self.soak_warmup_iterations > 100_000
            || self.soak_min_iterations == 0
            || self.soak_min_iterations > 10_000_000
            || self.soak_min_duration_seconds > self.soak_max_duration_seconds
            || self.soak_max_duration_seconds > 7 * 24 * 60 * 60
            || self.soak_pause_millis > 10_000
            || self.soak_active_wave_every_iterations == 0
            || self.soak_active_wave_every_iterations > self.soak_min_iterations
            || self.soak_active_wave_runs == 0
            || self.soak_active_wave_runs > 64
            || !self.soak_rss_growth_percent_max.is_finite()
        {
            return Err(HarnessError::InvalidThresholds);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum GateStatus {
    Pass,
    Fail,
}

#[derive(Debug, Clone, Serialize)]
struct GateResult {
    id: String,
    status: GateStatus,
    observed: Value,
    limit: Value,
    comparator: String,
    unit: String,
    detail: String,
}

impl GateResult {
    fn maximum(id: &str, observed: u64, limit: u64, unit: &str, detail: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            status: if observed <= limit {
                GateStatus::Pass
            } else {
                GateStatus::Fail
            },
            observed: json!(observed),
            limit: json!(limit),
            comparator: "<=".into(),
            unit: unit.into(),
            detail: detail.into(),
        }
    }

    fn maximum_i64(
        id: &str,
        observed: i64,
        limit: i64,
        unit: &str,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            status: if observed <= limit {
                GateStatus::Pass
            } else {
                GateStatus::Fail
            },
            observed: json!(observed),
            limit: json!(limit),
            comparator: "<=".into(),
            unit: unit.into(),
            detail: detail.into(),
        }
    }

    fn maximum_f64(
        id: &str,
        observed: f64,
        limit: f64,
        unit: &str,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            status: if observed <= limit {
                GateStatus::Pass
            } else {
                GateStatus::Fail
            },
            observed: json!(observed),
            limit: json!(limit),
            comparator: "<=".into(),
            unit: unit.into(),
            detail: detail.into(),
        }
    }

    fn exact(
        id: &str,
        observed: u64,
        expected: u64,
        unit: &str,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            status: if observed == expected {
                GateStatus::Pass
            } else {
                GateStatus::Fail
            },
            observed: json!(observed),
            limit: json!(expected),
            comparator: "==".into(),
            unit: unit.into(),
            detail: detail.into(),
        }
    }

    fn boolean(id: &str, observed: bool, detail: &str) -> Self {
        Self {
            id: id.into(),
            status: if observed {
                GateStatus::Pass
            } else {
                GateStatus::Fail
            },
            observed: json!(observed),
            limit: json!(true),
            comparator: "==".into(),
            unit: "boolean".into(),
            detail: detail.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
struct ProcessMetrics {
    live_allocated_bytes: u64,
    rss_bytes: Option<u64>,
    open_fds: Option<u64>,
    os_threads: Option<u64>,
}

#[derive(Debug, Serialize)]
struct SchedulerReport {
    resident_window: usize,
    attempted: usize,
    accepted: usize,
    rejected: usize,
    at_10000: ProcessMetrics,
    at_100000: ProcessMetrics,
    baseline: ProcessMetrics,
    operation_count: usize,
    operation_p95_ns: u64,
    operation_max_ns: u64,
}

#[derive(Debug, Serialize)]
struct ActiveLevel {
    runs: usize,
    measurement_phase: ActiveMeasurementPhase,
    configured_retained_payload_bytes_per_run: usize,
    observed_retained_bytes_per_run: usize,
    first_provider_entered: usize,
    first_provider_completed: usize,
    planner_provider_entered: usize,
    planner_provider_completed: usize,
    episode_checkpointed: usize,
    capability_entered: usize,
    capability_completed: usize,
    action_accepted: usize,
    run_state_checkpointed: usize,
    measurement_phase_entered: usize,
    measurement_phase_completed: usize,
    rejected_phase_entries: usize,
    baseline: ProcessMetrics,
    active: ProcessMetrics,
    cleaned: ProcessMetrics,
    live_delta_bytes: i64,
    rss_delta_bytes: Option<i64>,
    cleanup_live_delta_bytes: i64,
    cleanup_fd_delta: Option<i64>,
    spawned_tasks: usize,
    joined_tasks: usize,
}

#[derive(Debug, Serialize)]
struct ActiveRunReport {
    measurement_phase: ActiveMeasurementPhase,
    configured_retained_payload_bytes_per_run: usize,
    retained_bytes_per_run: usize,
    min_observed_retained_bytes_per_run: usize,
    max_observed_retained_bytes_per_run: usize,
    entered_runs: usize,
    completed_runs: usize,
    rejected_phase_entries: usize,
    levels: Vec<ActiveLevel>,
    live_heap_ols_slope_bytes_per_run: f64,
    live_heap_ols_r_squared: f64,
    max_live_heap_bytes_per_run: u64,
    max_rss_bytes_per_run: Option<u64>,
    max_cleanup_live_bytes: u64,
    max_cleanup_fd_growth: Option<i64>,
}

#[derive(Debug, Serialize)]
struct McpPoolReport {
    configured_cap: usize,
    same_key_parallel_requests: usize,
    same_key_entries: usize,
    same_key_initializing: usize,
    same_key_failed: usize,
    high_cardinality_attempts: usize,
    high_cardinality_entries: usize,
}

#[derive(Debug, Serialize)]
struct SoakSample {
    iteration: usize,
    elapsed_millis: u128,
    process: ProcessMetrics,
}

#[derive(Debug, Serialize)]
struct SoakReport {
    warmup_iterations: usize,
    completed_iterations: usize,
    elapsed_millis: u128,
    baseline: ProcessMetrics,
    final_metrics: ProcessMetrics,
    live_growth_bytes: i64,
    rss_growth_bytes: Option<i64>,
    rss_growth_percent: Option<f64>,
    fd_growth: Option<i64>,
    thread_growth: Option<i64>,
    active_measurement_phase: ActiveMeasurementPhase,
    active_configured_retained_payload_bytes_per_run: usize,
    active_min_observed_retained_bytes_per_run: usize,
    active_max_observed_retained_bytes_per_run: usize,
    active_measurement_phase_entered: usize,
    active_measurement_phase_completed: usize,
    active_rejected_phase_entries: usize,
    active_churn_waves: usize,
    active_churn_tasks_spawned: usize,
    active_churn_tasks_joined: usize,
    active_churn_max_cleanup_live_bytes: u64,
    active_churn_max_cleanup_fd_growth: Option<i64>,
    samples: Vec<SoakSample>,
}

#[derive(Debug, Serialize)]
struct ChatMatrixReport {
    principals: usize,
    rooms_per_principal: usize,
    turns_per_room: usize,
    logical_runs: usize,
    provider_turns: u64,
    capability_calls: u64,
    same_room_serialized: bool,
    turn_ordered: bool,
    same_principal_rooms_parallel: bool,
    principal_fair: bool,
    scope_isolated: bool,
    duplicate_run_rejected: bool,
    queue_only_principals: usize,
    queue_only_runs: usize,
    queue_only_tasks_spawned: usize,
    queue_only_provider_clients: usize,
    queue_only_mcp_clients: usize,
    queue_only_db_connections: usize,
    peak_active_rooms: usize,
    peak_active_per_principal: usize,
    rss_peak_by_active_rooms: BTreeMap<usize, Option<u64>>,
    enqueue_to_claim_p50_us: u64,
    enqueue_to_claim_p95_us: u64,
    enqueue_to_claim_p99_us: u64,
    end_to_end_deterministic_ms: u64,
}

#[derive(Debug, Clone)]
struct ChatMatrixJob {
    run_id: String,
    principal_id: String,
    session_id: String,
    turn: usize,
    provider_turns: u16,
    capability_calls: u16,
}

#[derive(Debug, Default)]
struct ChatMatrixObservation {
    active_by_room: BTreeMap<String, usize>,
    active_by_principal: BTreeMap<String, usize>,
    max_active_by_room: BTreeMap<String, usize>,
    max_active_by_principal: BTreeMap<String, usize>,
    completed_turns_by_room: BTreeMap<String, usize>,
    wait_micros: Vec<u128>,
    rss_peak_by_active_rooms: BTreeMap<usize, u64>,
    max_active_rooms: usize,
    same_room_serialized: bool,
    turn_ordered: bool,
    scope_isolated: bool,
}

#[derive(Debug, Serialize)]
struct EnvironmentReport {
    os: String,
    architecture: String,
    rust_debug_assertions: bool,
    logical_cpus: usize,
    epoch_seconds: u64,
}

#[derive(Debug, Serialize)]
struct PerformanceReport {
    schema_version: u16,
    harness_version: String,
    thresholds_hash: ContentHash,
    profile: String,
    scenario: Scenario,
    /// True only when the selected profile is long enough to be authoritative
    /// for the performance dimensions measured by this process.
    performance_authority: bool,
    /// This is deliberately narrower than a production release decision. The
    /// harness cannot attest tests or probes executed by another process.
    complete_performance_evidence: bool,
    status: String,
    environment: EnvironmentReport,
    scheduler: Option<SchedulerReport>,
    active_runs: Option<ActiveRunReport>,
    mcp_pool: Option<McpPoolReport>,
    soak: Option<SoakReport>,
    chat_matrix: Option<ChatMatrixReport>,
    gates: Vec<GateResult>,
    limitations: Vec<String>,
}

#[derive(Debug, Error)]
enum HarnessError {
    #[error("threshold file is invalid")]
    InvalidThresholds,
    #[error("unknown threshold profile: {0}")]
    UnknownProfile(String),
    #[error("I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("agent image failed: {0}")]
    Image(#[from] krw_agent_image::ImageError),
    #[error("scheduler failed: {0}")]
    Scheduler(#[from] krw_agent_scheduler::SchedulerError),
    #[error("MCP pool failed: {0}")]
    Mcp(#[from] McpError),
    #[error("active runs did not all reach the intended measurement phase: {0}")]
    ActiveStart(String),
    #[error("active-run fixture is invalid: {0}")]
    InvalidActiveFixture(&'static str),
    #[error("active run task failed to join: {0}")]
    Join(String),
    #[error("active run unexpectedly completed successfully")]
    UnexpectedActiveSuccess,
    #[error("vertical-slice soak failed: {0}")]
    VerticalSlice(String),
    #[error("release gate failed; report was written")]
    GateFailed,
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), HarnessError> {
    let args = Args::parse();
    let threshold_bytes = fs::read(&args.thresholds)?;
    let thresholds_hash = ContentHash::sha256(&threshold_bytes);
    let config: GateConfig = serde_json::from_slice(&threshold_bytes)?;
    if config.schema_version != 1 {
        return Err(HarnessError::InvalidThresholds);
    }
    let profile = config
        .profiles
        .get(&args.profile)
        .cloned()
        .ok_or_else(|| HarnessError::UnknownProfile(args.profile.clone()))?;
    profile.validate()?;

    let root = fs::canonicalize(&args.root)?;
    let mut gates = Vec::new();
    let scheduler = if matches!(args.scenario, Scenario::All | Scenario::Scheduler) {
        let report = run_scheduler(&profile)?;
        append_scheduler_gates(&mut gates, &report, &profile);
        Some(report)
    } else {
        None
    };

    let active_runs = if matches!(args.scenario, Scenario::All | Scenario::ActiveRuns) {
        let report = run_active_runs(&root, &profile).await?;
        append_active_gates(&mut gates, &report, &profile);
        Some(report)
    } else {
        None
    };

    let mcp_pool = if matches!(args.scenario, Scenario::All | Scenario::McpPool) {
        let report = run_mcp_pool(&profile).await?;
        append_mcp_gates(&mut gates, &report, &profile);
        Some(report)
    } else {
        None
    };

    let soak = if matches!(args.scenario, Scenario::All | Scenario::Soak) {
        let report = run_soak(&root, &profile).await?;
        append_soak_gates(&mut gates, &report, &profile);
        Some(report)
    } else {
        None
    };

    let chat_matrix = if matches!(args.scenario, Scenario::All | Scenario::ChatMatrix) {
        let report = run_chat_matrix().await?;
        append_chat_matrix_gates(&mut gates, &report);
        Some(report)
    } else {
        None
    };

    gates.push(GateResult::boolean(
        "optimized_release_build",
        !cfg!(debug_assertions),
        "performance evidence must be collected from cargo --release",
    ));

    let failed = gates
        .iter()
        .any(|gate| matches!(gate.status, GateStatus::Fail));
    let every_required_gate_passed = gates
        .iter()
        .all(|gate| matches!(gate.status, GateStatus::Pass));
    let complete_performance_evidence = profile.performance_authority
        && matches!(args.scenario, Scenario::All)
        && every_required_gate_passed;
    let report = PerformanceReport {
        schema_version: 1,
        harness_version: env!("CARGO_PKG_VERSION").into(),
        thresholds_hash,
        profile: args.profile,
        scenario: args.scenario,
        performance_authority: profile.performance_authority,
        complete_performance_evidence,
        status: if failed { "fail" } else { "pass" }.into(),
        environment: EnvironmentReport {
            os: std::env::consts::OS.into(),
            architecture: std::env::consts::ARCH.into(),
            rust_debug_assertions: cfg!(debug_assertions),
            logical_cpus: std::thread::available_parallelism()
                .map_or(1, std::num::NonZero::get),
            epoch_seconds: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        },
        scheduler,
        active_runs,
        mcp_pool,
        soak,
        chat_matrix,
        gates,
        limitations: vec![
            "This report is performance evidence only; it never attests workspace tests, database fault tests, secret rotation, or a production release decision.".into(),
            "No live DeepSeek request or production MCP endpoint is used.".into(),
            "CI is a bounded smoke profile; only the 6-hour and 7-day profiles can be authoritative for the performance dimensions measured here.".into(),
            "TypeScript/legacy process-tree ratios require a separate equal-workload A/B run.".into(),
            "RSS includes allocator and OS behavior; live allocation is reported separately.".into(),
            "The post-tool fixture uses public production run-engine ABIs and a compiled AgentImage, but replaces remote canonical output validation with the bounded structural guard.".into(),
        ],
    };
    let bytes = serde_json::to_vec_pretty(&report)?;
    if let Some(output) = args.output {
        atomic_write_report(&output, &bytes)?;
    }
    println!("{}", String::from_utf8_lossy(&bytes));
    if failed {
        return Err(HarnessError::GateFailed);
    }
    Ok(())
}

fn run_scheduler(profile: &GateProfile) -> Result<SchedulerReport, HarnessError> {
    let mut scheduler = FairScheduler::new(profile.resident_window, 4, 64)?;
    warm_allocator();
    let baseline = process_metrics();
    let mut accepted = 0_usize;
    let mut rejected = 0_usize;
    let mut at_10000 = baseline;
    let mut at_100000 = baseline;

    for index in 0..100_000_usize {
        let item = AdmissionItem {
            run_id: format!("queued-run-{index}"),
            session_id: format!("queued-session-{index}"),
            principal_id: format!("principal-{}", index % 64),
            estimated_cost: 1,
        };
        if scheduler.enqueue(item).is_ok() {
            accepted += 1;
        } else {
            rejected += 1;
        }
        if index + 1 == 10_000 {
            at_10000 = process_metrics();
        }
        if index + 1 == 100_000 {
            at_100000 = process_metrics();
        }
    }

    while let Some(item) = scheduler.pop_next() {
        scheduler.complete_session(&item.session_id);
    }

    let mut operation_ns = Vec::with_capacity(profile.scheduler_operations);
    for index in 0..profile.scheduler_operations {
        let started = Instant::now();
        scheduler.enqueue(AdmissionItem {
            run_id: format!("latency-run-{index}"),
            session_id: format!("latency-session-{index}"),
            principal_id: format!("latency-principal-{}", index % 64),
            estimated_cost: 1,
        })?;
        let admitted = scheduler
            .pop_next()
            .expect("single resident admission must be runnable");
        scheduler.complete_session(&admitted.session_id);
        operation_ns.push(nanos_u64(started.elapsed()));
    }
    operation_ns.sort_unstable();
    let p95_index = operation_ns.len().saturating_mul(95).div_ceil(100);
    let operation_p95_ns = operation_ns[p95_index.saturating_sub(1)];
    let operation_max_ns = operation_ns.last().copied().unwrap_or_default();

    Ok(SchedulerReport {
        resident_window: profile.resident_window,
        attempted: 100_000,
        accepted,
        rejected,
        at_10000,
        at_100000,
        baseline,
        operation_count: operation_ns.len(),
        operation_p95_ns,
        operation_max_ns,
    })
}

fn append_scheduler_gates(
    gates: &mut Vec<GateResult>,
    report: &SchedulerReport,
    profile: &GateProfile,
) {
    let queued_ten_thousand_heap = positive_delta(
        report.at_10000.live_allocated_bytes,
        report.baseline.live_allocated_bytes,
    );
    let queued_hundred_thousand_heap = positive_delta(
        report.at_100000.live_allocated_bytes,
        report.baseline.live_allocated_bytes,
    );
    gates.push(GateResult::maximum(
        "queued_10000_live_bytes",
        queued_ten_thousand_heap,
        profile.queued_10000_live_bytes_max,
        "bytes",
        "only the bounded resident admission window is retained",
    ));
    gates.push(GateResult::maximum(
        "queued_100000_live_bytes",
        queued_hundred_thousand_heap,
        profile.queued_100000_live_bytes_max,
        "bytes",
        "external DB queue cardinality must not become daemon state",
    ));
    append_optional_rss_gate(
        gates,
        "queued_10000_rss_bytes",
        report.at_10000.rss_bytes,
        report.baseline.rss_bytes,
        profile.queued_10000_rss_bytes_max,
    );
    append_optional_rss_gate(
        gates,
        "queued_100000_rss_bytes",
        report.at_100000.rss_bytes,
        report.baseline.rss_bytes,
        profile.queued_100000_rss_bytes_max,
    );
    gates.push(GateResult::maximum(
        "scheduler_operation_p95",
        report.operation_p95_ns,
        profile.scheduler_p95_ns_max,
        "nanoseconds",
        "enqueue, admit and complete without provider or DB latency",
    ));
    gates.push(GateResult::exact(
        "scheduler_resident_cap",
        report.accepted as u64,
        profile.resident_window as u64,
        "items",
        "all candidates after the resident window must be rejected",
    ));
}

const CHAT_MATRIX_PRINCIPALS: usize = 20;
const CHAT_MATRIX_ROOMS_PER_PRINCIPAL: usize = 3;
const CHAT_MATRIX_TURNS_PER_ROOM: usize = 2;
const CHAT_MATRIX_QUEUE_ONLY_PRINCIPALS: usize = 2_000;
const CHAT_MATRIX_GLOBAL_CONCURRENCY: usize = 8;
const CHAT_MATRIX_PROVIDER_TURNS_PER_RUN: u16 = 2;
const CHAT_MATRIX_CAPABILITY_CALLS_PER_RUN: u16 = 2;

async fn run_chat_matrix() -> Result<ChatMatrixReport, HarnessError> {
    let started = Instant::now();
    let logical_runs =
        CHAT_MATRIX_PRINCIPALS * CHAT_MATRIX_ROOMS_PER_PRINCIPAL * CHAT_MATRIX_TURNS_PER_ROOM;
    let mut jobs_by_run = BTreeMap::new();
    let mut run_ids = BTreeSet::new();
    let mut duplicate_run_rejected = true;
    for principal_index in 0..CHAT_MATRIX_PRINCIPALS {
        for room_index in 0..CHAT_MATRIX_ROOMS_PER_PRINCIPAL {
            let session_id = format!("session:principal-{principal_index}:room-{room_index}");
            for turn in 0..CHAT_MATRIX_TURNS_PER_ROOM {
                let job = ChatMatrixJob {
                    run_id: format!(
                        "run:principal-{principal_index}:room-{room_index}:turn-{turn}"
                    ),
                    principal_id: format!("principal-{principal_index}"),
                    session_id: session_id.clone(),
                    turn,
                    provider_turns: CHAT_MATRIX_PROVIDER_TURNS_PER_RUN,
                    capability_calls: CHAT_MATRIX_CAPABILITY_CALLS_PER_RUN,
                };
                if !run_ids.insert(job.run_id.clone()) {
                    duplicate_run_rejected = false;
                }
                jobs_by_run.insert(job.run_id.clone(), job);
            }
        }
    }

    // The queue keeps durable candidates cheap.  Only the bounded admission
    // order is promoted to the fake active workload below; no task, provider,
    // MCP client, or database connection is created for this queue-only case.
    let mut scheduler = FairScheduler::new(logical_runs, 1, 1)?;
    for job in jobs_by_run.values() {
        scheduler.enqueue(AdmissionItem {
            run_id: job.run_id.clone(),
            session_id: job.session_id.clone(),
            principal_id: job.principal_id.clone(),
            estimated_cost: 1,
        })?;
    }
    let mut admission_order = Vec::with_capacity(logical_runs);
    while let Some(item) = scheduler.pop_next() {
        let job =
            jobs_by_run
                .get(&item.run_id)
                .cloned()
                .ok_or(HarnessError::InvalidActiveFixture(
                    "chat matrix lost an admitted run",
                ))?;
        admission_order.push(job);
        scheduler.complete_session(&item.session_id);
    }
    if admission_order.len() != logical_runs {
        return Err(HarnessError::ActiveStart(
            "chat matrix did not admit every logical run".into(),
        ));
    }
    let principal_fair = admission_order
        .iter()
        .take(CHAT_MATRIX_PRINCIPALS)
        .map(|job| job.principal_id.as_str())
        .collect::<BTreeSet<_>>()
        .len()
        == CHAT_MATRIX_PRINCIPALS;

    let room_locks = admission_order
        .iter()
        .map(|job| job.session_id.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|session_id| (session_id, Arc::new(AsyncMutex::new(()))))
        .collect::<BTreeMap<_, _>>();
    let room_locks = Arc::new(room_locks);
    let turn_gates = admission_order
        .iter()
        .filter(|job| job.turn > 0)
        .map(|job| {
            (
                (job.session_id.clone(), job.turn),
                Arc::new(Semaphore::new(0)),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let turn_gates = Arc::new(turn_gates);
    let global_permits = Arc::new(Semaphore::new(CHAT_MATRIX_GLOBAL_CONCURRENCY));
    // Make the multi-room property deterministic.  The fair admission order
    // intentionally starts with different principals, so a tiny fake turn
    // can otherwise finish before a second room for the same principal is
    // admitted and the metric becomes scheduler-order dependent.  Holding a
    // pair of distinct rooms at a rendezvous proves that the runtime permits
    // same-principal parallelism while the exact-session lock still protects
    // each room independently.
    let same_principal_probe = Arc::new(Barrier::new(2));
    let observations = Arc::new(Mutex::new(ChatMatrixObservation {
        same_room_serialized: true,
        turn_ordered: true,
        scope_isolated: true,
        ..ChatMatrixObservation::default()
    }));
    let mut tasks = Vec::with_capacity(admission_order.len());
    for job in admission_order {
        let room_lock =
            room_locks
                .get(&job.session_id)
                .cloned()
                .ok_or(HarnessError::InvalidActiveFixture(
                    "chat matrix room lock is missing",
                ))?;
        let turn_gate = if job.turn > 0 {
            Some(
                turn_gates
                    .get(&(job.session_id.clone(), job.turn))
                    .cloned()
                    .ok_or(HarnessError::InvalidActiveFixture(
                        "chat matrix turn gate is missing",
                    ))?,
            )
        } else {
            None
        };
        let next_turn_gate = turn_gates
            .get(&(job.session_id.clone(), job.turn + 1))
            .cloned();
        let global_permits = Arc::clone(&global_permits);
        let same_principal_probe = Arc::clone(&same_principal_probe);
        let observations = Arc::clone(&observations);
        tasks.push(tokio::spawn(async move {
            let queued_at = Instant::now();
            if let Some(turn_gate) = turn_gate {
                turn_gate
                    .acquire()
                    .await
                    .expect("chat matrix turn gate remains open")
                    .forget();
            }
            let room_guard = room_lock.lock().await;
            let global_permit = global_permits
                .acquire()
                .await
                .expect("chat matrix global semaphore remains open");
            let wait_micros = queued_at.elapsed().as_micros();
            let active_rooms = {
                let mut state = observations
                    .lock()
                    .map_err(|_| "chat matrix observation lock poisoned")?;
                let room_active = {
                    let entry = state
                        .active_by_room
                        .entry(job.session_id.clone())
                        .or_default();
                    *entry += 1;
                    *entry
                };
                if room_active > 1 {
                    state.same_room_serialized = false;
                }
                let completed = state
                    .completed_turns_by_room
                    .get(&job.session_id)
                    .copied()
                    .unwrap_or_default();
                if completed != job.turn {
                    state.turn_ordered = false;
                }
                let principal_active = {
                    let entry = state
                        .active_by_principal
                        .entry(job.principal_id.clone())
                        .or_default();
                    *entry += 1;
                    *entry
                };
                let principal_peak = state
                    .max_active_by_principal
                    .entry(job.principal_id.clone())
                    .or_default();
                *principal_peak = (*principal_peak).max(principal_active);
                let room_peak = state
                    .max_active_by_room
                    .entry(job.session_id.clone())
                    .or_default();
                *room_peak = (*room_peak).max(room_active);
                let active_rooms = state
                    .active_by_room
                    .values()
                    .filter(|value| **value > 0)
                    .count();
                state.max_active_rooms = state.max_active_rooms.max(active_rooms);
                let expected_scope = job
                    .session_id
                    .strip_prefix("session:")
                    .is_some_and(|scope| scope.starts_with(&job.principal_id));
                if !expected_scope
                    || !job.run_id.contains(&job.principal_id)
                    || !job.session_id.contains("room-")
                {
                    state.scope_isolated = false;
                }
                active_rooms
            };
            if matches!(active_rooms, 1 | 4 | 16)
                && let Some(rss) = process_metrics().rss_bytes
            {
                let mut state = observations
                    .lock()
                    .map_err(|_| "chat matrix observation lock poisoned")?;
                state
                    .rss_peak_by_active_rooms
                    .entry(active_rooms)
                    .and_modify(|peak| *peak = (*peak).max(rss))
                    .or_insert(rss);
            }
            let is_same_principal_probe = job.principal_id == "principal-0"
                && job.turn == 0
                && (job.session_id.ends_with("room-0") || job.session_id.ends_with("room-1"));
            if is_same_principal_probe {
                same_principal_probe.wait().await;
            }
            // The fake executes the same configured provider/capability counts
            // as a normal two-turn room flow. It only removes network variance.
            for _ in 0..job.provider_turns {
                tokio::task::yield_now().await;
            }
            for _ in 0..job.capability_calls {
                tokio::task::yield_now().await;
            }
            {
                let mut state = observations
                    .lock()
                    .map_err(|_| "chat matrix observation lock poisoned")?;
                if let Some(active) = state.active_by_room.get_mut(&job.session_id) {
                    *active = active.saturating_sub(1);
                    if *active == 0 {
                        state.active_by_room.remove(&job.session_id);
                    }
                }
                if let Some(active) = state.active_by_principal.get_mut(&job.principal_id) {
                    *active = active.saturating_sub(1);
                    if *active == 0 {
                        state.active_by_principal.remove(&job.principal_id);
                    }
                }
                *state
                    .completed_turns_by_room
                    .entry(job.session_id.clone())
                    .or_default() += 1;
                state.wait_micros.push(wait_micros);
            }
            if let Some(next_turn_gate) = next_turn_gate {
                next_turn_gate.add_permits(1);
            }
            drop(global_permit);
            drop(room_guard);
            Ok::<(), &'static str>(())
        }));
    }
    for task in tasks {
        task.await
            .map_err(|error| HarnessError::Join(error.to_string()))?
            .map_err(|error| HarnessError::ActiveStart(error.into()))?;
    }

    let queue_only_runs = CHAT_MATRIX_QUEUE_ONLY_PRINCIPALS;
    let mut queue_only_ids = Vec::with_capacity(queue_only_runs);
    for principal_index in 0..queue_only_runs {
        queue_only_ids.push(format!("queued:principal-{principal_index}:run-1"));
    }
    let queue_only_tasks_spawned = 0;
    let queue_only_provider_clients = 0;
    let queue_only_mcp_clients = 0;
    let queue_only_db_connections = 0;
    if queue_only_ids.len() != queue_only_runs {
        return Err(HarnessError::ActiveStart(
            "chat matrix queue-only cardinality changed".into(),
        ));
    }

    let state = observations
        .lock()
        .map_err(|_| HarnessError::ActiveStart("chat matrix observation lock poisoned".into()))?;
    let wait_micros = state.wait_micros.clone();
    let mut sorted_waits = wait_micros.clone();
    sorted_waits.sort_unstable();
    let peak_active_per_principal = state
        .max_active_by_principal
        .values()
        .copied()
        .max()
        .unwrap_or_default();
    let peak_active_rooms = state.max_active_rooms;
    let same_principal_rooms_parallel = peak_active_per_principal >= 2;
    let mut rss_peak_by_active_rooms = BTreeMap::new();
    for room_count in [1_usize, 4, 16] {
        rss_peak_by_active_rooms.insert(
            room_count,
            state.rss_peak_by_active_rooms.get(&room_count).copied(),
        );
    }
    let mut p50_waits = sorted_waits.clone();
    let mut p95_waits = sorted_waits.clone();
    let wait_p50 = percentile_micros(&mut p50_waits, 50);
    let wait_p95 = percentile_micros(&mut p95_waits, 95);
    let wait_p99 = percentile_micros(&mut sorted_waits, 99);
    let same_room_serialized = state.same_room_serialized;
    let turn_ordered = state.turn_ordered;
    let scope_isolated = state.scope_isolated;
    drop(state);

    Ok(ChatMatrixReport {
        principals: CHAT_MATRIX_PRINCIPALS,
        rooms_per_principal: CHAT_MATRIX_ROOMS_PER_PRINCIPAL,
        turns_per_room: CHAT_MATRIX_TURNS_PER_ROOM,
        logical_runs,
        provider_turns: (logical_runs as u64) * u64::from(CHAT_MATRIX_PROVIDER_TURNS_PER_RUN),
        capability_calls: (logical_runs as u64) * u64::from(CHAT_MATRIX_CAPABILITY_CALLS_PER_RUN),
        same_room_serialized,
        turn_ordered,
        same_principal_rooms_parallel,
        principal_fair,
        scope_isolated,
        duplicate_run_rejected,
        queue_only_principals: CHAT_MATRIX_QUEUE_ONLY_PRINCIPALS,
        queue_only_runs,
        queue_only_tasks_spawned,
        queue_only_provider_clients,
        queue_only_mcp_clients,
        queue_only_db_connections,
        peak_active_rooms,
        peak_active_per_principal,
        rss_peak_by_active_rooms,
        enqueue_to_claim_p50_us: wait_p50,
        enqueue_to_claim_p95_us: wait_p95,
        enqueue_to_claim_p99_us: wait_p99,
        end_to_end_deterministic_ms: u64::try_from(started.elapsed().as_millis())
            .unwrap_or(u64::MAX),
    })
}

fn percentile_micros(values: &mut [u128], percentile: usize) -> u64 {
    if values.is_empty() {
        return 0;
    }
    values.sort_unstable();
    let index = values
        .len()
        .saturating_mul(percentile)
        .div_ceil(100)
        .saturating_sub(1);
    u64::try_from(values[index]).unwrap_or(u64::MAX)
}

fn append_chat_matrix_gates(gates: &mut Vec<GateResult>, report: &ChatMatrixReport) {
    gates.push(GateResult::boolean(
        "chat_same_room_serialized",
        report.same_room_serialized,
        "one active turn per exact chat-room session",
    ));
    gates.push(GateResult::boolean(
        "chat_turn_ordered",
        report.turn_ordered,
        "follow-up turns finish after the previous turn in the same room",
    ));
    gates.push(GateResult::boolean(
        "chat_same_principal_rooms_parallel",
        report.same_principal_rooms_parallel,
        "different rooms for one principal may progress concurrently",
    ));
    gates.push(GateResult::boolean(
        "chat_principal_fair",
        report.principal_fair,
        "the first fair-admission round includes every principal",
    ));
    gates.push(GateResult::boolean(
        "chat_scope_isolated",
        report.scope_isolated,
        "room and principal scope remain attached to every fake run",
    ));
    gates.push(GateResult::boolean(
        "chat_duplicate_run_rejected",
        report.duplicate_run_rejected,
        "the same product run identity cannot be admitted twice",
    ));
    gates.push(GateResult::exact(
        "chat_provider_turn_count",
        report.provider_turns,
        (report.logical_runs as u64) * u64::from(CHAT_MATRIX_PROVIDER_TURNS_PER_RUN),
        "turns",
        "fake workload preserves the configured provider-turn policy",
    ));
    gates.push(GateResult::exact(
        "chat_capability_call_count",
        report.capability_calls,
        (report.logical_runs as u64) * u64::from(CHAT_MATRIX_CAPABILITY_CALLS_PER_RUN),
        "calls",
        "fake workload preserves the configured capability-call policy",
    ));
    gates.push(GateResult::exact(
        "chat_queue_only_tasks",
        report.queue_only_tasks_spawned as u64,
        0,
        "tasks",
        "queued principals remain durable candidates until admission",
    ));
    gates.push(GateResult::exact(
        "chat_queue_only_provider_clients",
        report.queue_only_provider_clients as u64,
        0,
        "clients",
        "idle queued principals do not allocate provider clients",
    ));
}

async fn run_active_runs(
    root: &Path,
    profile: &GateProfile,
) -> Result<ActiveRunReport, HarnessError> {
    let image = Arc::new(compile_agent_dir(root.join("agents/krw-ontology"))?.into_loaded()?);
    let deployment = Arc::new(active_deployment());
    let workload = Arc::new(ActiveWorkload::load(
        root,
        profile.active_retained_payload_bytes,
    )?);

    let _ = measure_active_level(
        1,
        Arc::clone(&workload),
        Arc::clone(&image),
        Arc::clone(&deployment),
    )
    .await?;
    let mut levels = Vec::with_capacity(profile.active_counts.len());
    for &count in &profile.active_counts {
        levels.push(
            measure_active_level(
                count,
                Arc::clone(&workload),
                Arc::clone(&image),
                Arc::clone(&deployment),
            )
            .await?,
        );
    }
    let points = levels
        .iter()
        .map(|level| {
            (
                f64::from(u32::try_from(level.runs).unwrap_or(u32::MAX)),
                bounded_f64(positive_i64(level.live_delta_bytes)),
            )
        })
        .collect::<Vec<_>>();
    let (slope, r_squared) = ordinary_least_squares(&points);
    let max_live_heap_bytes_per_run = levels
        .iter()
        .map(|level| positive_i64(level.live_delta_bytes) / level.runs as u64)
        .max()
        .unwrap_or_default();
    let rss_values = levels
        .iter()
        .filter_map(|level| {
            level
                .rss_delta_bytes
                .map(|delta| positive_i64(delta) / level.runs as u64)
        })
        .collect::<Vec<_>>();
    let max_rss_bytes_per_run = rss_values.into_iter().max();
    let max_cleanup_live_bytes = levels
        .iter()
        .map(|level| positive_i64(level.cleanup_live_delta_bytes))
        .max()
        .unwrap_or_default();
    let cleanup_fd_values = levels
        .iter()
        .filter_map(|level| level.cleanup_fd_delta)
        .collect::<Vec<_>>();
    let max_cleanup_fd_growth = cleanup_fd_values.into_iter().max();
    let min_observed_retained_bytes_per_run = levels
        .iter()
        .map(|level| level.observed_retained_bytes_per_run)
        .min()
        .unwrap_or_default();
    let max_observed_retained_bytes_per_run = levels
        .iter()
        .map(|level| level.observed_retained_bytes_per_run)
        .max()
        .unwrap_or_default();
    let entered_runs = levels
        .iter()
        .map(|level| level.measurement_phase_entered)
        .sum();
    let completed_runs = levels
        .iter()
        .map(|level| level.measurement_phase_completed)
        .sum();
    let rejected_phase_entries = levels
        .iter()
        .map(|level| level.rejected_phase_entries)
        .sum();
    if min_observed_retained_bytes_per_run != max_observed_retained_bytes_per_run {
        return Err(HarnessError::ActiveStart(
            "accepted retained result size differs between concurrency levels".into(),
        ));
    }

    Ok(ActiveRunReport {
        measurement_phase: ACTIVE_MEASUREMENT_PHASE,
        configured_retained_payload_bytes_per_run: profile.active_retained_payload_bytes,
        retained_bytes_per_run: min_observed_retained_bytes_per_run,
        min_observed_retained_bytes_per_run,
        max_observed_retained_bytes_per_run,
        entered_runs,
        completed_runs,
        rejected_phase_entries,
        levels,
        live_heap_ols_slope_bytes_per_run: slope.max(0.0),
        live_heap_ols_r_squared: r_squared,
        max_live_heap_bytes_per_run,
        max_rss_bytes_per_run,
        max_cleanup_live_bytes,
        max_cleanup_fd_growth,
    })
}

fn append_active_gates(
    gates: &mut Vec<GateResult>,
    report: &ActiveRunReport,
    profile: &GateProfile,
) {
    let expected_runs = profile.active_counts.iter().sum::<usize>();
    gates.push(GateResult::exact(
        "active_run_measurement_phase_entered",
        report.entered_runs as u64,
        expected_runs as u64,
        "runs",
        "every configured run must reach the second provider wait after an accepted capability",
    ));
    gates.push(GateResult::exact(
        "active_run_measurement_phase_completed",
        report.completed_runs as u64,
        expected_runs as u64,
        "runs",
        "every measurement-phase run must be released and joined",
    ));
    gates.push(GateResult::exact(
        "active_run_rejected_phase_entries",
        report.rejected_phase_entries as u64,
        0,
        "runs",
        "malformed or pre-provider rejected runs cannot count as active measurements",
    ));
    gates.push(GateResult::maximum_f64(
        "active_run_live_heap_ols_slope",
        report.live_heap_ols_slope_bytes_per_run,
        bounded_f64(profile.active_heap_slope_bytes_per_run_max),
        "bytes_per_run",
        format!(
            "OLS linearity diagnostic r_squared={:.6}",
            report.live_heap_ols_r_squared
        ),
    ));
    gates.push(GateResult::maximum(
        "active_run_live_heap_working_set",
        report.max_live_heap_bytes_per_run,
        profile.active_working_set_bytes_per_run_max,
        "bytes_per_run",
        format!(
            "maximum live allocation per run at {:?}; accepted result={}..{} bytes/run (configured retained payload={} bytes/run)",
            report.measurement_phase,
            report.min_observed_retained_bytes_per_run,
            report.max_observed_retained_bytes_per_run,
            report.configured_retained_payload_bytes_per_run
        ),
    ));
    if let Some(value) = report.max_rss_bytes_per_run {
        gates.push(GateResult::maximum(
            "active_run_rss_working_set",
            value,
            profile.active_working_set_bytes_per_run_max,
            "bytes_per_run",
            "maximum observed RSS delta divided by active runs",
        ));
    } else {
        gates.push(unavailable_gate(
            "active_run_rss_working_set",
            "OS RSS measurement is unavailable",
        ));
    }
    gates.push(GateResult::maximum(
        "active_run_cleanup_live_bytes",
        report.max_cleanup_live_bytes,
        profile.active_cleanup_live_bytes_max,
        "bytes",
        "live allocation remaining after every spawned task was joined",
    ));
    if let Some(value) = report.max_cleanup_fd_growth {
        gates.push(GateResult::maximum_i64(
            "active_run_cleanup_fd_growth",
            value,
            profile.active_cleanup_fd_growth_max,
            "file_descriptors",
            "no provider network connection is opened by this offline fixture",
        ));
    } else {
        gates.push(unavailable_gate(
            "active_run_cleanup_fd_growth",
            "OS file-descriptor measurement is unavailable",
        ));
    }
}

#[derive(Debug)]
struct ActiveWorkload {
    orientation_assistant: Arc<AssistantMessage>,
    planner_assistant: Arc<AssistantMessage>,
    orientation_template: Arc<Value>,
    research_template: Arc<Value>,
    retained_payload_bytes: usize,
}

impl ActiveWorkload {
    fn load(root: &Path, retained_payload_bytes: usize) -> Result<Self, HarnessError> {
        let fixture = root.join("fixtures/vertical-slice/v1");
        let mut planner_assistant: AssistantMessage =
            serde_json::from_slice(&fs::read(fixture.join("provider/turn-1-assistant.json"))?)?;
        if !planner_assistant.tool_calls.is_empty() && planner_assistant.content.is_none() {
            planner_assistant.content = Some(String::new());
        }
        let mut research_template: Value = serde_json::from_slice(&fs::read(
            fixture.join("mcp/research-state-answerable.json"),
        )?)?;
        match planner_assistant.tool_calls.as_slice() {
            [tool_call]
                if tool_call.function.name.as_str()
                    == provider_tool_name("ontology.query_context") =>
            {
                // The provider fixture must remain a model-facing
                // ResearchIntent. The production engine compiles it to the
                // physical root SearchPlan before `RetainedCapability` sees
                // the invocation; rewriting it here would measure a retired
                // provider ABI instead of the real active-run path.
            }
            _ => {
                return Err(HarnessError::InvalidActiveFixture(
                    "provider fixture must contain one query_context tool call",
                ));
            }
        }
        let summary = research_template
            .pointer_mut("/evidence_units/0/summary")
            .ok_or(HarnessError::InvalidActiveFixture(
                "research-state summary is missing",
            ))?;
        *summary = Value::String("r".repeat(retained_payload_bytes));

        // A real company run now starts with this narrow, mandatory ontology
        // orientation read before the planner emits its ResearchProposal. Keep
        // the active-run fixture on that production path rather than sending
        // the planner's query_context call into the orienter state.
        let orientation_assistant = AssistantMessage {
            content: Some(String::new()),
            reasoning_content: None,
            reasoning_signature: None,
            tool_calls: vec![ToolCall {
                id: "perf-company-context".into(),
                kind: ToolCallKind::Function,
                function: FunctionCall {
                    name: ProviderFunctionName::parse(provider_tool_name(
                        "ontology.company_context",
                    ))
                    .map_err(|_| {
                        HarnessError::InvalidActiveFixture(
                            "company context provider tool name is invalid",
                        )
                    })?,
                    arguments: r#"{"ticker":"VG"}"#.into(),
                },
            }],
        };
        let orientation_template = json!({
            "format": "company-context-orientation/v1",
            "ticker": "VG",
            "status": "available",
            "advisory_only": true,
            "topics": [{
                "topic_label": "spaceflight services",
                "period": "FY2025",
                "document_type": "10-K",
                "trace_status": "available"
            }],
            "usage": "Use these labels only to narrow a later evidence query; do not cite them as company facts."
        });
        Ok(Self {
            orientation_assistant: Arc::new(orientation_assistant),
            planner_assistant: Arc::new(planner_assistant),
            orientation_template: Arc::new(orientation_template),
            research_template: Arc::new(research_template),
            retained_payload_bytes,
        })
    }
}

#[derive(Debug, Default)]
struct ActivePhaseCounters {
    first_provider_entered: AtomicUsize,
    first_provider_completed: AtomicUsize,
    planner_provider_entered: AtomicUsize,
    planner_provider_completed: AtomicUsize,
    episode_checkpointed: AtomicUsize,
    capability_entered: AtomicUsize,
    capability_completed: AtomicUsize,
    action_accepted: AtomicUsize,
    run_state_checkpointed: AtomicUsize,
    measurement_phase_entered: AtomicUsize,
    measurement_phase_completed: AtomicUsize,
    rejected_phase_entries: AtomicUsize,
    observed_result_bytes: AtomicUsize,
}

#[derive(Debug, Clone, Copy, Default)]
struct ActivePhaseSnapshot {
    first_provider_entered: usize,
    first_provider_completed: usize,
    planner_provider_entered: usize,
    planner_provider_completed: usize,
    episode_checkpointed: usize,
    capability_entered: usize,
    capability_completed: usize,
    action_accepted: usize,
    run_state_checkpointed: usize,
    measurement_phase_entered: usize,
    measurement_phase_completed: usize,
    rejected_phase_entries: usize,
    observed_result_bytes: usize,
}

impl ActivePhaseCounters {
    fn snapshot(&self) -> ActivePhaseSnapshot {
        ActivePhaseSnapshot {
            first_provider_entered: self.first_provider_entered.load(Ordering::SeqCst),
            first_provider_completed: self.first_provider_completed.load(Ordering::SeqCst),
            planner_provider_entered: self.planner_provider_entered.load(Ordering::SeqCst),
            planner_provider_completed: self.planner_provider_completed.load(Ordering::SeqCst),
            episode_checkpointed: self.episode_checkpointed.load(Ordering::SeqCst),
            capability_entered: self.capability_entered.load(Ordering::SeqCst),
            capability_completed: self.capability_completed.load(Ordering::SeqCst),
            action_accepted: self.action_accepted.load(Ordering::SeqCst),
            run_state_checkpointed: self.run_state_checkpointed.load(Ordering::SeqCst),
            measurement_phase_entered: self.measurement_phase_entered.load(Ordering::SeqCst),
            measurement_phase_completed: self.measurement_phase_completed.load(Ordering::SeqCst),
            rejected_phase_entries: self.rejected_phase_entries.load(Ordering::SeqCst),
            observed_result_bytes: self.observed_result_bytes.load(Ordering::SeqCst),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProviderRequestPhase {
    FirstProvider,
    PlannerAfterOrientation,
    Measurement,
}

fn provider_request_phase(request: &MessagesRequest) -> Result<ProviderRequestPhase, &'static str> {
    provider_message_phase(&request.messages)
}

fn provider_message_phase(
    messages: &[ProviderMessage],
) -> Result<ProviderRequestPhase, &'static str> {
    let mut compacted_contexts = Vec::new();
    let mut has_raw_tool_result = false;
    for block in messages.iter().flat_map(|message| message.content.iter()) {
        match block {
            ContentBlock::Text { text } => {
                if let Some(context) = verified_compacted_context(text)? {
                    compacted_contexts.push(context);
                }
            }
            ContentBlock::ToolResult { .. } => has_raw_tool_result = true,
            ContentBlock::ToolUse { .. } | ContentBlock::Thinking { .. } => {}
        }
    }

    let Some(context) = compacted_contexts.first() else {
        return if has_raw_tool_result {
            Err("post-tool provider request leaked a raw tool result")
        } else {
            Ok(ProviderRequestPhase::FirstProvider)
        };
    };
    if compacted_contexts.len() != 1 || has_raw_tool_result {
        return Err("post-tool provider request has an ambiguous evidence context");
    }

    let context: Value = serde_json::from_str(context)
        .map_err(|_| "verified compacted context was not canonical JSON text")?;
    let has_evidence = context
        .get("evidence_index")
        .and_then(Value::as_array)
        .is_some_and(|entries| !entries.is_empty());
    let has_facts = context
        .get("retained_facts")
        .and_then(Value::as_array)
        .is_some_and(|facts| !facts.is_empty());
    if context
        .get("schema_version")
        .and_then(Value::as_u64)
        .filter(|version| *version > 0)
        .is_none()
        || context.get("authority").and_then(Value::as_str)
            != Some("validated_state_and_committed_evidence_only")
        || !has_evidence
        || !has_facts
    {
        return Err("verified compacted context lacks accepted evidence");
    }
    // A company-orientation map is deliberately advisory only. It produces a
    // valid compacted context, but the next provider request must still be the
    // planner's required query_context call. Only a committed ResearchState
    // carries the planning projection that makes the following analyst turn
    // the active-memory measurement boundary.
    if context
        .get("research_projection")
        .is_none_or(Value::is_null)
    {
        return Ok(ProviderRequestPhase::PlannerAfterOrientation);
    }
    Ok(ProviderRequestPhase::Measurement)
}

fn verified_compacted_context(text: &str) -> Result<Option<&str>, &'static str> {
    const OPEN: &str = "<verified-compacted-context>\n";
    const CLOSE: &str = "\n</verified-compacted-context>";
    let Some((_, tail)) = text.split_once(OPEN) else {
        return Ok(None);
    };
    let Some((context, trailing)) = tail.split_once(CLOSE) else {
        return Err("verified compacted context lacks a closing delimiter");
    };
    if context.is_empty() || trailing.contains(OPEN) {
        return Err("verified compacted context is ambiguous");
    }
    Ok(Some(context))
}

#[derive(Debug)]
struct PostToolBlockingProvider {
    orientation_assistant: Arc<AssistantMessage>,
    planner_assistant: Arc<AssistantMessage>,
    counters: Arc<ActivePhaseCounters>,
    release: Arc<Semaphore>,
}

#[async_trait]
impl Provider for PostToolBlockingProvider {
    async fn complete(
        &self,
        request: &MessagesRequest,
        context: &EpisodeContext,
    ) -> Result<ProviderEpisodeV1, DependencyFailure> {
        match provider_request_phase(request) {
            Ok(ProviderRequestPhase::FirstProvider) => {
                require_active_tool_choice(request, "ontology.company_context")?;
                self.counters
                    .first_provider_entered
                    .fetch_add(1, Ordering::SeqCst);
                let request_bytes = serde_jcs::to_vec(request)
                    .map_err(|_| active_fixture_failure("serialize first provider request"))?;
                let mut episode = ProviderEpisodeV1 {
                    schema_version: 1,
                    request_hash: ContentHash::sha256(request_bytes),
                    requested_model: request.model.clone(),
                    observed_model: request.model.clone(),
                    api_version: context.api_version.clone(),
                    assistant: (*self.orientation_assistant).clone(),
                    tool_results: Vec::new(),
                    tool_schema_hash: context.tool_schema_hash.clone(),
                    agent_image_hash: context.agent_image_hash.clone(),
                    finish_reason: "tool_calls".into(),
                    usage: TokenUsage {
                        prompt_tokens: 10,
                        completion_tokens: 5,
                        total_tokens: 15,
                        prompt_cache_hit_tokens: 5,
                        prompt_cache_miss_tokens: 5,
                    },
                    replay_hash: ContentHash::sha256("pending"),
                };
                episode.replay_hash = episode
                    .calculate_replay_hash()
                    .map_err(|_| active_fixture_failure("hash first provider episode"))?;
                self.counters
                    .first_provider_completed
                    .fetch_add(1, Ordering::SeqCst);
                Ok(episode)
            }
            Ok(ProviderRequestPhase::PlannerAfterOrientation) => {
                require_active_tool_choice(request, "ontology.query_context")?;
                self.counters
                    .planner_provider_entered
                    .fetch_add(1, Ordering::SeqCst);
                let request_bytes = serde_jcs::to_vec(request)
                    .map_err(|_| active_fixture_failure("serialize planner provider request"))?;
                let mut episode = ProviderEpisodeV1 {
                    schema_version: 1,
                    request_hash: ContentHash::sha256(request_bytes),
                    requested_model: request.model.clone(),
                    observed_model: request.model.clone(),
                    api_version: context.api_version.clone(),
                    assistant: (*self.planner_assistant).clone(),
                    tool_results: Vec::new(),
                    tool_schema_hash: context.tool_schema_hash.clone(),
                    agent_image_hash: context.agent_image_hash.clone(),
                    finish_reason: "tool_calls".into(),
                    usage: TokenUsage {
                        prompt_tokens: 10,
                        completion_tokens: 5,
                        total_tokens: 15,
                        prompt_cache_hit_tokens: 5,
                        prompt_cache_miss_tokens: 5,
                    },
                    replay_hash: ContentHash::sha256("pending"),
                };
                episode.replay_hash = episode
                    .calculate_replay_hash()
                    .map_err(|_| active_fixture_failure("hash planner provider episode"))?;
                self.counters
                    .planner_provider_completed
                    .fetch_add(1, Ordering::SeqCst);
                Ok(episode)
            }
            Ok(ProviderRequestPhase::Measurement) => {
                self.counters
                    .measurement_phase_entered
                    .fetch_add(1, Ordering::SeqCst);
                let permit = self.release.acquire().await.map_err(|_| {
                    active_fixture_failure("active-run harness release semaphore closed")
                })?;
                permit.forget();
                self.counters
                    .measurement_phase_completed
                    .fetch_add(1, Ordering::SeqCst);
                Err(DependencyFailure::redacted(
                    "harness_release",
                    "intentional offline provider stop after accepted capability",
                    false,
                    DeliveryCertainty::NotDispatched,
                ))
            }
            Err(reason) => {
                self.counters
                    .rejected_phase_entries
                    .fetch_add(1, Ordering::SeqCst);
                Err(active_fixture_failure(reason))
            }
        }
    }
}

fn require_active_tool_choice(
    request: &MessagesRequest,
    capability_id: &str,
) -> Result<(), DependencyFailure> {
    let expected = provider_tool_name(capability_id);
    if matches!(
        request.tool_choice.as_ref(),
        Some(ToolChoice::Tool { name }) if name.as_str() == expected
    ) {
        Ok(())
    } else {
        let observed = match request.tool_choice.as_ref() {
            Some(ToolChoice::Any) => "any",
            Some(ToolChoice::Auto) => "auto",
            Some(ToolChoice::None) => "none",
            Some(ToolChoice::Tool { .. }) => "different_tool",
            None => "missing",
        };
        Err(DependencyFailure::redacted(
            match (capability_id, observed) {
                ("ontology.company_context", "any") => "active_orientation_tool_choice_any",
                ("ontology.company_context", "auto") => "active_orientation_tool_choice_auto",
                ("ontology.company_context", "none") => "active_orientation_tool_choice_none",
                ("ontology.company_context", "different_tool") => {
                    "active_orientation_tool_choice_different"
                }
                ("ontology.company_context", _) => "active_orientation_tool_choice_missing",
                (_, "any") => "active_planner_tool_choice_any",
                (_, "auto") => "active_planner_tool_choice_auto",
                (_, "none") => "active_planner_tool_choice_none",
                (_, "different_tool") => "active_planner_tool_choice_different",
                _ => "active_planner_tool_choice_missing",
            },
            "active provider request did not expose its required capability",
            false,
            DeliveryCertainty::NotDispatched,
        ))
    }
}

fn active_fixture_failure(reason: &'static str) -> DependencyFailure {
    DependencyFailure::redacted(
        "invalid_active_fixture",
        reason,
        false,
        DeliveryCertainty::NotDispatched,
    )
}

#[derive(Debug)]
struct RetainedCapability {
    orientation_template: Arc<Value>,
    research_template: Arc<Value>,
    counters: Arc<ActivePhaseCounters>,
}

#[async_trait]
impl CapabilityRuntime for RetainedCapability {
    async fn invoke(
        &self,
        invocation: &CapabilityInvocation,
    ) -> Result<CapabilityResult, DependencyFailure> {
        self.counters
            .capability_entered
            .fetch_add(1, Ordering::SeqCst);
        let result = match invocation.capability_id.as_str() {
            "ontology.company_context" => {
                orientation_capability_result(&self.orientation_template, invocation)?
            }
            "ontology.query_context" => {
                research_context_capability_result(&self.research_template, invocation)?
            }
            _ => {
                return Err(active_fixture_failure(
                    "active fixture invoked an unexpected capability",
                ));
            }
        };
        self.counters
            .capability_completed
            .fetch_add(1, Ordering::SeqCst);
        Ok(result)
    }
}

fn orientation_capability_result(
    template: &Value,
    invocation: &CapabilityInvocation,
) -> Result<CapabilityResult, DependencyFailure> {
    let ticker = invocation
        .arguments
        .get("ticker")
        .and_then(Value::as_str)
        .filter(|ticker| *ticker == "VG")
        .ok_or_else(|| {
            active_fixture_failure("company orientation did not retain the trusted ticker")
        })?;
    let topic_label = template
        .pointer("/topics/0/topic_label")
        .and_then(Value::as_str)
        .ok_or_else(|| active_fixture_failure("orientation template is missing its topic label"))?;
    let payload_hash = ContentHash::sha256("perf-company-orientation-result");
    Ok(CapabilityResult {
        provider_content: template.clone(),
        evidence: vec![EvidenceRecord {
            evidence_id: format!("perf-orientation-{}", invocation.action_key),
            content_hash: payload_hash.clone(),
            source: evidence_source(invocation),
            scope: perf_evidence_scope(),
            entity: Some(ticker.to_owned()),
            period: Some("FY2025".into()),
            as_of: None,
            directness: Directness::Unverified,
            grade: EvidenceGrade::Unverified,
            strong_claim_allowed: false,
            payload_ref: payload_hash,
            citation: PublicCitation {
                title: "KRW ontology company orientation: spaceflight services".into(),
                document_type: Some("10-K".into()),
                period: Some("FY2025".into()),
            },
            facts: vec![NormalizedFact {
                subject: ticker.to_owned(),
                predicate: "company_topic_orientation".into(),
                value: Value::String(topic_label.to_owned()),
                unit: None,
                period: Some("FY2025".into()),
            }],
            supports: Vec::new(),
            refutes: Vec::new(),
            qualifies: Vec::new(),
            source_object_ids: Vec::new(),
        }],
        answerability: None,
        calculations: Vec::new(),
        presentation: None,
        truncation: None,
    })
}

fn research_context_capability_result(
    template: &Value,
    invocation: &CapabilityInvocation,
) -> Result<CapabilityResult, DependencyFailure> {
    let mut provider_content = template.clone();
    // The checked-in MCP fixture predates the kernel-owned ResearchProposal
    // lowering path and consequently carries its historical clause id. A real
    // MCP response echoes the exact physical SearchPlan that it was given,
    // including every clause reference in coverage and evidence. Rebase the
    // synthetic response in the same way so this harness keeps exercising the
    // active provider -> planner -> capability path rather than measuring an
    // impossible response contract.
    let clause_id = invocation
        .arguments
        .pointer("/clauses/0/clause_id")
        .and_then(Value::as_str)
        .ok_or_else(|| active_fixture_failure("compiled SearchPlan is missing clause id"))?;
    replace_active_fixture_clause_reference(&mut provider_content, "cash_generation", clause_id);
    provider_content["plan"] = invocation.arguments.clone();
    let payload_hash = ContentHash::sha256("perf-retained-capability-result");
    Ok(CapabilityResult {
        provider_content,
        evidence: vec![EvidenceRecord {
            evidence_id: format!("perf-evidence-{}", invocation.action_key),
            content_hash: payload_hash.clone(),
            source: evidence_source(invocation),
            scope: perf_evidence_scope(),
            entity: Some("VG".into()),
            period: Some("CY2025".into()),
            as_of: Some("2026-08-02".into()),
            directness: Directness::Direct,
            grade: EvidenceGrade::Strong,
            strong_claim_allowed: true,
            payload_ref: payload_hash,
            citation: PublicCitation {
                title: "VG 2025 Form 10-K".into(),
                document_type: Some("10-K".into()),
                period: Some("CY2025".into()),
            },
            facts: vec![NormalizedFact {
                subject: "VG".into(),
                predicate: "cash_generation".into(),
                value: Value::Bool(true),
                unit: None,
                period: Some("CY2025".into()),
            }],
            supports: vec![clause_id.to_owned()],
            refutes: Vec::new(),
            qualifies: Vec::new(),
            source_object_ids: Vec::new(),
        }],
        answerability: Some(Answerability::StrongAllowed),
        calculations: Vec::new(),
        presentation: None,
        truncation: None,
    })
}

fn evidence_source(invocation: &CapabilityInvocation) -> EvidenceSource {
    EvidenceSource {
        capability_id: invocation.capability_id.clone(),
        action_key: invocation.action_key.clone(),
        server_build: invocation.binding.server_build.clone(),
        normalized_contract_hash: invocation.normalized_output_contract_hash.clone(),
        server_schema_bundle_hash: invocation.binding.server_schema_bundle_hash.clone(),
        data_release_hash: invocation.binding.data_release_hash.clone(),
    }
}

fn perf_evidence_scope() -> EvidenceScope {
    EvidenceScope {
        auth_scope: AuthScope::Tenant,
        scope_hash: ContentHash::sha256("perf-tenant-scope"),
    }
}

fn replace_active_fixture_clause_reference(value: &mut Value, from: &str, to: &str) {
    match value {
        Value::String(current) if current == from => current.clone_from(&to.to_owned()),
        Value::Array(values) => values
            .iter_mut()
            .for_each(|value| replace_active_fixture_clause_reference(value, from, to)),
        Value::Object(values) => values
            .values_mut()
            .for_each(|value| replace_active_fixture_clause_reference(value, from, to)),
        Value::String(_) | Value::Bool(_) | Value::Number(_) | Value::Null => {}
    }
}

#[derive(Debug)]
struct ActivePersistence {
    actions: Mutex<BTreeMap<(String, String), ActionReceipt>>,
    children: Mutex<BTreeMap<(String, String), ChildExecutionReceipt>>,
    counters: Arc<ActivePhaseCounters>,
}

impl ActivePersistence {
    fn reset(&self) -> Result<(), HarnessError> {
        self.actions
            .lock()
            .map_err(|_| HarnessError::ActiveStart("persistence lock poisoned".into()))?
            .clear();
        self.children
            .lock()
            .map_err(|_| HarnessError::ActiveStart("persistence lock poisoned".into()))?
            .clear();
        Ok(())
    }

    fn transition_child(
        &self,
        run_id: &str,
        child_id: &str,
        code: &str,
        transition: impl FnOnce(
            &ChildExecutionReceipt,
        ) -> Result<ChildExecutionReceipt, ChildExecutionError>,
    ) -> Result<ChildExecutionReceipt, DependencyFailure> {
        let key = (run_id.to_owned(), child_id.to_owned());
        let mut children = self
            .children
            .lock()
            .map_err(|_| active_fixture_failure("persistence child lock poisoned"))?;
        let current = children
            .get(&key)
            .ok_or_else(|| active_fixture_failure("bounded child is missing"))?;
        let next = transition(current).map_err(|error| {
            DependencyFailure::redacted(
                code,
                error.to_string(),
                false,
                DeliveryCertainty::NotDispatched,
            )
        })?;
        children.insert(key, next.clone());
        Ok(next)
    }
}

#[async_trait]
impl Persistence for ActivePersistence {
    async fn inspect_run(&self, run: &RunIdentity) -> Result<RunControl, DependencyFailure> {
        Ok(RunControl::Active {
            fencing_token: run.fencing_token,
            cancel_generation: run.expected_cancel_generation,
        })
    }

    async fn load_recovery(
        &self,
        _run: &RunIdentity,
    ) -> Result<RecoverySnapshot, DependencyFailure> {
        Ok(RecoverySnapshot::Fresh)
    }

    async fn checkpoint_episode(&self, _episode: &DurableEpisode) -> Result<(), DependencyFailure> {
        self.counters
            .episode_checkpointed
            .fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn checkpoint_run_state(
        &self,
        _state: &DurableRunState,
    ) -> Result<(), DependencyFailure> {
        self.counters
            .run_state_checkpointed
            .fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn reserve_child(
        &self,
        mutation: &ReserveChildMutation,
    ) -> Result<ChildExecutionReceipt, DependencyFailure> {
        let proposed = krw_agent_bounded_child::reserve(mutation).map_err(|error| {
            DependencyFailure::redacted(
                "perf_child_reserve",
                error.to_string(),
                false,
                DeliveryCertainty::NotDispatched,
            )
        })?;
        let key = (mutation.run_id.clone(), mutation.child_id.clone());
        let mut children = self
            .children
            .lock()
            .map_err(|_| active_fixture_failure("persistence child lock poisoned"))?;
        match children.get(&key) {
            Some(existing) if existing == &proposed => Ok(existing.clone()),
            Some(_) => Err(active_fixture_failure("bounded child reserve conflict")),
            None => {
                children.insert(key, proposed.clone());
                Ok(proposed)
            }
        }
    }

    async fn invoke_child(
        &self,
        mutation: &InvokeChildMutation,
    ) -> Result<ChildExecutionReceipt, DependencyFailure> {
        self.transition_child(
            &mutation.run_id,
            &mutation.child_id,
            "perf_child_invoke",
            |current| krw_agent_bounded_child::invoke(current, mutation),
        )
    }

    async fn complete_child(
        &self,
        mutation: &CompleteChildMutation,
    ) -> Result<ChildExecutionReceipt, DependencyFailure> {
        self.transition_child(
            &mutation.run_id,
            &mutation.child_id,
            "perf_child_complete",
            |current| krw_agent_bounded_child::complete(current, mutation),
        )
    }

    async fn cancel_child(
        &self,
        mutation: &CancelChildMutation,
    ) -> Result<ChildExecutionReceipt, DependencyFailure> {
        self.transition_child(
            &mutation.run_id,
            &mutation.child_id,
            "perf_child_cancel",
            |current| krw_agent_bounded_child::cancel(current, mutation),
        )
    }

    async fn begin_action(
        &self,
        intent: &ActionIntent,
    ) -> Result<ActionReceipt, DependencyFailure> {
        let mut actions = self
            .actions
            .lock()
            .map_err(|_| active_fixture_failure("persistence action lock poisoned"))?;
        let receipt = actions
            .entry((
                intent.mutation.run_id.clone(),
                intent.mutation.action_key.clone(),
            ))
            .or_insert_with(|| ActionReceipt {
                action_key: intent.mutation.action_key.clone(),
                mutation_id: intent.mutation.mutation_id.clone(),
                request_hash: intent.mutation.request_hash.clone(),
                result_hash: None,
                stage: ActionStage::Begun,
                retryable_read: intent.mutation.retryable_read,
            });
        Ok(receipt.clone())
    }

    async fn observe_action(
        &self,
        observation: &DurableActionObservation,
    ) -> Result<ActionReceipt, DependencyFailure> {
        let mut actions = self
            .actions
            .lock()
            .map_err(|_| active_fixture_failure("persistence action lock poisoned"))?;
        let receipt = actions
            .get_mut(&(
                observation.mutation.run_id.clone(),
                observation.mutation.action_key.clone(),
            ))
            .ok_or_else(|| active_fixture_failure("observation lacks begun action"))?;
        receipt.result_hash = Some(observation.mutation.result_hash.clone());
        receipt.stage = ActionStage::Observed;
        self.counters
            .observed_result_bytes
            .fetch_add(observation.result_bytes.len(), Ordering::SeqCst);
        Ok(receipt.clone())
    }

    async fn finalize_action(
        &self,
        mutation: FinalizeActionMutation,
    ) -> Result<ActionFinalizationReceipt, DependencyFailure> {
        let mut actions = self
            .actions
            .lock()
            .map_err(|_| active_fixture_failure("persistence action lock poisoned"))?;
        let receipt = actions
            .get_mut(&(mutation.run_id.clone(), mutation.action_key.clone()))
            .ok_or_else(|| active_fixture_failure("finalize lacks observed action"))?;
        if receipt.result_hash.as_ref() != Some(&mutation.result_hash) {
            return Err(active_fixture_failure("finalize result hash mismatch"));
        }
        receipt.stage = mutation.disposition.stage();
        if mutation.disposition == ActionDisposition::Accepted {
            self.counters.action_accepted.fetch_add(1, Ordering::SeqCst);
        }
        Ok(ActionFinalizationReceipt {
            action: receipt.clone(),
            disposition: mutation.disposition,
            validation_receipt_hash: mutation.validation_receipt_hash,
            policy_receipt_hash: mutation.policy_receipt_hash,
        })
    }

    async fn mark_action_ambiguous(
        &self,
        _mutation: MarkActionAmbiguous,
    ) -> Result<(), DependencyFailure> {
        Err(unreachable_dependency("mark_action_ambiguous"))
    }

    async fn load_action_result(
        &self,
        _run: &RunIdentity,
        _action_key: &str,
        _expected_hash: &ContentHash,
    ) -> Result<Option<Vec<u8>>, DependencyFailure> {
        Err(unreachable_dependency("load_action_result"))
    }

    async fn commit_final(
        &self,
        _final_value: &DurableFinal,
    ) -> Result<FinalStatus, DependencyFailure> {
        Err(unreachable_dependency("commit_final"))
    }
}

fn unreachable_dependency(component: &str) -> DependencyFailure {
    DependencyFailure::redacted(
        "harness_unreachable",
        component,
        false,
        DeliveryCertainty::NotDispatched,
    )
}

async fn measure_active_level(
    runs: usize,
    workload: Arc<ActiveWorkload>,
    image: Arc<LoadedImage>,
    deployment: Arc<DeploymentBinding>,
) -> Result<ActiveLevel, HarnessError> {
    if runs == 0 || (0..runs).any(|index| !active_request_is_bounded(&active_request(index))) {
        return Err(HarnessError::InvalidActiveFixture(
            "generated RunRequest exceeds the harness request bound",
        ));
    }
    let counters = Arc::new(ActivePhaseCounters::default());
    let release = Arc::new(Semaphore::new(0));
    let provider = Arc::new(PostToolBlockingProvider {
        orientation_assistant: Arc::clone(&workload.orientation_assistant),
        planner_assistant: Arc::clone(&workload.planner_assistant),
        counters: Arc::clone(&counters),
        release: Arc::clone(&release),
    });
    let persistence = Arc::new(ActivePersistence {
        actions: Mutex::new(BTreeMap::new()),
        children: Mutex::new(BTreeMap::new()),
        counters: Arc::clone(&counters),
    });
    let engine = Arc::new(RunEngine::new(
        provider,
        Arc::new(RetainedCapability {
            orientation_template: Arc::clone(&workload.orientation_template),
            research_template: Arc::clone(&workload.research_template),
            counters: Arc::clone(&counters),
        }),
        Arc::clone(&persistence),
        EngineConfig::default(),
    ));
    let baseline = process_metrics();
    let mut handles = Vec::with_capacity(runs);

    for index in 0..runs {
        let engine = Arc::clone(&engine);
        let image = Arc::clone(&image);
        let deployment = Arc::clone(&deployment);
        handles.push(tokio::spawn(async move {
            let request = active_request(index);
            let snapshot = active_snapshot(&request, &image.manifest, &deployment);
            let input = RunInput {
                image: &image,
                deployment: &deployment,
                resolved_deployment_binding_hash: &snapshot.deployment_binding_hash,
                request: &request,
                snapshot: &snapshot,
                market_snapshot_context: None,
                runtime_timings: None,
                execution_plan: None,
                hard_deadline: Instant::now() + Duration::from_secs(30),
            };
            engine.run(input).await
        }));
    }

    let all_entered = tokio::time::timeout(Duration::from_secs(10), async {
        while counters.measurement_phase_entered.load(Ordering::SeqCst) < runs {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await;
    if all_entered.is_err() {
        release.add_permits(runs);
        let mut outcomes = Vec::with_capacity(runs);
        for handle in handles {
            outcomes.push(match handle.await {
                Ok(result) => format!("{result:?}"),
                Err(error) => format!("join:{error}"),
            });
        }
        return Err(HarnessError::ActiveStart(outcomes.join(", ")));
    }
    let at_measurement = counters.snapshot();
    let phase_validation = validate_active_phase(at_measurement, runs, false).and_then(|()| {
        if at_measurement.observed_result_bytes / runs < workload.retained_payload_bytes {
            Err(HarnessError::ActiveStart(
                "accepted result is smaller than the configured retained payload".into(),
            ))
        } else {
            Ok(())
        }
    });
    if let Err(error) = phase_validation {
        release.add_permits(runs);
        for handle in handles {
            let _ = handle.await;
        }
        return Err(error);
    }
    tokio::task::yield_now().await;
    let active = process_metrics();
    release.add_permits(runs);
    let mut joined_tasks = 0;
    for handle in handles {
        let outcome = handle
            .await
            .map_err(|error| HarnessError::Join(error.to_string()))?;
        if outcome.is_ok() {
            return Err(HarnessError::UnexpectedActiveSuccess);
        }
        joined_tasks += 1;
    }
    let completed_phase = counters.snapshot();
    validate_active_phase(completed_phase, runs, true)?;
    persistence.reset()?;
    tokio::task::yield_now().await;
    tokio::time::sleep(Duration::from_millis(10)).await;
    let cleaned = process_metrics();

    Ok(ActiveLevel {
        runs,
        measurement_phase: ACTIVE_MEASUREMENT_PHASE,
        configured_retained_payload_bytes_per_run: workload.retained_payload_bytes,
        observed_retained_bytes_per_run: at_measurement.observed_result_bytes / runs,
        first_provider_entered: at_measurement.first_provider_entered,
        first_provider_completed: at_measurement.first_provider_completed,
        planner_provider_entered: at_measurement.planner_provider_entered,
        planner_provider_completed: at_measurement.planner_provider_completed,
        episode_checkpointed: at_measurement.episode_checkpointed,
        capability_entered: at_measurement.capability_entered,
        capability_completed: at_measurement.capability_completed,
        action_accepted: at_measurement.action_accepted,
        run_state_checkpointed: at_measurement.run_state_checkpointed,
        measurement_phase_entered: at_measurement.measurement_phase_entered,
        measurement_phase_completed: completed_phase.measurement_phase_completed,
        rejected_phase_entries: completed_phase.rejected_phase_entries,
        baseline,
        active,
        cleaned,
        live_delta_bytes: signed_delta(active.live_allocated_bytes, baseline.live_allocated_bytes),
        rss_delta_bytes: optional_signed_delta(active.rss_bytes, baseline.rss_bytes),
        cleanup_live_delta_bytes: signed_delta(
            cleaned.live_allocated_bytes,
            baseline.live_allocated_bytes,
        ),
        cleanup_fd_delta: optional_signed_delta(cleaned.open_fds, baseline.open_fds),
        spawned_tasks: runs,
        joined_tasks,
    })
}

fn validate_active_phase(
    snapshot: ActivePhaseSnapshot,
    runs: usize,
    released: bool,
) -> Result<(), HarnessError> {
    let expected_completed = if released { runs } else { 0 };
    if snapshot.first_provider_entered != runs
        || snapshot.first_provider_completed != runs
        || snapshot.planner_provider_entered != runs
        || snapshot.planner_provider_completed != runs
        || snapshot.episode_checkpointed != 2 * runs
        || snapshot.capability_entered != 2 * runs
        || snapshot.capability_completed != 2 * runs
        || snapshot.action_accepted != 2 * runs
        || snapshot.run_state_checkpointed != 2 * runs
        || snapshot.measurement_phase_entered != runs
        || snapshot.measurement_phase_completed != expected_completed
        || snapshot.rejected_phase_entries != 0
        || snapshot.observed_result_bytes == 0
        || !snapshot.observed_result_bytes.is_multiple_of(runs)
    {
        return Err(HarnessError::ActiveStart(format!(
            "phase={ACTIVE_MEASUREMENT_PHASE:?} released={released} counters={snapshot:?}"
        )));
    }
    Ok(())
}

fn active_deployment() -> DeploymentBinding {
    let query_context = CapabilityBinding {
        binding_key: "krw_ontology_query_context".into(),
        mcp_tool_name: "krw_ontology_query_context".into(),
        endpoint_ref: "offline".into(),
        credential_ref: None,
        auth_scope: AuthScope::Public,
        tool_session_reuse: McpToolSessionReuse::RunScoped,
        server_schema_bundle_hash: ContentHash::sha256("perf-schema"),
        server_build: "perf-offline".into(),
        data_release_hash: ContentHash::sha256("perf-release"),
        max_connections: 1,
        request_timeout_ms: 1_000,
    };
    let mut company_context = query_context.clone();
    company_context.binding_key = "krw_ontology_company_context".into();
    company_context.mcp_tool_name = "krw_ontology_company_context".into();
    DeploymentBinding {
        schema_version: 4,
        deployment_id: "perf-offline".into(),
        capabilities: vec![query_context, company_context],
    }
}

fn active_request(index: usize) -> RunRequest {
    let mut capability_call_limits = BTreeMap::new();
    capability_call_limits.insert("ontology.company_context".into(), 1);
    capability_call_limits.insert("ontology.query_context".into(), 1);
    RunRequest {
        run_id: format!("perf-run-{index}"),
        session_id: format!("perf-session-{index}"),
        tenant_id: format!("perf-tenant-{}", index % 4),
        principal_id: format!("perf-principal-{index}"),
        run_kind: "company_research".into(),
        locale: "ko-KR".into(),
        // Keep the synthetic active-run request aligned with the immutable
        // ResearchProposal fixture. The model first receives the required
        // company orientation, then emits this fixture's plan on the same
        // question-bound production path.
        question: "VG의 현금창출력이 공시 근거로 확인되는지 설명해줘".into(),
        requested_model: "glm-5.3".into(),
        model_profile: "glm_high".into(),
        budget: BudgetLimits {
            // Production company research has four minimum provider decisions:
            // orientation, planning, evidence assessment, and final composition.
            // The former three-turn fixture predated the mandatory orientation
            // read and therefore reserved the composer too early.
            max_provider_turns: 4,
            max_capability_calls: 2,
            max_replans: 1,
            max_repairs: 1,
            max_input_tokens: 4_096,
            // Company research reserves two complete 16,384-token composition
            // attempts and requires at least one viable 2,048-token research
            // decision before that workflow-specific reserve is consumed. A synthetic
            // benchmark request must satisfy the same immutable execution
            // contract as a real run; otherwise it measures rejected-input
            // handling rather than the post-tool active phase.
            max_output_tokens: 56_000,
            max_evidence_bytes: 1024 * 1024,
            deadline_ms: 30_000,
            capability_call_limits,
        },
        session_memory: None,
        context: RunContextV1::CompanyTickerSet {
            tickers: vec!["VG".into()],
        },
    }
}

fn active_request_is_bounded(request: &RunRequest) -> bool {
    !request.question.is_empty()
        && request.question.len() <= MAX_RUN_QUESTION_BYTES
        && request.context.validate().is_ok()
}

fn active_snapshot(
    request: &RunRequest,
    image: &AgentImageManifest,
    deployment: &DeploymentBinding,
) -> ResolvedExecutionSnapshot {
    ResolvedExecutionSnapshot {
        protocol_version: PROTOCOL_VERSION,
        run_id: request.run_id.clone(),
        fencing_token: 7,
        cancel_generation: 0,
        agent_image_hash: image.content_hash.clone(),
        deployment_binding_hash: ContentHash::sha256(
            serde_jcs::to_vec(deployment).expect("deployment"),
        ),
        model_registry_hash: ContentHash::sha256("perf-model-registry"),
        budget_registry_hash: ContentHash::sha256("perf-budget-registry"),
        model_profile: request.model_profile.clone(),
        requested_model: request.requested_model.clone(),
        resolved_model: request.requested_model.clone(),
        provider_api_version: "anthropic-messages-v1".into(),
        provider_max_context_tokens: 204_800,
        provider_wire_capabilities: ProviderWireCapabilities::glm_5_3(),
        thinking: ThinkingMode::Enabled,
        reasoning_effort: Some(ReasoningEffort::High),
        capability_release_hashes: BTreeMap::from([
            (
                "ontology.query_context".into(),
                deployment.capabilities[0].data_release_hash.clone(),
            ),
            (
                "ontology.company_context".into(),
                deployment.capabilities[1].data_release_hash.clone(),
            ),
        ]),
        budget: request.budget.clone(),
    }
}

async fn run_mcp_pool(profile: &GateProfile) -> Result<McpPoolReport, HarnessError> {
    let pool = Arc::new(McpClientPool::new(
        profile.mcp_pool_entries_max,
        Duration::from_mins(1),
    )?);
    let binding = mcp_binding();
    let endpoint = "http://127.0.0.1:1/mcp";
    let readiness_endpoint = "http://127.0.0.1:1/readyz";
    let origin = "https://offline.invalid";
    let key = PoolKey::from_binding(
        "offline-mcp",
        &binding,
        endpoint,
        readiness_endpoint,
        origin,
        "2025-06-18",
        &krw_agent_tool_mcp::PoolScope {
            tenant_id: "tenant".into(),
            principal_id: "same-principal".into(),
            run_id: "run".into(),
        },
        "none",
        "rustls-ring",
        None,
    )?;
    let mut tasks = Vec::with_capacity(profile.mcp_singleflight_parallelism);
    for _ in 0..profile.mcp_singleflight_parallelism {
        let pool = Arc::clone(&pool);
        let key = key.clone();
        tasks.push(tokio::spawn(async move {
            pool.get_or_connect(key, invalid_mcp_config()).await
        }));
    }
    for task in tasks {
        let result = task
            .await
            .map_err(|error| HarnessError::Join(error.to_string()))?;
        if result.is_ok() {
            return Err(HarnessError::Mcp(McpError::InvalidEndpoint));
        }
    }
    let same_key = pool.stats().await;

    let high_cardinality_attempts = profile.mcp_pool_entries_max.saturating_mul(4);
    for index in 0..high_cardinality_attempts {
        let endpoint = format!("http://127.0.0.1:1/mcp/{index}");
        let readiness_endpoint = format!("http://127.0.0.1:1/readyz/{index}");
        let key = PoolKey::from_binding(
            "offline-mcp",
            &binding,
            &endpoint,
            &readiness_endpoint,
            origin,
            "2025-06-18",
            &krw_agent_tool_mcp::PoolScope {
                tenant_id: "tenant".into(),
                principal_id: format!("principal-{index}"),
                run_id: format!("run-{index}"),
            },
            "none",
            "rustls-ring",
            None,
        )?;
        let _ = pool
            .get_or_connect(key, invalid_mcp_config_for(endpoint, readiness_endpoint))
            .await;
    }
    let high_cardinality = pool.stats().await;
    Ok(McpPoolReport {
        configured_cap: profile.mcp_pool_entries_max,
        same_key_parallel_requests: profile.mcp_singleflight_parallelism,
        same_key_entries: same_key.entries,
        same_key_initializing: same_key.initializing,
        same_key_failed: same_key.failed,
        high_cardinality_attempts,
        high_cardinality_entries: high_cardinality.entries,
    })
}

fn append_mcp_gates(gates: &mut Vec<GateResult>, report: &McpPoolReport, profile: &GateProfile) {
    gates.push(GateResult::exact(
        "mcp_same_key_pool_entries",
        report.same_key_entries as u64,
        1,
        "entries",
        "all concurrent callers must share one initialization slot",
    ));
    gates.push(GateResult::exact(
        "mcp_same_key_failed_slots",
        report.same_key_failed as u64,
        1,
        "entries",
        "the failed initialization is negative-cached once per key",
    ));
    gates.push(GateResult::exact(
        "mcp_same_key_initializing_slots",
        report.same_key_initializing as u64,
        0,
        "entries",
        "no initialization future remains after all callers return",
    ));
    gates.push(GateResult::maximum(
        "mcp_high_cardinality_pool_cap",
        report.high_cardinality_entries as u64,
        profile.mcp_pool_entries_max as u64,
        "entries",
        "principal-partitioned failed entries are still bounded by the hard cap",
    ));
}

fn mcp_binding() -> CapabilityBinding {
    CapabilityBinding {
        binding_key: "krw_ontology_query_context".into(),
        mcp_tool_name: "krw_ontology_query_context".into(),
        endpoint_ref: "offline-invalid".into(),
        credential_ref: None,
        auth_scope: AuthScope::Principal,
        tool_session_reuse: McpToolSessionReuse::RunScoped,
        server_schema_bundle_hash: ContentHash::sha256("perf-mcp-schema"),
        server_build: "perf-mcp".into(),
        data_release_hash: ContentHash::sha256("perf-mcp-release"),
        max_connections: 1,
        request_timeout_ms: 10,
    }
}

fn invalid_mcp_config() -> McpHttpConfig {
    invalid_mcp_config_for(
        "http://127.0.0.1:1/mcp".into(),
        "http://127.0.0.1:1/readyz".into(),
    )
}

fn invalid_mcp_config_for(endpoint: String, readiness_endpoint: String) -> McpHttpConfig {
    McpHttpConfig {
        endpoint,
        readiness_endpoint,
        origin: "https://offline.invalid".into(),
        bearer_token: None,
        protocol_version: "2025-06-18".into(),
        client_name: "krw-agent-perf-harness".into(),
        client_version: env!("CARGO_PKG_VERSION").into(),
        tls_profile: "system-roots-v1".into(),
        tls_ca_pem: None,
        max_concurrency: 1,
        request_timeout: Duration::from_millis(10),
        max_response_bytes: 1024,
    }
}

async fn run_soak(root: &Path, profile: &GateProfile) -> Result<SoakReport, HarnessError> {
    let agent_dir = root.join("agents/krw-ontology");
    let fixture_dir = root.join("fixtures/vertical-slice/v1");
    let image = Arc::new(compile_agent_dir(&agent_dir)?.into_loaded()?);
    let deployment = Arc::new(active_deployment());
    let active_workload = Arc::new(ActiveWorkload::load(
        root,
        profile.active_retained_payload_bytes,
    )?);
    for _ in 0..profile.soak_warmup_iterations {
        run_vertical_slice(&agent_dir, &fixture_dir)
            .map_err(|error| HarnessError::VerticalSlice(error.to_string()))?;
    }
    let _ = measure_active_level(
        profile.soak_active_wave_runs,
        Arc::clone(&active_workload),
        Arc::clone(&image),
        Arc::clone(&deployment),
    )
    .await?;
    let baseline = process_metrics();
    let started = Instant::now();
    let minimum_duration = Duration::from_secs(profile.soak_min_duration_seconds);
    let maximum_duration = Duration::from_secs(profile.soak_max_duration_seconds);
    let pause = Duration::from_millis(profile.soak_pause_millis);
    let mut completed = 0_usize;
    let mut active_churn_waves = 0_usize;
    let mut active_churn_tasks_spawned = 0_usize;
    let mut active_churn_tasks_joined = 0_usize;
    let mut active_min_observed_retained_bytes_per_run = usize::MAX;
    let mut active_max_observed_retained_bytes_per_run = 0_usize;
    let mut active_measurement_phase_entered = 0_usize;
    let mut active_measurement_phase_completed = 0_usize;
    let mut active_rejected_phase_entries = 0_usize;
    let mut active_churn_max_cleanup_live_bytes = 0_u64;
    let mut active_churn_max_cleanup_fd_growth = Some(i64::MIN);
    let mut samples = Vec::new();
    let iteration_sample_interval = profile.soak_min_iterations.div_ceil(10).max(1);
    let time_sample_interval = if minimum_duration.is_zero() {
        None
    } else {
        Some((minimum_duration / 20).max(Duration::from_secs(1)))
    };
    let mut next_time_sample = time_sample_interval.map(|interval| Instant::now() + interval);

    loop {
        run_vertical_slice(&agent_dir, &fixture_dir)
            .map_err(|error| HarnessError::VerticalSlice(error.to_string()))?;
        completed += 1;
        if completed.is_multiple_of(profile.soak_active_wave_every_iterations) {
            let level = measure_active_level(
                profile.soak_active_wave_runs,
                Arc::clone(&active_workload),
                Arc::clone(&image),
                Arc::clone(&deployment),
            )
            .await?;
            active_churn_waves += 1;
            active_churn_tasks_spawned += level.spawned_tasks;
            active_churn_tasks_joined += level.joined_tasks;
            active_min_observed_retained_bytes_per_run = active_min_observed_retained_bytes_per_run
                .min(level.observed_retained_bytes_per_run);
            active_max_observed_retained_bytes_per_run = active_max_observed_retained_bytes_per_run
                .max(level.observed_retained_bytes_per_run);
            active_measurement_phase_entered += level.measurement_phase_entered;
            active_measurement_phase_completed += level.measurement_phase_completed;
            active_rejected_phase_entries += level.rejected_phase_entries;
            active_churn_max_cleanup_live_bytes = active_churn_max_cleanup_live_bytes
                .max(positive_i64(level.cleanup_live_delta_bytes));
            active_churn_max_cleanup_fd_growth =
                match (active_churn_max_cleanup_fd_growth, level.cleanup_fd_delta) {
                    (Some(current), Some(observed)) => Some(current.max(observed)),
                    _ => None,
                };
        }
        if !pause.is_zero() {
            tokio::time::sleep(pause).await;
        }
        let elapsed = started.elapsed();
        let iteration_due = completed.is_multiple_of(iteration_sample_interval);
        let time_due = next_time_sample.is_some_and(|deadline| Instant::now() >= deadline);
        if iteration_due || time_due {
            samples.push(SoakSample {
                iteration: completed,
                elapsed_millis: elapsed.as_millis(),
                process: process_metrics(),
            });
            if time_due {
                next_time_sample = time_sample_interval.map(|interval| Instant::now() + interval);
            }
        }
        if completed >= profile.soak_min_iterations && elapsed >= minimum_duration {
            break;
        }
        if elapsed >= maximum_duration {
            return Err(HarnessError::InvalidThresholds);
        }
    }

    tokio::task::yield_now().await;
    let final_metrics = process_metrics();
    let live_growth_bytes = signed_delta(
        final_metrics.live_allocated_bytes,
        baseline.live_allocated_bytes,
    );
    let rss_growth_bytes = optional_signed_delta(final_metrics.rss_bytes, baseline.rss_bytes);
    let rss_growth_percent = match (final_metrics.rss_bytes, baseline.rss_bytes) {
        (Some(final_rss), Some(baseline_rss)) if baseline_rss > 0 => {
            let final_kib = bounded_f64(final_rss / 1024);
            let baseline_kib = bounded_f64(baseline_rss / 1024);
            Some((final_kib - baseline_kib) * 100.0 / baseline_kib)
        }
        _ => None,
    };
    let fd_growth = optional_signed_delta(final_metrics.open_fds, baseline.open_fds);
    let thread_growth = optional_signed_delta(final_metrics.os_threads, baseline.os_threads);
    Ok(SoakReport {
        warmup_iterations: profile.soak_warmup_iterations,
        completed_iterations: completed,
        elapsed_millis: started.elapsed().as_millis(),
        baseline,
        final_metrics,
        live_growth_bytes,
        rss_growth_bytes,
        rss_growth_percent,
        fd_growth,
        thread_growth,
        active_measurement_phase: ACTIVE_MEASUREMENT_PHASE,
        active_configured_retained_payload_bytes_per_run: active_workload.retained_payload_bytes,
        active_min_observed_retained_bytes_per_run: if active_churn_waves == 0 {
            0
        } else {
            active_min_observed_retained_bytes_per_run
        },
        active_max_observed_retained_bytes_per_run,
        active_measurement_phase_entered,
        active_measurement_phase_completed,
        active_rejected_phase_entries,
        active_churn_waves,
        active_churn_tasks_spawned,
        active_churn_tasks_joined,
        active_churn_max_cleanup_live_bytes,
        active_churn_max_cleanup_fd_growth,
        samples,
    })
}

fn append_soak_gates(gates: &mut Vec<GateResult>, report: &SoakReport, profile: &GateProfile) {
    gates.push(GateResult::maximum(
        "soak_live_growth_bytes",
        positive_i64(report.live_growth_bytes),
        profile.soak_live_growth_bytes_max,
        "bytes",
        "warm baseline to final live allocation growth",
    ));
    if let Some(value) = report.rss_growth_bytes {
        gates.push(GateResult::maximum(
            "soak_rss_growth_bytes",
            positive_i64(value),
            profile.soak_rss_growth_bytes_max,
            "bytes",
            "warm baseline to final resident-set growth",
        ));
    } else {
        gates.push(unavailable_gate(
            "soak_rss_growth_bytes",
            "OS RSS measurement is unavailable",
        ));
    }
    if let Some(value) = report.rss_growth_percent {
        gates.push(GateResult::maximum_f64(
            "soak_rss_growth_percent",
            value.max(0.0),
            profile.soak_rss_growth_percent_max,
            "percent",
            "warm baseline to final RSS growth percentage",
        ));
    } else {
        gates.push(unavailable_gate(
            "soak_rss_growth_percent",
            "OS RSS percentage is unavailable",
        ));
    }
    append_optional_growth_gate(
        gates,
        "soak_fd_growth",
        report.fd_growth,
        profile.soak_fd_growth_max,
        "file_descriptors",
    );
    append_optional_growth_gate(
        gates,
        "soak_thread_growth",
        report.thread_growth,
        profile.soak_thread_growth_max,
        "threads",
    );
    gates.push(GateResult::exact(
        "soak_active_tasks_joined",
        report.active_churn_tasks_joined as u64,
        report.active_churn_tasks_spawned as u64,
        "tasks",
        format!(
            "all RunEngine tasks from {} active churn waves must join",
            report.active_churn_waves
        ),
    ));
    gates.push(GateResult::maximum(
        "soak_active_cleanup_live_bytes",
        report.active_churn_max_cleanup_live_bytes,
        profile.active_cleanup_live_bytes_max,
        "bytes",
        "maximum post-join live allocation delta across active churn waves",
    ));
    append_optional_growth_gate(
        gates,
        "soak_active_cleanup_fd_growth",
        report.active_churn_max_cleanup_fd_growth,
        profile.active_cleanup_fd_growth_max,
        "file_descriptors",
    );
}

fn process_metrics() -> ProcessMetrics {
    ProcessMetrics {
        live_allocated_bytes: live_allocated_bytes(),
        rss_bytes: process_rss_bytes(),
        open_fds: process_open_fds(),
        os_threads: process_thread_count(),
    }
}

fn atomic_write_report(output: &Path, bytes: &[u8]) -> Result<(), std::io::Error> {
    let parent = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let file_name = output
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("performance-report.json");
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let staging = parent.join(format!(".{file_name}.{}.{}.tmp", std::process::id(), nonce));
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staging)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&staging, output)?;
        fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&staging);
    }
    result
}

fn live_allocated_bytes() -> u64 {
    let stats = INSTRUMENTED_SYSTEM.stats();
    // `stats_alloc` already books realloc growth/shrinkage into allocated or
    // deallocated bytes. `bytes_reallocated` is a diagnostic view of the same
    // delta and adding it again would double-count every growing buffer.
    let live = stats.bytes_allocated as i128 - stats.bytes_deallocated as i128;
    u64::try_from(live.max(0)).unwrap_or(u64::MAX)
}

fn process_rss_bytes() -> Option<u64> {
    let output = Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let kib = String::from_utf8(output.stdout)
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()?;
    kib.checked_mul(1024)
}

#[cfg(target_os = "linux")]
fn process_open_fds() -> Option<u64> {
    Some(u64::try_from(fs::read_dir("/proc/self/fd").ok()?.count()).ok()?)
}

#[cfg(target_os = "macos")]
fn process_open_fds() -> Option<u64> {
    let output = Command::new("lsof")
        .args(["-nP", "-p", &std::process::id().to_string()])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let lines = String::from_utf8(output.stdout).ok()?.lines().count();
    u64::try_from(lines.saturating_sub(1)).ok()
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn process_open_fds() -> Option<u64> {
    None
}

#[cfg(target_os = "linux")]
fn process_thread_count() -> Option<u64> {
    Some(u64::try_from(fs::read_dir("/proc/self/task").ok()?.count()).ok()?)
}

#[cfg(target_os = "macos")]
fn process_thread_count() -> Option<u64> {
    let output = Command::new("ps")
        .args(["-M", "-p", &std::process::id().to_string(), "-o", "pid="])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    u64::try_from(String::from_utf8(output.stdout).ok()?.lines().count()).ok()
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn process_thread_count() -> Option<u64> {
    None
}

fn warm_allocator() {
    let values = (0..1024).map(|value| value.to_string()).collect::<Vec<_>>();
    drop(values);
}

fn positive_delta(current: u64, baseline: u64) -> u64 {
    current.saturating_sub(baseline)
}

fn positive_i64(value: i64) -> u64 {
    u64::try_from(value.max(0)).unwrap_or(u64::MAX)
}

fn signed_delta(current: u64, baseline: u64) -> i64 {
    let delta = i128::from(current) - i128::from(baseline);
    i64::try_from(delta).unwrap_or_else(|_| {
        if delta.is_negative() {
            i64::MIN
        } else {
            i64::MAX
        }
    })
}

fn optional_signed_delta(current: Option<u64>, baseline: Option<u64>) -> Option<i64> {
    Some(signed_delta(current?, baseline?))
}

fn nanos_u64(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

fn bounded_f64(value: u64) -> f64 {
    f64::from(u32::try_from(value).unwrap_or(u32::MAX))
}

fn ordinary_least_squares(points: &[(f64, f64)]) -> (f64, f64) {
    if points.len() < 2 {
        return (points.first().map_or(0.0, |(_, y)| *y), 1.0);
    }
    let count = f64::from(u32::try_from(points.len()).unwrap_or(u32::MAX));
    let mean_x = points.iter().map(|(x, _)| x).sum::<f64>() / count;
    let mean_y = points.iter().map(|(_, y)| y).sum::<f64>() / count;
    let covariance = points
        .iter()
        .map(|(x, y)| (x - mean_x) * (y - mean_y))
        .sum::<f64>();
    let variance = points
        .iter()
        .map(|(x, _)| (x - mean_x).powi(2))
        .sum::<f64>();
    if variance == 0.0 {
        return (0.0, 1.0);
    }
    let slope = covariance / variance;
    let intercept = mean_y - slope * mean_x;
    let total = points
        .iter()
        .map(|(_, y)| (y - mean_y).powi(2))
        .sum::<f64>();
    let residual = points
        .iter()
        .map(|(x, y)| (y - (intercept + slope * x)).powi(2))
        .sum::<f64>();
    let r_squared = if total == 0.0 {
        1.0
    } else {
        (1.0 - residual / total).clamp(0.0, 1.0)
    };
    (slope, r_squared)
}

fn append_optional_rss_gate(
    gates: &mut Vec<GateResult>,
    id: &str,
    current: Option<u64>,
    baseline: Option<u64>,
    limit: u64,
) {
    match (current, baseline) {
        (Some(current), Some(baseline)) => gates.push(GateResult::maximum(
            id,
            positive_delta(current, baseline),
            limit,
            "bytes",
            "OS resident-set delta",
        )),
        _ => gates.push(unavailable_gate(id, "OS RSS measurement is unavailable")),
    }
}

fn append_optional_growth_gate(
    gates: &mut Vec<GateResult>,
    id: &str,
    observed: Option<i64>,
    limit: i64,
    unit: &str,
) {
    if let Some(observed) = observed {
        gates.push(GateResult::maximum_i64(
            id,
            observed,
            limit,
            unit,
            "warm baseline to final process-resource growth",
        ));
    } else {
        gates.push(unavailable_gate(
            id,
            "required OS resource measurement is unavailable",
        ));
    }
}

fn unavailable_gate(id: &str, detail: &str) -> GateResult {
    GateResult {
        id: id.into(),
        status: GateStatus::Fail,
        observed: Value::Null,
        limit: json!("available"),
        comparator: "measurement_required".into(),
        unit: "unavailable".into(),
        detail: detail.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    #[test]
    fn checked_in_profiles_are_valid_and_bounded() {
        let config: GateConfig =
            serde_json::from_str(include_str!("../../../perf/release-gates.json"))
                .expect("checked-in gate configuration");
        assert_eq!(config.schema_version, 1);
        assert_eq!(config.profiles.len(), 3);
        for profile in config.profiles.values() {
            profile.validate().expect("bounded gate profile");
            assert_eq!(profile.active_counts, vec![1, 2, 4, 8, 16, 32]);
            assert!(profile.active_retained_payload_bytes > MAX_RUN_QUESTION_BYTES);
        }
    }

    #[test]
    fn retained_payload_never_expands_the_bounded_run_question() {
        let config: GateConfig =
            serde_json::from_str(include_str!("../../../perf/release-gates.json"))
                .expect("checked-in gate configuration");
        let request = active_request(usize::MAX);
        assert!(active_request_is_bounded(&request));
        assert!(request.question.len() <= MAX_RUN_QUESTION_BYTES);
        assert!(
            config
                .profiles
                .values()
                .all(|profile| { profile.active_retained_payload_bytes > request.question.len() })
        );
        assert_eq!(
            request.context,
            RunContextV1::CompanyTickerSet {
                tickers: vec!["VG".into()]
            }
        );
    }

    #[test]
    fn checked_in_chat_matrix_fixture_matches_bounded_dimensions() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../fixtures/performance/v1/multi-user-chat-load-v1.json"
        ))
        .expect("chat matrix fixture");
        assert_eq!(fixture["schema_version"], 1);
        assert_eq!(fixture["principals"], CHAT_MATRIX_PRINCIPALS);
        assert_eq!(
            fixture["rooms_per_principal"],
            CHAT_MATRIX_ROOMS_PER_PRINCIPAL
        );
        assert_eq!(fixture["turns_per_room"], CHAT_MATRIX_TURNS_PER_ROOM);
        assert_eq!(
            fixture["queue_only_principals"],
            CHAT_MATRIX_QUEUE_ONLY_PRINCIPALS
        );
        assert_eq!(
            fixture["active_room_rss_samples"],
            serde_json::json!([1, 4, 16])
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn chat_matrix_preserves_room_scope_fairness_and_call_policy() {
        let report = run_chat_matrix().await.expect("deterministic chat matrix");
        assert_eq!(report.logical_runs, 120);
        assert_eq!(report.provider_turns, 240);
        assert_eq!(report.capability_calls, 240);
        assert!(report.same_room_serialized);
        assert!(report.turn_ordered);
        assert!(report.same_principal_rooms_parallel);
        assert!(report.principal_fair);
        assert!(report.scope_isolated);
        assert!(report.duplicate_run_rejected);
        assert_eq!(report.queue_only_tasks_spawned, 0);
        assert_eq!(report.queue_only_provider_clients, 0);
        assert_eq!(report.queue_only_mcp_clients, 0);
        assert_eq!(report.queue_only_db_connections, 0);
        assert!(report.peak_active_rooms <= CHAT_MATRIX_GLOBAL_CONCURRENCY);
        assert!(report.enqueue_to_claim_p99_us >= report.enqueue_to_claim_p95_us);
    }

    #[test]
    fn only_a_valid_post_tool_request_counts_as_measurement_phase() {
        let first = vec![ProviderMessage::user("bounded")];
        assert_eq!(
            provider_message_phase(&first),
            Ok(ProviderRequestPhase::FirstProvider)
        );

        let valid = vec![ProviderMessage::user(format!(
            "<verified-compacted-context>\n{}\n</verified-compacted-context>",
            json!({
                "schema_version": 1,
                "authority": "validated_state_and_committed_evidence_only",
                "research_projection": {"plan": "committed"},
                "evidence_index": [{"evidence_id": "perf-evidence"}],
                "retained_facts": [{"fact_ref": "perf-fact"}]
            })
        ))];
        assert_eq!(
            provider_message_phase(&valid),
            Ok(ProviderRequestPhase::Measurement)
        );

        let orientation_only = vec![ProviderMessage::user(format!(
            "<verified-compacted-context>\n{}\n</verified-compacted-context>",
            json!({
                "schema_version": 1,
                "authority": "validated_state_and_committed_evidence_only",
                "research_projection": null,
                "evidence_index": [{"evidence_id": "perf-orientation"}],
                "retained_facts": [{"fact_ref": "perf-orientation-fact"}]
            })
        ))];
        assert_eq!(
            provider_message_phase(&orientation_only),
            Ok(ProviderRequestPhase::PlannerAfterOrientation)
        );

        let legacy_raw_result = vec![
            ToolResultMessage::from_value(
                "call-valid",
                &json!({
                    "result": {"evidence_units": [{"summary": "rrrrrrrr"}]},
                    "kernel_research_goals": {"schema_version": 1, "bindings": []}
                }),
            )
            .unwrap()
            .into_provider_message(),
        ];
        assert!(provider_message_phase(&legacy_raw_result).is_err());

        let rejected = vec![
            ToolResultMessage::from_value(
                "call-rejected",
                &json!({"evidence_units": [{"summary": "too-small"}]}),
            )
            .unwrap()
            .into_provider_message(),
        ];
        assert!(provider_message_phase(&rejected).is_err());
        let rejected_snapshot = ActivePhaseSnapshot {
            rejected_phase_entries: 1,
            ..ActivePhaseSnapshot::default()
        };
        assert!(validate_active_phase(rejected_snapshot, 1, false).is_err());
    }

    #[tokio::test]
    async fn rejected_post_tool_request_never_enters_the_measurement_phase() {
        let workload = ActiveWorkload::load(&repo_root(), 8).expect("load active workload");
        let counters = Arc::new(ActivePhaseCounters::default());
        let provider = PostToolBlockingProvider {
            orientation_assistant: Arc::clone(&workload.orientation_assistant),
            planner_assistant: Arc::clone(&workload.planner_assistant),
            counters: Arc::clone(&counters),
            release: Arc::new(Semaphore::new(0)),
        };
        let request = MessagesRequest {
            model: "glm-5.3".into(),
            messages: vec![
                ToolResultMessage::from_value(
                    "call-rejected",
                    &json!({"evidence_units": [{"summary": "too-small"}]}),
                )
                .unwrap()
                .into_provider_message(),
            ],
            system: "test".into(),
            max_tokens: 128,
            tools: Vec::new(),
            tool_choice: None,
            output_config: None,
            response_format: None,
            thinking: krw_agent_provider_wire::ThinkingConfig {
                kind: ThinkingMode::Enabled,
                budget_tokens: Some(64),
            },
            stream: true,
            metadata: None,
        };
        let context = EpisodeContext {
            tool_schema_hash: ContentHash::sha256("tool-schema"),
            agent_image_hash: ContentHash::sha256("image"),
            api_version: "anthropic-messages-v1".into(),
        };

        assert!(provider.complete(&request, &context).await.is_err());
        let snapshot = counters.snapshot();
        assert_eq!(snapshot.rejected_phase_entries, 1);
        assert_eq!(snapshot.measurement_phase_entered, 0);
        assert_eq!(snapshot.measurement_phase_completed, 0);
    }

    #[tokio::test]
    async fn active_fixture_reaches_the_post_tool_measurement_phase() {
        let root = repo_root();
        let image = Arc::new(
            compile_agent_dir(root.join("agents/krw-ontology"))
                .expect("compile active fixture image")
                .into_loaded()
                .expect("load active fixture image"),
        );
        let deployment = Arc::new(active_deployment());
        let workload =
            Arc::new(ActiveWorkload::load(&root, 4 * 1024).expect("load bounded active workload"));
        let level = measure_active_level(1, workload, image, deployment)
            .await
            .expect("active fixture reaches measurement phase");
        assert_eq!(level.measurement_phase, ACTIVE_MEASUREMENT_PHASE);
        assert!(level.observed_retained_bytes_per_run >= 4 * 1024);
        assert_eq!(level.first_provider_entered, 1);
        assert_eq!(level.first_provider_completed, 1);
        assert_eq!(level.planner_provider_entered, 1);
        assert_eq!(level.planner_provider_completed, 1);
        assert_eq!(level.episode_checkpointed, 2);
        assert_eq!(level.capability_entered, 2);
        assert_eq!(level.capability_completed, 2);
        assert_eq!(level.action_accepted, 2);
        assert_eq!(level.run_state_checkpointed, 2);
        assert_eq!(level.measurement_phase_entered, 1);
        assert_eq!(level.measurement_phase_completed, 1);
        assert_eq!(level.rejected_phase_entries, 0);
        assert_eq!(level.spawned_tasks, level.joined_tasks);
    }

    #[test]
    fn impossible_profile_is_rejected_before_work_starts() {
        let config: GateConfig =
            serde_json::from_str(include_str!("../../../perf/release-gates.json"))
                .expect("checked-in gate configuration");
        let mut profile = config.profiles["ci"].clone();
        profile.active_counts = vec![65];
        assert!(matches!(
            profile.validate(),
            Err(HarnessError::InvalidThresholds)
        ));
    }

    #[test]
    fn linear_regression_reports_exact_slope() {
        let (slope, r_squared) =
            ordinary_least_squares(&[(1.0, 3.0), (2.0, 5.0), (4.0, 9.0), (8.0, 17.0)]);
        assert!((slope - 2.0).abs() < f64::EPSILON);
        assert!((r_squared - 1.0).abs() < f64::EPSILON);
    }
}
