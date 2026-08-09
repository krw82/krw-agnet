//! Offline performance and soak gates for the bounded KRW agent runtime.
//!
//! The binary is intentionally not linked into `krw-agentd`. Its process-wide
//! allocator instrumentation exists only to measure live allocations in this
//! disposable harness.

use std::alloc::System;
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use clap::{Parser, ValueEnum};
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
    ResolvedExecutionSnapshot, RunContextV1, RunRequest, ThinkingMode, TransportKind,
    provider_tool_name,
};
#[cfg(test)]
use krw_agent_provider_wire::ToolResultMessage;
use krw_agent_provider_wire::{
    AssistantMessage, ContentBlock, EpisodeContext, MessagesRequest, ProviderEpisodeV1,
    ProviderMessage, TokenUsage,
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
use tokio::sync::Semaphore;

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
    assistant: Arc<AssistantMessage>,
    capability_template: Arc<Value>,
    retained_payload_bytes: usize,
}

impl ActiveWorkload {
    fn load(root: &Path, retained_payload_bytes: usize) -> Result<Self, HarnessError> {
        let fixture = root.join("fixtures/vertical-slice/v1");
        let mut assistant: AssistantMessage =
            serde_json::from_slice(&fs::read(fixture.join("provider/turn-1-assistant.json"))?)?;
        if !assistant.tool_calls.is_empty() && assistant.content.is_none() {
            assistant.content = Some(String::new());
        }
        let mut capability_template: Value = serde_json::from_slice(&fs::read(
            fixture.join("mcp/research-state-answerable.json"),
        )?)?;
        match assistant.tool_calls.as_slice() {
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
        let summary = capability_template
            .pointer_mut("/evidence_units/0/summary")
            .ok_or(HarnessError::InvalidActiveFixture(
                "research-state summary is missing",
            ))?;
        *summary = Value::String("r".repeat(retained_payload_bytes));
        Ok(Self {
            assistant: Arc::new(assistant),
            capability_template: Arc::new(capability_template),
            retained_payload_bytes,
        })
    }
}

#[derive(Debug, Default)]
struct ActivePhaseCounters {
    first_provider_entered: AtomicUsize,
    first_provider_completed: AtomicUsize,
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
    Measurement,
}

fn provider_request_phase(
    request: &MessagesRequest,
    retained_payload_bytes: usize,
) -> Result<ProviderRequestPhase, &'static str> {
    provider_message_phase(&request.messages, retained_payload_bytes)
}

fn provider_message_phase(
    messages: &[ProviderMessage],
    retained_payload_bytes: usize,
) -> Result<ProviderRequestPhase, &'static str> {
    let mut tool_messages = messages.iter().flat_map(|message| {
        message.content.iter().filter_map(|block| match block {
            ContentBlock::ToolResult { content, .. } => Some(content.as_str()),
            _ => None,
        })
    });
    let Some(tool_message_text) = tool_messages.next() else {
        return Ok(ProviderRequestPhase::FirstProvider);
    };
    let tool_message: Value = serde_json::from_str(tool_message_text)
        .map_err(|_| "tool result was not canonical JSON text")?;
    // A lowered ResearchProposal receives the model-visible result envelope,
    // not the raw MCP payload.  Keep this assertion explicit: accepting the
    // old raw result would let the active-run memory benchmark silently stop
    // covering the real provider transcript contract.
    let research_state = tool_message
        .get("result")
        .ok_or("accepted tool result lacks the model-visible result envelope")?;
    if tool_messages.next().is_some()
        || research_state
            .pointer("/evidence_units/0/summary")
            .and_then(Value::as_str)
            .map(str::len)
            != Some(retained_payload_bytes)
    {
        return Err("second provider request lacks the accepted retained payload");
    }
    Ok(ProviderRequestPhase::Measurement)
}

#[derive(Debug)]
struct PostToolBlockingProvider {
    assistant: Arc<AssistantMessage>,
    retained_payload_bytes: usize,
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
        match provider_request_phase(request, self.retained_payload_bytes) {
            Ok(ProviderRequestPhase::FirstProvider) => {
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
                    assistant: (*self.assistant).clone(),
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
    template: Arc<Value>,
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
        let mut provider_content = (*self.template).clone();
        // The checked-in MCP fixture predates the kernel-owned ResearchProposal
        // lowering path and consequently carries its historical clause id.  A
        // real MCP response echoes the exact physical SearchPlan that it was
        // given, including every clause reference in coverage and evidence.
        // Rebase the synthetic response in the same way so this harness keeps
        // exercising the active provider -> planner -> capability path rather
        // than measuring an impossible response contract.
        let clause_id = invocation
            .arguments
            .pointer("/clauses/0/clause_id")
            .and_then(Value::as_str)
            .ok_or_else(|| active_fixture_failure("compiled SearchPlan is missing clause id"))?;
        replace_active_fixture_clause_reference(
            &mut provider_content,
            "cash_generation",
            clause_id,
        );
        provider_content["plan"] = invocation.arguments.clone();
        let payload_hash = ContentHash::sha256("perf-retained-capability-result");
        let result = CapabilityResult {
            provider_content,
            evidence: vec![EvidenceRecord {
                evidence_id: format!("perf-evidence-{}", invocation.action_key),
                content_hash: payload_hash.clone(),
                source: EvidenceSource {
                    capability_id: invocation.capability_id.clone(),
                    action_key: invocation.action_key.clone(),
                    server_build: invocation.binding.server_build.clone(),
                    normalized_contract_hash: invocation.normalized_output_contract_hash.clone(),
                    server_schema_bundle_hash: invocation.binding.server_schema_bundle_hash.clone(),
                    data_release_hash: invocation.binding.data_release_hash.clone(),
                },
                scope: EvidenceScope {
                    auth_scope: AuthScope::Tenant,
                    scope_hash: ContentHash::sha256("perf-tenant-scope"),
                },
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
        };
        self.counters
            .capability_completed
            .fetch_add(1, Ordering::SeqCst);
        Ok(result)
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
    counters: Arc<ActivePhaseCounters>,
}

impl ActivePersistence {
    fn reset(&self) -> Result<(), HarnessError> {
        self.actions
            .lock()
            .map_err(|_| HarnessError::ActiveStart("persistence lock poisoned".into()))?
            .clear();
        Ok(())
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
        assistant: Arc::clone(&workload.assistant),
        retained_payload_bytes: workload.retained_payload_bytes,
        counters: Arc::clone(&counters),
        release: Arc::clone(&release),
    });
    let persistence = Arc::new(ActivePersistence {
        actions: Mutex::new(BTreeMap::new()),
        counters: Arc::clone(&counters),
    });
    let engine = Arc::new(RunEngine::new(
        provider,
        Arc::new(RetainedCapability {
            template: Arc::clone(&workload.capability_template),
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
        || snapshot.episode_checkpointed != runs
        || snapshot.capability_entered != runs
        || snapshot.capability_completed != runs
        || snapshot.action_accepted != runs
        || snapshot.run_state_checkpointed != runs
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
    DeploymentBinding {
        schema_version: 3,
        deployment_id: "perf-offline".into(),
        capabilities: vec![CapabilityBinding {
            binding_key: "krw_ontology_query_context".into(),
            mcp_tool_name: "krw_ontology_query_context".into(),
            transport: TransportKind::McpHttp,
            endpoint_ref: "offline".into(),
            credential_ref: None,
            auth_scope: AuthScope::Public,
            tool_session_reuse: McpToolSessionReuse::RunScoped,
            server_schema_bundle_hash: ContentHash::sha256("perf-schema"),
            server_build: "perf-offline".into(),
            data_release_hash: ContentHash::sha256("perf-release"),
            max_connections: 1,
            request_timeout_ms: 1_000,
        }],
    }
}

fn active_request(index: usize) -> RunRequest {
    let mut capability_call_limits = BTreeMap::new();
    capability_call_limits.insert("ontology.query_context".into(), 1);
    RunRequest {
        run_id: format!("perf-run-{index}"),
        session_id: format!("perf-session-{index}"),
        tenant_id: format!("perf-tenant-{}", index % 4),
        principal_id: format!("perf-principal-{index}"),
        run_kind: "company_research".into(),
        locale: "ko-KR".into(),
        // Keep the synthetic active-run request aligned with the immutable
        // fixture's exact user-span anchor. The model fixture is a real
        // ResearchIntent, so substituting a semantically similar question
        // would correctly fail the production anchor validator.
        question: "VG의 현금창출력이 공시 근거로 확인되는지 설명해줘".into(),
        requested_model: "glm-5.2".into(),
        model_profile: "glm_high".into(),
        budget: BudgetLimits {
            max_provider_turns: 3,
            max_capability_calls: 2,
            max_replans: 1,
            max_repairs: 1,
            max_input_tokens: 4_096,
            // The active image reserves 5,120 tokens for a complete final
            // answer and requires at least one viable 2,048-token research
            // decision before that reserve is consumed.  A synthetic
            // benchmark request must satisfy the same immutable execution
            // contract as a real run; otherwise it measures rejected-input
            // handling rather than the post-tool active phase.
            max_output_tokens: 12_000,
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
        provider_wire_capabilities: ProviderWireCapabilities::glm_5_2(),
        thinking: ThinkingMode::Enabled,
        reasoning_effort: Some(ReasoningEffort::High),
        capability_release_hashes: BTreeMap::from([(
            "ontology.query_context".into(),
            deployment.capabilities[0].data_release_hash.clone(),
        )]),
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
        transport: TransportKind::McpHttp,
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
    fn only_a_valid_post_tool_request_counts_as_measurement_phase() {
        let first = vec![ProviderMessage::user("bounded")];
        assert_eq!(
            provider_message_phase(&first, 8),
            Ok(ProviderRequestPhase::FirstProvider)
        );

        let valid = vec![
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
        assert_eq!(
            provider_message_phase(&valid, 8),
            Ok(ProviderRequestPhase::Measurement)
        );

        let rejected = vec![
            ToolResultMessage::from_value(
                "call-rejected",
                &json!({"evidence_units": [{"summary": "too-small"}]}),
            )
            .unwrap()
            .into_provider_message(),
        ];
        assert!(provider_message_phase(&rejected, 8).is_err());
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
            assistant: Arc::clone(&workload.assistant),
            retained_payload_bytes: workload.retained_payload_bytes,
            counters: Arc::clone(&counters),
            release: Arc::new(Semaphore::new(0)),
        };
        let request = MessagesRequest {
            model: "glm-5.2".into(),
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
        assert_eq!(level.episode_checkpointed, 1);
        assert_eq!(level.capability_entered, 1);
        assert_eq!(level.capability_completed, 1);
        assert_eq!(level.action_accepted, 1);
        assert_eq!(level.run_state_checkpointed, 1);
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
