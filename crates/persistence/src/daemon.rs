//! Claim, lease, failure, graceful-drain, and outbox orchestration for the
//! machine-wide daemon.
//!
//! Provider and capability execution is intentionally injected. This layer
//! owns only durable work admission and delivery. A run executor must take the
//! [`LeaseCoordinator`] mutation guard around every `agent_v1` mutation and
//! advance it with the returned receipt; that serializes mutations with lease
//! heartbeats and prevents `run_version` races.

use std::collections::{BTreeSet, hash_map::DefaultHasher};
use std::fmt;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use krw_agent_protocol::ContentHash;
use serde_json::{Value, json};
use thiserror::Error;
use tokio::sync::{Mutex, OwnedMutexGuard};
use tokio::task::JoinSet;
use tokio::time::{sleep, timeout};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};
use zeroize::Zeroizing;

use crate::agent_v1::{
    AckOutboxRequest, AckOutboxResponse, ActionAbiReceipt, AgentV1Client, AgentV1Error,
    CheckpointRunStateResponse, ClaimOutboxRequest, ClaimOutboxResponse, ClaimReceipt,
    ClaimRunRequest, ClaimRunResponse, FailOrDeferRequest, FailOrDeferResponse, FailureDisposition,
    JsonProcedureExecutor, MutationReceipt, OutboxReceipt, RejectionKind, RenewLeaseRequest,
};

const MAX_IN_FLIGHT_RUNS: usize = 256;
const MAX_OUTBOX_BATCH: u16 = 100;
const MAX_RESIDENT_RECOVERY_BYTES: usize = 1024 * 1024 * 1024;
const MAX_RECOVERY_EPISODES: usize = 256;
const MAX_RECOVERY_ACTIONS: usize = 512;
const MAX_RECOVERY_STATE_BYTES: usize = 8 * 1024 * 1024;
const MIN_LEASE: Duration = Duration::from_secs(1);
const MAX_LEASE: Duration = Duration::from_mins(10);

#[async_trait]
pub trait AgentV1Store: fmt::Debug + Send + Sync {
    async fn claim_run(&self, request: &ClaimRunRequest) -> Result<ClaimRunResponse, AgentV1Error>;
    async fn renew_lease(
        &self,
        request: &RenewLeaseRequest,
    ) -> Result<MutationReceipt, AgentV1Error>;
    async fn fail_or_defer(
        &self,
        request: &FailOrDeferRequest,
    ) -> Result<FailOrDeferResponse, AgentV1Error>;
    async fn claim_outbox(
        &self,
        request: &ClaimOutboxRequest,
    ) -> Result<ClaimOutboxResponse, AgentV1Error>;
    async fn ack_outbox(
        &self,
        request: &AckOutboxRequest,
    ) -> Result<AckOutboxResponse, AgentV1Error>;
}

#[async_trait]
impl<E> AgentV1Store for AgentV1Client<E>
where
    E: JsonProcedureExecutor,
{
    async fn claim_run(&self, request: &ClaimRunRequest) -> Result<ClaimRunResponse, AgentV1Error> {
        self.execute(request).await
    }

    async fn renew_lease(
        &self,
        request: &RenewLeaseRequest,
    ) -> Result<MutationReceipt, AgentV1Error> {
        self.execute(request).await
    }

    async fn fail_or_defer(
        &self,
        request: &FailOrDeferRequest,
    ) -> Result<FailOrDeferResponse, AgentV1Error> {
        self.execute(request).await
    }

    async fn claim_outbox(
        &self,
        request: &ClaimOutboxRequest,
    ) -> Result<ClaimOutboxResponse, AgentV1Error> {
        self.execute(request).await
    }

    async fn ack_outbox(
        &self,
        request: &AckOutboxRequest,
    ) -> Result<AckOutboxResponse, AgentV1Error> {
        self.execute(request).await
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunWorkerConfig {
    pub worker_id: String,
    pub runtime_version: String,
    pub accepted_agent_image_hashes: Vec<ContentHash>,
    pub max_in_flight: usize,
    pub lease_duration: Duration,
    pub heartbeat_interval: Duration,
    pub idle_backoff_min: Duration,
    pub idle_backoff_max: Duration,
    pub error_backoff_min: Duration,
    pub error_backoff_max: Duration,
    pub recovery_preflight_timeout: Duration,
    pub max_recovery_artifact_bytes: usize,
    pub max_recovery_total_bytes: usize,
    pub drain_timeout: Duration,
    pub forced_abort_timeout: Duration,
}

impl RunWorkerConfig {
    pub fn validate(&self) -> Result<(), DaemonConfigError> {
        validate_identifier(&self.worker_id, 128, "worker_id")?;
        validate_identifier(&self.runtime_version, 64, "runtime_version")?;
        if self.accepted_agent_image_hashes.is_empty()
            || self.accepted_agent_image_hashes.len() > 64
        {
            return Err(DaemonConfigError::Invalid("accepted image count"));
        }
        if !(1..=MAX_IN_FLIGHT_RUNS).contains(&self.max_in_flight) {
            return Err(DaemonConfigError::Invalid("max_in_flight"));
        }
        if !(MIN_LEASE..=MAX_LEASE).contains(&self.lease_duration) {
            return Err(DaemonConfigError::Invalid("lease_duration"));
        }
        if self.heartbeat_interval.is_zero() || self.heartbeat_interval > self.lease_duration / 3 {
            return Err(DaemonConfigError::Invalid("heartbeat_interval"));
        }
        validate_backoff(self.idle_backoff_min, self.idle_backoff_max, "idle backoff")?;
        validate_backoff(
            self.error_backoff_min,
            self.error_backoff_max,
            "error backoff",
        )?;
        if self.recovery_preflight_timeout.is_zero()
            || self.recovery_preflight_timeout >= self.lease_duration
        {
            return Err(DaemonConfigError::Invalid("recovery preflight timeout"));
        }
        if !(1_024..=64 * 1024 * 1024).contains(&self.max_recovery_artifact_bytes)
            || self.max_recovery_total_bytes < self.max_recovery_artifact_bytes
            || self.max_recovery_total_bytes > 128 * 1024 * 1024
            || self
                .max_recovery_total_bytes
                .checked_mul(self.max_in_flight)
                .is_none_or(|resident| resident > MAX_RESIDENT_RECOVERY_BYTES)
        {
            return Err(DaemonConfigError::Invalid("recovery memory bounds"));
        }
        if self.drain_timeout.is_zero() || self.forced_abort_timeout.is_zero() {
            return Err(DaemonConfigError::Invalid("shutdown timeout"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxConfig {
    pub worker_id: String,
    pub batch_size: u16,
    pub lease_duration: Duration,
    pub handler_timeout: Duration,
    pub idle_backoff_min: Duration,
    pub idle_backoff_max: Duration,
    pub error_backoff_min: Duration,
    pub error_backoff_max: Duration,
}

impl OutboxConfig {
    pub fn validate(&self) -> Result<(), DaemonConfigError> {
        validate_identifier(&self.worker_id, 128, "outbox worker_id")?;
        if !(1..=MAX_OUTBOX_BATCH).contains(&self.batch_size) {
            return Err(DaemonConfigError::Invalid("outbox batch size"));
        }
        if !(MIN_LEASE..=MAX_LEASE).contains(&self.lease_duration)
            || self.handler_timeout.is_zero()
            || self.handler_timeout >= self.lease_duration
        {
            return Err(DaemonConfigError::Invalid("outbox lease/handler timeout"));
        }
        validate_backoff(
            self.idle_backoff_min,
            self.idle_backoff_max,
            "outbox idle backoff",
        )?;
        validate_backoff(
            self.error_backoff_min,
            self.error_backoff_max,
            "outbox error backoff",
        )?;
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum DaemonConfigError {
    #[error("invalid daemon setting: {0}")]
    Invalid(&'static str),
}

#[derive(Debug, Error)]
pub enum SupervisorError {
    #[error(transparent)]
    Configuration(#[from] DaemonConfigError),
    #[error("fatal claim ABI error: {0}")]
    Claim(AgentV1Error),
    #[error("run worker task failed ({0})")]
    WorkerTask(ContentHash),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuccessfulRunOutcome {
    Committed,
    Cancelled,
}

#[derive(Clone, PartialEq)]
pub struct RunExecutionFailure {
    pub retryable: bool,
    pub reason_code: String,
    pub retry_delay: Duration,
    pub release: Value,
}

impl fmt::Debug for RunExecutionFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RunExecutionFailure")
            .field("retryable", &self.retryable)
            .field("reason_code", &self.reason_code)
            .field("retry_delay", &self.retry_delay)
            .field("release", &"[REDACTED]")
            .finish()
    }
}

impl RunExecutionFailure {
    pub fn deferred(reason_code: impl Into<String>, retry_delay: Duration) -> Self {
        Self {
            retryable: true,
            reason_code: reason_code.into(),
            retry_delay,
            release: json!({}),
        }
    }

    pub fn failed(reason_code: impl Into<String>) -> Self {
        Self {
            retryable: false,
            reason_code: reason_code.into(),
            retry_delay: Duration::ZERO,
            release: json!({}),
        }
    }

    /// Attach a bounded, non-content-bearing terminal release payload.
    ///
    /// The supervisor persists this value only through the failure settlement
    /// ABI. Callers must never use it for prompts, provider payloads, tool
    /// results, or raw diagnostic text. It exists so a durable failure can
    /// retain a release-safe fingerprint without opening an ad-hoc logging
    /// path for private run artifacts.
    #[must_use]
    pub fn with_release(mut self, release: Value) -> Self {
        self.release = release;
        self
    }
}

#[async_trait]
pub trait ClaimedRunExecutor: fmt::Debug + Send + Sync {
    /// `Committed` means the executor already completed the atomic final ABI
    /// transaction; `Cancelled` means it observed the durable terminal cancel.
    /// Returning success does not ask this supervisor to synthesize a terminal
    async fn execute(
        &self,
        context: ClaimedRunContext,
    ) -> Result<SuccessfulRunOutcome, RunExecutionFailure>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryArtifactKind {
    RuntimeState,
    ProviderEpisode,
    ActionArguments,
    ActionResult,
}

#[derive(Clone, PartialEq, Eq)]
pub struct RecoveryArtifactRequest {
    pub kind: RecoveryArtifactKind,
    pub run_id: String,
    pub tenant_id: String,
    pub principal_id: String,
    pub action_key: Option<String>,
    pub artifact_ref: String,
    pub expected_hash: ContentHash,
    pub expected_size_bytes: Option<usize>,
    pub schema_hash: Option<ContentHash>,
}

impl fmt::Debug for RecoveryArtifactRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RecoveryArtifactRequest")
            .field("kind", &self.kind)
            .field("run_id_hash", &ContentHash::sha256(&self.run_id))
            .field("tenant_id_hash", &ContentHash::sha256(&self.tenant_id))
            .field(
                "principal_id_hash",
                &ContentHash::sha256(&self.principal_id),
            )
            .field(
                "action_key_hash",
                &self.action_key.as_ref().map(ContentHash::sha256),
            )
            .field("artifact_ref", &"[REDACTED]")
            .field("expected_hash", &self.expected_hash)
            .field("expected_size_bytes", &self.expected_size_bytes)
            .field("schema_hash", &self.schema_hash)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct RecoveryArtifactFailure {
    pub diagnostic_hash: ContentHash,
    /// When `false` the failure is permanent — retrying will never succeed.
    /// Typical cause: the referenced artifact was never written to this
    /// worker's local store (stale checkpoint from a previous daemon
    /// incarnation). Retrying only burns claim cycles, so the daemon
    /// should `fail` instead of `defer`.
    pub retryable: bool,
}

impl RecoveryArtifactFailure {
    pub fn redacted(diagnostic: impl AsRef<[u8]>) -> Self {
        Self {
            diagnostic_hash: ContentHash::sha256(diagnostic),
            retryable: true,
        }
    }

    /// Like `redacted` but marks the failure as non-retryable. Use when
    /// the underlying cause (e.g. a missing artifact file) cannot be
    /// resolved by re-claiming the run.
    pub fn permanent(diagnostic: impl AsRef<[u8]>) -> Self {
        Self {
            diagnostic_hash: ContentHash::sha256(diagnostic),
            retryable: false,
        }
    }
}

impl fmt::Debug for RecoveryArtifactFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RecoveryArtifactFailure")
            .field("diagnostic_hash", &self.diagnostic_hash)
            .field("retryable", &self.retryable)
            .finish()
    }
}

/// Typed boundary to encrypted/content-addressed artifact storage. The
/// implementation must authenticate the tenant/principal/run scope, enforce `max_bytes`,
/// and return the referenced plaintext only in zeroizing memory. The daemon
/// independently verifies `expected_hash`; bytes never enter `PostgreSQL` or
/// ordinary logs.
#[async_trait]
pub trait RecoveryArtifactStore: fmt::Debug + Send + Sync {
    async fn load_verified(
        &self,
        request: &RecoveryArtifactRequest,
        max_bytes: usize,
    ) -> Result<Zeroizing<Vec<u8>>, RecoveryArtifactFailure>;
}

pub struct VerifiedRecoveryArtifact {
    pub kind: RecoveryArtifactKind,
    pub action_key: Option<String>,
    pub expected_hash: ContentHash,
    pub schema_hash: Option<ContentHash>,
    bytes: Zeroizing<Vec<u8>>,
}

impl VerifiedRecoveryArtifact {
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

impl fmt::Debug for VerifiedRecoveryArtifact {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VerifiedRecoveryArtifact")
            .field("kind", &self.kind)
            .field(
                "action_key_hash",
                &self.action_key.as_ref().map(ContentHash::sha256),
            )
            .field("expected_hash", &self.expected_hash)
            .field("schema_hash", &self.schema_hash)
            .field("bytes", &"[REDACTED]")
            .field("byte_len", &self.bytes.len())
            .finish()
    }
}

#[derive(Debug, Default)]
pub struct VerifiedRecoveryArtifacts {
    artifacts: Vec<VerifiedRecoveryArtifact>,
    total_bytes: usize,
}

impl VerifiedRecoveryArtifacts {
    pub fn artifacts(&self) -> &[VerifiedRecoveryArtifact] {
        &self.artifacts
    }

    pub const fn total_bytes(&self) -> usize {
        self.total_bytes
    }
}

#[derive(Clone)]
pub struct ClaimedRunContext {
    receipt: Arc<ClaimReceipt>,
    pub lease: LeaseCoordinator,
    pub cancellation: CancellationToken,
    pub recovery: Arc<VerifiedRecoveryArtifacts>,
}

impl ClaimedRunContext {
    pub fn receipt(&self) -> &ClaimReceipt {
        &self.receipt
    }
}

impl fmt::Debug for ClaimedRunContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ClaimedRunContext")
            .field("run_id_hash", &ContentHash::sha256(&self.receipt.run_id))
            .field("fencing_token", &self.receipt.fencing_token)
            .field("reclaimed", &self.receipt.reclaimed)
            .field("immutable_snapshot", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseSnapshot {
    pub fencing_token: u64,
    pub run_version: u64,
    pub cancel_generation: u64,
    pub checkpoint_seq: u64,
    pub action_frontier_seq: u64,
    pub action_frontier_hash: ContentHash,
}

#[derive(Debug)]
struct LeaseState {
    snapshot: LeaseSnapshot,
    local_deadline: Instant,
    lost: bool,
    terminal: bool,
}

#[derive(Debug, Clone)]
pub struct LeaseCoordinator {
    state: Arc<Mutex<LeaseState>>,
}

impl LeaseCoordinator {
    fn from_claim(
        receipt: &ClaimReceipt,
        lease_duration: Duration,
        claim_round_trip: Duration,
    ) -> Self {
        Self {
            state: Arc::new(Mutex::new(LeaseState {
                snapshot: LeaseSnapshot {
                    fencing_token: receipt.fencing_token,
                    run_version: receipt.run_version,
                    cancel_generation: receipt.cancel_generation,
                    checkpoint_seq: receipt.checkpoint_seq,
                    action_frontier_seq: receipt.action_frontier_seq,
                    action_frontier_hash: receipt.action_frontier_hash.clone(),
                },
                // The database starts the lease before the response reaches
                // us. Subtract the whole round trip as a conservative bound.
                local_deadline: Instant::now() + lease_duration.saturating_sub(claim_round_trip),
                lost: false,
                terminal: false,
            })),
        }
    }

    /// Acquire the per-run mutation lane. Production executors must hold this
    /// across each procedure call that checks or advances `run_version`.
    pub async fn mutation_guard(&self) -> Result<LeaseMutationGuard, LeaseUnavailable> {
        let guard = Arc::clone(&self.state).lock_owned().await;
        if guard.lost || guard.terminal {
            return Err(LeaseUnavailable);
        }
        Ok(LeaseMutationGuard { guard })
    }

    pub async fn is_lost(&self) -> bool {
        self.state.lock().await.lost
    }

    async fn mark_terminal(&self) {
        self.state.lock().await.terminal = true;
    }
}

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
#[error("run lease is no longer available")]
pub struct LeaseUnavailable;

#[derive(Debug)]
pub struct LeaseMutationGuard {
    guard: OwnedMutexGuard<LeaseState>,
}

impl LeaseMutationGuard {
    pub fn snapshot(&self) -> LeaseSnapshot {
        self.guard.snapshot.clone()
    }

    pub fn observe_mutation(&mut self, receipt: &MutationReceipt, remaining_lease: Duration) {
        self.guard.snapshot.run_version = receipt.run_version;
        self.guard.snapshot.cancel_generation = receipt.cancel_generation;
        self.guard.snapshot.checkpoint_seq = receipt.checkpoint_seq;
        self.guard.local_deadline = Instant::now() + remaining_lease;
    }

    /// Advance the serialized mutation lane after an action transition.
    pub fn observe_action(&mut self, receipt: &ActionAbiReceipt) {
        self.guard.snapshot.run_version = receipt.run_version;
        self.guard.snapshot.action_frontier_seq = receipt.action_frontier_seq;
        self.guard.snapshot.action_frontier_hash = receipt.action_frontier_hash.clone();
    }

    /// Advance the serialized mutation lane after a runtime-state checkpoint.
    pub fn observe_run_state(&mut self, receipt: &CheckpointRunStateResponse) {
        self.guard.snapshot.run_version = receipt.run_version;
        self.guard.snapshot.checkpoint_seq = receipt.provider_checkpoint_seq;
        self.guard.snapshot.action_frontier_seq = receipt.action_frontier_seq;
        self.guard.snapshot.action_frontier_hash = receipt.action_frontier_hash.clone();
    }

    /// Update the version after another ABI response (action/final/failure).
    pub fn observe_version(
        &mut self,
        run_version: u64,
        cancel_generation: Option<u64>,
        checkpoint_seq: Option<u64>,
    ) {
        self.guard.snapshot.run_version = run_version;
        if let Some(cancel_generation) = cancel_generation {
            self.guard.snapshot.cancel_generation = cancel_generation;
        }
        if let Some(checkpoint_seq) = checkpoint_seq {
            self.guard.snapshot.checkpoint_seq = checkpoint_seq;
        }
    }

    pub fn mark_terminal(&mut self) {
        self.guard.terminal = true;
    }
}

#[derive(Debug)]
pub struct RunSupervisor {
    store: Arc<dyn AgentV1Store>,
    executor: Arc<dyn ClaimedRunExecutor>,
    recovery_artifacts: Arc<dyn RecoveryArtifactStore>,
    config: RunWorkerConfig,
}

impl RunSupervisor {
    pub fn new(
        store: Arc<dyn AgentV1Store>,
        executor: Arc<dyn ClaimedRunExecutor>,
        recovery_artifacts: Arc<dyn RecoveryArtifactStore>,
        config: RunWorkerConfig,
    ) -> Result<Self, DaemonConfigError> {
        config.validate()?;
        Ok(Self {
            store,
            executor,
            recovery_artifacts,
            config,
        })
    }

    pub async fn run(&self, shutdown: CancellationToken) -> Result<(), SupervisorError> {
        let forced_cancellation = CancellationToken::new();
        let mut tasks = JoinSet::new();
        let mut idle_attempt = 0_u32;
        let mut error_attempt = 0_u32;

        loop {
            if shutdown.is_cancelled() {
                break;
            }
            while tasks.len() >= self.config.max_in_flight {
                tokio::select! {
                    biased;
                    () = shutdown.cancelled() => break,
                    joined = tasks.join_next() => handle_joined(joined)?,
                }
                if shutdown.is_cancelled() {
                    break;
                }
            }
            if shutdown.is_cancelled() {
                break;
            }

            let request = ClaimRunRequest {
                worker_id: self.config.worker_id.clone(),
                lease_ms: duration_millis_u64(self.config.lease_duration),
                runtime_version: self.config.runtime_version.clone(),
                accepted_agent_image_hashes: self.config.accepted_agent_image_hashes.clone(),
            };
            let claim_started = Instant::now();
            let claim = tokio::select! {
                biased;
                () = shutdown.cancelled() => break,
                result = self.store.claim_run(&request) => result,
            };
            match claim {
                Ok(ClaimRunResponse {
                    claimed: true,
                    receipt: Some(receipt),
                }) => {
                    idle_attempt = 0;
                    error_attempt = 0;
                    let store = Arc::clone(&self.store);
                    let executor = Arc::clone(&self.executor);
                    let recovery_artifacts = Arc::clone(&self.recovery_artifacts);
                    let config = self.config.clone();
                    let claim_round_trip = claim_started.elapsed();
                    let run_cancellation = forced_cancellation.child_token();
                    tasks.spawn(async move {
                        drive_claim(
                            store,
                            executor,
                            recovery_artifacts,
                            config,
                            receipt,
                            claim_round_trip,
                            run_cancellation,
                        )
                        .await;
                    });
                }
                Ok(ClaimRunResponse {
                    claimed: false,
                    receipt: None,
                }) => {
                    error_attempt = 0;
                    let delay = jittered_backoff(
                        self.config.idle_backoff_min,
                        self.config.idle_backoff_max,
                        idle_attempt,
                        &self.config.worker_id,
                    );
                    idle_attempt = idle_attempt.saturating_add(1);
                    wait_for_work_or_shutdown(&mut tasks, &shutdown, delay).await?;
                }
                Ok(_) => {
                    return Err(SupervisorError::Claim(AgentV1Error::InvalidResponse {
                        procedure: crate::agent_v1::AgentV1Procedure::ClaimRun,
                        reason: "claim response receipt invariant violated".into(),
                    }));
                }
                Err(error) if is_retryable_claim_error(&error) => {
                    let delay = jittered_backoff(
                        self.config.error_backoff_min,
                        self.config.error_backoff_max,
                        error_attempt,
                        &self.config.worker_id,
                    );
                    error_attempt = error_attempt.saturating_add(1);
                    warn!(
                        diagnostic_hash = %error_diagnostic_hash(&error),
                        attempt = error_attempt,
                        "retryable claim failure (database or response-shape drift)"
                    );
                    wait_for_work_or_shutdown(&mut tasks, &shutdown, delay).await?;
                }
                Err(error) => return Err(SupervisorError::Claim(error)),
            }
        }

        drain_tasks(
            &mut tasks,
            &forced_cancellation,
            self.config.drain_timeout,
            self.config.forced_abort_timeout,
        )
        .await
    }
}

async fn drive_claim(
    store: Arc<dyn AgentV1Store>,
    executor: Arc<dyn ClaimedRunExecutor>,
    recovery_artifacts: Arc<dyn RecoveryArtifactStore>,
    config: RunWorkerConfig,
    receipt: ClaimReceipt,
    claim_round_trip: Duration,
    cancellation: CancellationToken,
) {
    let run_started = Instant::now();
    crate::metrics::record_run_started();
    let lease = LeaseCoordinator::from_claim(&receipt, config.lease_duration, claim_round_trip);
    let heartbeat_stop = CancellationToken::new();
    let heartbeat = tokio::spawn(heartbeat_loop(
        Arc::clone(&store),
        config.clone(),
        receipt.run_id.clone(),
        receipt.tenant_id.clone(),
        lease.clone(),
        cancellation.clone(),
        heartbeat_stop.clone(),
    ));
    let recovery_ready = timeout(
        config.recovery_preflight_timeout,
        load_recovery_artifacts(
            &receipt,
            &*recovery_artifacts,
            config.max_recovery_artifact_bytes,
            config.max_recovery_total_bytes,
        ),
    )
    .await;
    let outcome = match recovery_ready {
        Ok(Ok(recovery)) => {
            let context = ClaimedRunContext {
                receipt: Arc::new(receipt.clone()),
                lease: lease.clone(),
                cancellation: cancellation.clone(),
                recovery: Arc::new(recovery),
            };
            executor.execute(context).await
        }
        Ok(Err(failure)) => {
            warn!(
                run_id_hash = %ContentHash::sha256(&receipt.run_id),
                diagnostic_hash = %failure.diagnostic_hash,
                retryable = failure.retryable,
                "claim recovery artifact preflight failed"
            );
            if failure.retryable {
                Err(RunExecutionFailure::deferred(
                    "recovery_artifact_unavailable",
                    Duration::from_secs(5),
                ))
            } else {
                Err(RunExecutionFailure::failed(
                    "recovery_artifact_permanently_missing",
                ))
            }
        }
        Err(_) => Err(RunExecutionFailure::deferred(
            "recovery_artifact_timeout",
            Duration::from_secs(5),
        )),
    };
    heartbeat_stop.cancel();
    if let Err(join_error) = heartbeat.await {
        warn!(
            diagnostic_hash = %ContentHash::sha256(format!("{join_error:?}")),
            "lease heartbeat task failed"
        );
    }

    match outcome {
        Ok(SuccessfulRunOutcome::Committed) => {
            lease.mark_terminal().await;
            crate::metrics::record_run_outcome(
                crate::metrics::OUTCOME_FINAL,
                run_started.elapsed(),
            );
        }
        Ok(SuccessfulRunOutcome::Cancelled) => {
            lease.mark_terminal().await;
            crate::metrics::record_run_outcome(
                crate::metrics::OUTCOME_CANCELLED,
                run_started.elapsed(),
            );
        }
        Err(failure) => {
            if lease.is_lost().await {
                debug!(
                    run_id_hash = %ContentHash::sha256(&receipt.run_id),
                    "run stopped after lease loss; no stale failure mutation attempted"
                );
                crate::metrics::record_run_outcome(
                    crate::metrics::OUTCOME_FAILED,
                    run_started.elapsed(),
                );
                return;
            }
            if let Err(error) =
                persist_execution_failure(&*store, &config, &receipt, &lease, failure).await
                && !is_fence_or_lease_loss(&error)
            {
                warn!(
                    run_id_hash = %ContentHash::sha256(&receipt.run_id),
                    diagnostic_hash = %error_diagnostic_hash(&error),
                    "could not persist run failure disposition; lease expiry will recover it"
                );
            }
            crate::metrics::record_run_outcome(
                crate::metrics::OUTCOME_FAILED,
                run_started.elapsed(),
            );
        }
    }
}

async fn load_recovery_artifacts(
    receipt: &ClaimReceipt,
    artifacts: &dyn RecoveryArtifactStore,
    max_artifact_bytes: usize,
    max_total_bytes: usize,
) -> Result<VerifiedRecoveryArtifacts, RecoveryArtifactFailure> {
    validate_recovery_receipt(receipt)?;
    let mut requests = Vec::new();
    if let Some(state) = &receipt.recovery.state_checkpoint {
        let state_size_bytes = usize::try_from(state.state_size_bytes).map_err(|_| {
            RecoveryArtifactFailure::redacted("recovery_state_size_not_representable")
        })?;
        if state_size_bytes > max_artifact_bytes || state_size_bytes > MAX_RECOVERY_STATE_BYTES {
            return Err(RecoveryArtifactFailure::redacted(
                "recovery_state_size_exceeded",
            ));
        }
        requests.push(RecoveryArtifactRequest {
            kind: RecoveryArtifactKind::RuntimeState,
            run_id: receipt.run_id.clone(),
            tenant_id: receipt.tenant_id.clone(),
            principal_id: receipt.principal_id.clone(),
            action_key: None,
            artifact_ref: state.state_artifact_ref.clone(),
            expected_hash: state.state_hash.clone(),
            expected_size_bytes: Some(state_size_bytes),
            schema_hash: Some(state.recovery_schema_hash.clone()),
        });
    }
    for episode in &receipt.recovery.episodes {
        requests.push(RecoveryArtifactRequest {
            kind: RecoveryArtifactKind::ProviderEpisode,
            run_id: receipt.run_id.clone(),
            tenant_id: receipt.tenant_id.clone(),
            principal_id: receipt.principal_id.clone(),
            action_key: None,
            artifact_ref: episode.episode_artifact_ref.clone(),
            expected_hash: episode.episode_hash.clone(),
            expected_size_bytes: None,
            schema_hash: None,
        });
    }
    for action in &receipt.recovery.actions {
        requests.push(RecoveryArtifactRequest {
            kind: RecoveryArtifactKind::ActionArguments,
            run_id: receipt.run_id.clone(),
            tenant_id: receipt.tenant_id.clone(),
            principal_id: receipt.principal_id.clone(),
            action_key: Some(action.action_key.clone()),
            artifact_ref: action.arguments_artifact_ref.clone(),
            expected_hash: action.request_hash.clone(),
            expected_size_bytes: None,
            schema_hash: None,
        });
        match (&action.result_hash, &action.result_artifact_ref) {
            (Some(result_hash), Some(result_artifact_ref)) => {
                requests.push(RecoveryArtifactRequest {
                    kind: RecoveryArtifactKind::ActionResult,
                    run_id: receipt.run_id.clone(),
                    tenant_id: receipt.tenant_id.clone(),
                    principal_id: receipt.principal_id.clone(),
                    action_key: Some(action.action_key.clone()),
                    artifact_ref: result_artifact_ref.clone(),
                    expected_hash: result_hash.clone(),
                    expected_size_bytes: None,
                    schema_hash: None,
                });
            }
            (None, None) => {}
            _ => {
                return Err(RecoveryArtifactFailure::redacted(
                    "inconsistent_recovery_result_receipt",
                ));
            }
        }
    }
    let mut verified = VerifiedRecoveryArtifacts::default();
    for request in requests {
        let bytes = artifacts
            .load_verified(&request, max_artifact_bytes)
            .await?;
        if bytes.len() > max_artifact_bytes
            || request
                .expected_size_bytes
                .is_some_and(|expected| bytes.len() != expected)
            || ContentHash::sha256(bytes.as_slice()) != request.expected_hash
        {
            return Err(RecoveryArtifactFailure::redacted(
                "recovery_artifact_hash_or_size_mismatch",
            ));
        }
        verified.total_bytes = verified
            .total_bytes
            .checked_add(bytes.len())
            .filter(|total| *total <= max_total_bytes)
            .ok_or_else(|| {
                RecoveryArtifactFailure::redacted("recovery_artifact_total_size_exceeded")
            })?;
        verified.artifacts.push(VerifiedRecoveryArtifact {
            kind: request.kind,
            action_key: request.action_key,
            expected_hash: request.expected_hash,
            schema_hash: request.schema_hash,
            bytes,
        });
    }
    Ok(verified)
}

fn validate_recovery_receipt(receipt: &ClaimReceipt) -> Result<(), RecoveryArtifactFailure> {
    if receipt.recovery.episodes.len() > MAX_RECOVERY_EPISODES
        || receipt.recovery.actions.len() > MAX_RECOVERY_ACTIONS
    {
        return Err(RecoveryArtifactFailure::redacted(
            "recovery_receipt_item_limit_exceeded",
        ));
    }

    let empty_action_frontier = ContentHash::sha256(b"[]");
    if (receipt.action_frontier_seq == 0 && receipt.action_frontier_hash != empty_action_frontier)
        || (receipt.action_frontier_seq > 0 && receipt.recovery.actions.is_empty())
    {
        return Err(RecoveryArtifactFailure::redacted(
            "invalid_current_action_frontier",
        ));
    }
    if let Some(state) = &receipt.recovery.state_checkpoint {
        let state_size = usize::try_from(state.state_size_bytes).map_err(|_| {
            RecoveryArtifactFailure::redacted("recovery_state_size_not_representable")
        })?;
        if !(1..=MAX_RECOVERY_STATE_BYTES).contains(&state_size)
            || state.state_artifact_ref.is_empty()
            || state.state_artifact_ref.chars().count() > 2_048
            || state.provider_checkpoint_seq > receipt.checkpoint_seq
            || state.action_frontier_seq > receipt.action_frontier_seq
            || (state.action_frontier_seq == 0
                && state.action_frontier_hash != empty_action_frontier)
            || (state.action_frontier_seq == receipt.action_frontier_seq
                && state.action_frontier_hash != receipt.action_frontier_hash)
        {
            return Err(RecoveryArtifactFailure::redacted(
                "invalid_runtime_state_checkpoint_receipt",
            ));
        }
    }

    let mut episode_hashes = BTreeSet::new();
    let mut previous_checkpoint = 0_u64;
    for episode in &receipt.recovery.episodes {
        if episode.checkpoint_seq == 0
            || previous_checkpoint.checked_add(1) != Some(episode.checkpoint_seq)
            || episode.checkpoint_seq > receipt.checkpoint_seq
            || !episode_hashes.insert(episode.episode_hash.clone())
        {
            return Err(RecoveryArtifactFailure::redacted(
                "invalid_recovery_episode_sequence",
            ));
        }
        previous_checkpoint = episode.checkpoint_seq;
    }
    if previous_checkpoint != receipt.checkpoint_seq {
        return Err(RecoveryArtifactFailure::redacted(
            "incomplete_recovery_episode_sequence",
        ));
    }

    let mut action_keys = BTreeSet::new();
    for action in &receipt.recovery.actions {
        if !action_keys.insert(action.action_key.as_str())
            || !episode_hashes.contains(&action.episode_hash)
            || !matches!(
                action.stage.as_str(),
                "begun" | "observed" | "accepted" | "rejected" | "committed" | "ambiguous"
            )
        {
            return Err(RecoveryArtifactFailure::redacted(
                "invalid_recovery_action_receipt",
            ));
        }
        match (
            action.stage.as_str(),
            action.result_hash.is_some(),
            action.result_artifact_ref.is_some(),
            action.ambiguous_reason_code.is_some(),
            action.disposition,
            action.validation_receipt_hash.is_some(),
            action.policy_receipt_hash.is_some(),
        ) {
            ("begun", false, false, false, None, false, false)
            | ("observed" | "committed", true, true, false, None, false, false)
            | (
                "accepted",
                true,
                true,
                false,
                Some(crate::ActionDisposition::Accepted),
                true,
                true,
            )
            | (
                "rejected",
                true,
                true,
                false,
                Some(crate::ActionDisposition::Rejected),
                true,
                true,
            )
            | ("ambiguous", false, false, true, None, false, false) => {}
            _ => {
                return Err(RecoveryArtifactFailure::redacted(
                    "inconsistent_recovery_action_stage",
                ));
            }
        }
    }
    Ok(())
}

async fn heartbeat_loop(
    store: Arc<dyn AgentV1Store>,
    config: RunWorkerConfig,
    run_id: String,
    tenant_id: String,
    lease: LeaseCoordinator,
    run_cancellation: CancellationToken,
    stop: CancellationToken,
) {
    loop {
        tokio::select! {
            biased;
            () = stop.cancelled() => return,
            () = sleep(config.heartbeat_interval) => {}
        }
        let Ok(mut guard) = lease.mutation_guard().await else {
            return;
        };
        let snapshot = guard.snapshot();
        let request = RenewLeaseRequest {
            mutation_id: mutation_id(
                "renew",
                &config.worker_id,
                &run_id,
                snapshot.fencing_token,
                snapshot.run_version,
            ),
            run_id: run_id.clone(),
            tenant_id: tenant_id.clone(),
            fencing_token: snapshot.fencing_token,
            expected_run_version: snapshot.run_version,
            worker_id: config.worker_id.clone(),
            lease_ms: duration_millis_u64(config.lease_duration),
        };
        let renew_started = Instant::now();
        match store.renew_lease(&request).await {
            Ok(receipt) => guard.observe_mutation(
                &receipt,
                config
                    .lease_duration
                    .saturating_sub(renew_started.elapsed()),
            ),
            Err(error) if is_fence_or_lease_loss(&error) => {
                guard.guard.lost = true;
                run_cancellation.cancel();
                warn!(
                    run_id_hash = %ContentHash::sha256(&run_id),
                    diagnostic_hash = %error_diagnostic_hash(&error),
                    "run lease was fenced"
                );
                return;
            }
            Err(error) => {
                let safety_margin = config.heartbeat_interval.max(Duration::from_millis(100));
                if Instant::now() + safety_margin >= guard.guard.local_deadline {
                    guard.guard.lost = true;
                    run_cancellation.cancel();
                    warn!(
                        run_id_hash = %ContentHash::sha256(&run_id),
                        diagnostic_hash = %error_diagnostic_hash(&error),
                        "run lease could not be proven live before its safety deadline"
                    );
                    return;
                }
                warn!(
                    run_id_hash = %ContentHash::sha256(&run_id),
                    diagnostic_hash = %error_diagnostic_hash(&error),
                    "transient lease renewal failure"
                );
            }
        }
    }
}

async fn persist_execution_failure(
    store: &dyn AgentV1Store,
    config: &RunWorkerConfig,
    receipt: &ClaimReceipt,
    lease: &LeaseCoordinator,
    failure: RunExecutionFailure,
) -> Result<(), AgentV1Error> {
    validate_identifier(&failure.reason_code, 64, "failure reason").map_err(|_| {
        AgentV1Error::InvalidRequest {
            procedure: crate::agent_v1::AgentV1Procedure::FailOrDefer,
            reason: "invalid worker failure reason",
        }
    })?;
    let mut guard = lease
        .mutation_guard()
        .await
        .map_err(|_| stale_lease_error())?;
    let snapshot = guard.snapshot();
    let disposition = if failure.retryable {
        FailureDisposition::Defer
    } else {
        FailureDisposition::Fail
    };
    let retry_delay_ms = duration_millis_u64(failure.retry_delay).min(86_400_000);
    let request = FailOrDeferRequest {
        mutation_id: mutation_id(
            if failure.retryable { "defer" } else { "fail" },
            &config.worker_id,
            &receipt.run_id,
            snapshot.fencing_token,
            snapshot.run_version,
        ),
        run_id: receipt.run_id.clone(),
        tenant_id: receipt.tenant_id.clone(),
        fencing_token: snapshot.fencing_token,
        expected_run_version: snapshot.run_version,
        disposition,
        reason_code: failure.reason_code,
        retry_delay_ms,
        release: failure.release,
    };
    let response = store.fail_or_defer(&request).await?;
    guard.observe_version(
        response.run_version,
        Some(response.cancel_generation),
        Some(response.checkpoint_seq),
    );
    guard.mark_terminal();
    Ok(())
}

#[derive(Clone, PartialEq, Eq)]
pub struct OutboxDeliveryFailure {
    pub diagnostic_hash: ContentHash,
}

impl OutboxDeliveryFailure {
    pub fn redacted(diagnostic: impl AsRef<[u8]>) -> Self {
        Self {
            diagnostic_hash: ContentHash::sha256(diagnostic),
        }
    }
}

impl fmt::Debug for OutboxDeliveryFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OutboxDeliveryFailure")
            .field("diagnostic_hash", &self.diagnostic_hash)
            .finish()
    }
}

#[async_trait]
pub trait OutboxHandler: fmt::Debug + Send + Sync {
    /// Implementations must make `dedupe_key` idempotent at the external sink.
    async fn deliver(&self, event: &OutboxEvent) -> Result<(), OutboxDeliveryFailure>;
}

#[derive(Clone, PartialEq)]
pub struct OutboxEvent {
    pub outbox_id: u64,
    pub run_id: String,
    pub event_kind: String,
    pub dedupe_key: String,
    pub payload: Value,
    pub delivery_attempts: u32,
}

impl fmt::Debug for OutboxEvent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OutboxEvent")
            .field("outbox_id", &self.outbox_id)
            .field("run_id_hash", &ContentHash::sha256(&self.run_id))
            .field("event_kind", &self.event_kind)
            .field("dedupe_key_hash", &ContentHash::sha256(&self.dedupe_key))
            .field("payload", &"[REDACTED]")
            .field("delivery_attempts", &self.delivery_attempts)
            .finish()
    }
}

impl From<&OutboxReceipt> for OutboxEvent {
    fn from(receipt: &OutboxReceipt) -> Self {
        Self {
            outbox_id: receipt.outbox_id,
            run_id: receipt.run_id.clone(),
            event_kind: receipt.event_kind.clone(),
            dedupe_key: receipt.dedupe_key.clone(),
            payload: receipt.payload.clone(),
            delivery_attempts: receipt.delivery_attempts,
        }
    }
}

#[derive(Debug)]
pub struct OutboxDispatcher {
    store: Arc<dyn AgentV1Store>,
    handler: Arc<dyn OutboxHandler>,
    config: OutboxConfig,
}

impl OutboxDispatcher {
    pub fn new(
        store: Arc<dyn AgentV1Store>,
        handler: Arc<dyn OutboxHandler>,
        config: OutboxConfig,
    ) -> Result<Self, DaemonConfigError> {
        config.validate()?;
        Ok(Self {
            store,
            handler,
            config,
        })
    }

    pub async fn run(&self, shutdown: CancellationToken) -> Result<(), SupervisorError> {
        let mut idle_attempt = 0_u32;
        let mut error_attempt = 0_u32;
        loop {
            if shutdown.is_cancelled() {
                return Ok(());
            }
            let request = ClaimOutboxRequest {
                worker_id: self.config.worker_id.clone(),
                limit: self.config.batch_size,
                lease_ms: duration_millis_u64(self.config.lease_duration),
            };
            let claim = tokio::select! {
                biased;
                () = shutdown.cancelled() => return Ok(()),
                result = self.store.claim_outbox(&request) => result,
            };
            match claim {
                Ok(response) if response.events.is_empty() => {
                    error_attempt = 0;
                    let delay = jittered_backoff(
                        self.config.idle_backoff_min,
                        self.config.idle_backoff_max,
                        idle_attempt,
                        &self.config.worker_id,
                    );
                    idle_attempt = idle_attempt.saturating_add(1);
                    tokio::select! {
                        biased;
                        () = shutdown.cancelled() => return Ok(()),
                        () = sleep(delay) => {}
                    }
                }
                Ok(response) => {
                    idle_attempt = 0;
                    error_attempt = 0;
                    // Finish a claimed batch during graceful shutdown so events
                    // are not stranded until their delivery leases expire.
                    for event in response.events {
                        self.deliver_one(&event).await;
                    }
                }
                Err(error) if is_transient_database_error(&error) => {
                    warn!(
                        diagnostic_hash = %error_diagnostic_hash(&error),
                        "transient outbox claim failure"
                    );
                    let delay = jittered_backoff(
                        self.config.error_backoff_min,
                        self.config.error_backoff_max,
                        error_attempt,
                        &self.config.worker_id,
                    );
                    error_attempt = error_attempt.saturating_add(1);
                    tokio::select! {
                        biased;
                        () = shutdown.cancelled() => return Ok(()),
                        () = sleep(delay) => {}
                    }
                }
                Err(error) => return Err(SupervisorError::Claim(error)),
            }
        }
    }

    async fn deliver_one(&self, event: &OutboxReceipt) {
        let safe_event = OutboxEvent::from(event);
        let delivered = match timeout(
            self.config.handler_timeout,
            self.handler.deliver(&safe_event),
        )
        .await
        {
            Ok(Ok(())) => Ok(()),
            Ok(Err(failure)) => Err(failure),
            Err(_) => Err(OutboxDeliveryFailure::redacted("outbox_handler_timeout")),
        };
        let request = AckOutboxRequest {
            worker_id: self.config.worker_id.clone(),
            outbox_id: event.outbox_id,
            success: delivered.is_ok(),
            error_hash: delivered
                .as_ref()
                .err()
                .map(|failure| failure.diagnostic_hash.clone()),
        };
        if let Err(error) = self.store.ack_outbox(&request).await {
            warn!(
                outbox_id = event.outbox_id,
                diagnostic_hash = %error_diagnostic_hash(&error),
                "outbox acknowledgement failed; its lease will make it recoverable"
            );
        }
    }
}

/// Runs durable claims and outbox delivery under one shutdown tree. A fatal
/// error in either service stops admission in the sibling and drains active
/// run work according to [`RunWorkerConfig`].
#[derive(Debug)]
pub struct DaemonRuntime {
    runs: RunSupervisor,
    outbox: OutboxDispatcher,
}

enum FirstServiceResult {
    Runs(Result<(), SupervisorError>),
    Outbox(Result<(), SupervisorError>),
}

impl DaemonRuntime {
    pub const fn new(runs: RunSupervisor, outbox: OutboxDispatcher) -> Self {
        Self { runs, outbox }
    }

    pub async fn run(&self, shutdown: CancellationToken) -> Result<(), SupervisorError> {
        let service_shutdown = shutdown.child_token();
        let run_future = self.runs.run(service_shutdown.clone());
        let outbox_future = self.outbox.run(service_shutdown.clone());
        tokio::pin!(run_future);
        tokio::pin!(outbox_future);
        let first = tokio::select! {
            result = &mut run_future => FirstServiceResult::Runs(result),
            result = &mut outbox_future => FirstServiceResult::Outbox(result),
        };
        service_shutdown.cancel();
        match first {
            FirstServiceResult::Runs(run_result) => {
                let outbox_result = outbox_future.await;
                run_result?;
                outbox_result
            }
            FirstServiceResult::Outbox(outbox_result) => {
                let run_result = run_future.await;
                outbox_result?;
                run_result
            }
        }
    }
}

async fn wait_for_work_or_shutdown(
    tasks: &mut JoinSet<()>,
    shutdown: &CancellationToken,
    delay: Duration,
) -> Result<(), SupervisorError> {
    if tasks.is_empty() {
        tokio::select! {
            biased;
            () = shutdown.cancelled() => {},
            () = sleep(delay) => {},
        }
    } else {
        tokio::select! {
            biased;
            () = shutdown.cancelled() => {},
            joined = tasks.join_next() => handle_joined(joined)?,
            () = sleep(delay) => {},
        }
    }
    Ok(())
}

async fn drain_tasks(
    tasks: &mut JoinSet<()>,
    forced_cancellation: &CancellationToken,
    drain_timeout: Duration,
    forced_abort_timeout: Duration,
) -> Result<(), SupervisorError> {
    let graceful = async {
        while let Some(joined) = tasks.join_next().await {
            handle_joined(Some(joined))?;
        }
        Ok::<_, SupervisorError>(())
    };
    if let Ok(result) = timeout(drain_timeout, graceful).await {
        return result;
    }
    forced_cancellation.cancel();
    let forced = async {
        while let Some(joined) = tasks.join_next().await {
            handle_joined(Some(joined))?;
        }
        Ok::<_, SupervisorError>(())
    };
    if let Ok(result) = timeout(forced_abort_timeout, forced).await {
        result
    } else {
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
        Ok(())
    }
}

fn handle_joined(
    joined: Option<Result<(), tokio::task::JoinError>>,
) -> Result<(), SupervisorError> {
    if let Some(Err(error)) = joined {
        return Err(SupervisorError::WorkerTask(ContentHash::sha256(format!(
            "{error:?}"
        ))));
    }
    Ok(())
}

fn validate_identifier(
    value: &str,
    max_chars: usize,
    name: &'static str,
) -> Result<(), DaemonConfigError> {
    if value.is_empty() || value.chars().count() > max_chars {
        return Err(DaemonConfigError::Invalid(name));
    }
    Ok(())
}

fn validate_backoff(
    minimum: Duration,
    maximum: Duration,
    name: &'static str,
) -> Result<(), DaemonConfigError> {
    if minimum.is_zero() || maximum < minimum || maximum > Duration::from_mins(1) {
        return Err(DaemonConfigError::Invalid(name));
    }
    Ok(())
}

fn duration_millis_u64(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn mutation_id(
    operation: &str,
    worker_id: &str,
    run_id: &str,
    fencing_token: u64,
    run_version: u64,
) -> String {
    let material = format!("{operation}\0{worker_id}\0{run_id}\0{fencing_token}\0{run_version}");
    format!(
        "m-{}",
        ContentHash::sha256(material)
            .as_str()
            .trim_start_matches("sha256:")
    )
}

fn jittered_backoff(
    minimum: Duration,
    maximum: Duration,
    attempt: u32,
    worker_id: &str,
) -> Duration {
    let multiplier = 1_u128 << attempt.min(20);
    let base_millis = minimum.as_millis().saturating_mul(multiplier);
    let capped_millis = base_millis.min(maximum.as_millis());
    let mut hasher = DefaultHasher::new();
    worker_id.hash(&mut hasher);
    attempt.hash(&mut hasher);
    let bucket = hasher.finish() % 501;
    // Full jitter in [75%, 125%], then re-cap to the configured ceiling.
    let permille = 750_u128 + u128::from(bucket);
    let millis = capped_millis
        .saturating_mul(permille)
        .checked_div(1_000)
        .unwrap_or(0)
        .max(1)
        .min(maximum.as_millis());
    Duration::from_millis(u64::try_from(millis).unwrap_or(u64::MAX))
}

fn is_transient_database_error(error: &AgentV1Error) -> bool {
    matches!(error, AgentV1Error::Database { .. })
}

/// Claim-path retry classification. Beyond transport-level `Database`
/// errors, `InvalidResponse` (response-shape drift after a web-side
/// migration moved a procedure before the local binary caught up) is
/// deliberately retryable: the drift self-heals when the deploy
/// completes, and staying alive preserves metrics/logs instead of the
/// 2-second launchd crash loop observed 24,535 times in the 2026-08
/// analysis. `Rejected` stays fatal — a K-code is a contract violation
/// that retrying cannot fix.
fn is_retryable_claim_error(error: &AgentV1Error) -> bool {
    matches!(error, AgentV1Error::Database { .. })
        || matches!(error, AgentV1Error::InvalidResponse { .. })
}

fn is_fence_or_lease_loss(error: &AgentV1Error) -> bool {
    matches!(
        error,
        AgentV1Error::Rejected {
            kind: RejectionKind::StaleFence
                | RejectionKind::LeaseLost
                | RejectionKind::RunVersionMismatch
                | RejectionKind::TerminalRun,
            ..
        }
    )
}

fn error_diagnostic_hash(error: &AgentV1Error) -> ContentHash {
    match error {
        AgentV1Error::Rejected {
            diagnostic_hash, ..
        }
        | AgentV1Error::Database {
            diagnostic_hash, ..
        } => diagnostic_hash.clone(),
        _ => ContentHash::sha256(format!("{error:?}")),
    }
}

fn stale_lease_error() -> AgentV1Error {
    AgentV1Error::Rejected {
        procedure: crate::agent_v1::AgentV1Procedure::FailOrDefer,
        kind: RejectionKind::LeaseLost,
        diagnostic_hash: ContentHash::sha256("local_lease_unavailable"),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tokio::sync::Notify;

    use super::*;
    use crate::agent_v1::{
        AgentV1Procedure, RecoveryActionReceipt, RecoveryEpisodeReceipt, RecoveryReceipt,
        RecoveryStateCheckpointReceipt,
    };

    #[derive(Debug, Default)]
    struct MockStore {
        claims: Mutex<VecDeque<ClaimReceipt>>,
        renew_mode: AtomicUsize,
        defer_requests: Mutex<Vec<FailOrDeferRequest>>,
        outbox: Mutex<VecDeque<OutboxReceipt>>,
        acks: Mutex<Vec<AckOutboxRequest>>,
    }

    #[async_trait]
    impl AgentV1Store for MockStore {
        async fn claim_run(
            &self,
            _request: &ClaimRunRequest,
        ) -> Result<ClaimRunResponse, AgentV1Error> {
            let receipt = self.claims.lock().await.pop_front();
            Ok(ClaimRunResponse {
                claimed: receipt.is_some(),
                receipt,
            })
        }

        async fn renew_lease(
            &self,
            request: &RenewLeaseRequest,
        ) -> Result<MutationReceipt, AgentV1Error> {
            if self.renew_mode.load(Ordering::SeqCst) == 1 {
                return Err(AgentV1Error::Rejected {
                    procedure: AgentV1Procedure::RenewLease,
                    kind: RejectionKind::StaleFence,
                    diagnostic_hash: ContentHash::sha256("stale"),
                });
            }
            Ok(MutationReceipt {
                outcome: "renewed".into(),
                run_id: request.run_id.clone(),
                fencing_token: request.fencing_token,
                run_version: request.expected_run_version + 1,
                cancel_generation: 0,
                checkpoint_seq: 0,
            })
        }

        async fn fail_or_defer(
            &self,
            request: &FailOrDeferRequest,
        ) -> Result<FailOrDeferResponse, AgentV1Error> {
            self.defer_requests.lock().await.push(request.clone());
            Ok(FailOrDeferResponse {
                outcome: "deferred".into(),
                state: "deferred".into(),
                run_id: request.run_id.clone(),
                fencing_token: request.fencing_token,
                run_version: request.expected_run_version + 1,
                cancel_generation: 0,
                checkpoint_seq: 0,
            })
        }

        async fn claim_outbox(
            &self,
            request: &ClaimOutboxRequest,
        ) -> Result<ClaimOutboxResponse, AgentV1Error> {
            let mut events = Vec::new();
            let mut outbox = self.outbox.lock().await;
            for _ in 0..request.limit {
                let Some(event) = outbox.pop_front() else {
                    break;
                };
                events.push(event);
            }
            Ok(ClaimOutboxResponse { events })
        }

        async fn ack_outbox(
            &self,
            request: &AckOutboxRequest,
        ) -> Result<AckOutboxResponse, AgentV1Error> {
            self.acks.lock().await.push(request.clone());
            Ok(AckOutboxResponse {
                outcome: "delivered".into(),
                outbox_id: request.outbox_id,
            })
        }
    }

    #[derive(Debug)]
    struct DrainExecutor {
        started: Arc<Notify>,
        completed: Arc<AtomicUsize>,
        delay: Duration,
    }

    #[async_trait]
    impl ClaimedRunExecutor for DrainExecutor {
        async fn execute(
            &self,
            _context: ClaimedRunContext,
        ) -> Result<SuccessfulRunOutcome, RunExecutionFailure> {
            self.started.notify_one();
            sleep(self.delay).await;
            self.completed.fetch_add(1, Ordering::SeqCst);
            Ok(SuccessfulRunOutcome::Committed)
        }
    }

    #[derive(Debug)]
    struct LeaseAwareExecutor {
        cancelled: Arc<Notify>,
    }

    #[async_trait]
    impl ClaimedRunExecutor for LeaseAwareExecutor {
        async fn execute(
            &self,
            context: ClaimedRunContext,
        ) -> Result<SuccessfulRunOutcome, RunExecutionFailure> {
            context.cancellation.cancelled().await;
            self.cancelled.notify_one();
            Err(RunExecutionFailure::deferred(
                "lease_lost",
                Duration::from_secs(1),
            ))
        }
    }

    #[derive(Debug, Default)]
    struct RecordingOutboxHandler {
        delivered: AtomicUsize,
    }

    #[async_trait]
    impl OutboxHandler for RecordingOutboxHandler {
        async fn deliver(&self, _event: &OutboxEvent) -> Result<(), OutboxDeliveryFailure> {
            self.delivered.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[derive(Debug, Default)]
    struct FixtureArtifactStore;

    #[async_trait]
    impl RecoveryArtifactStore for FixtureArtifactStore {
        async fn load_verified(
            &self,
            _request: &RecoveryArtifactRequest,
            _max_bytes: usize,
        ) -> Result<Zeroizing<Vec<u8>>, RecoveryArtifactFailure> {
            Ok(Zeroizing::new(Vec::new()))
        }
    }

    #[derive(Debug, Default)]
    struct WrongArtifactStore;

    #[async_trait]
    impl RecoveryArtifactStore for WrongArtifactStore {
        async fn load_verified(
            &self,
            _request: &RecoveryArtifactRequest,
            _max_bytes: usize,
        ) -> Result<Zeroizing<Vec<u8>>, RecoveryArtifactFailure> {
            Ok(Zeroizing::new(b"wrong bytes".to_vec()))
        }
    }

    #[derive(Debug)]
    struct StateArtifactStore {
        bytes: Vec<u8>,
        requests: Mutex<Vec<RecoveryArtifactRequest>>,
    }

    #[async_trait]
    impl RecoveryArtifactStore for StateArtifactStore {
        async fn load_verified(
            &self,
            request: &RecoveryArtifactRequest,
            _max_bytes: usize,
        ) -> Result<Zeroizing<Vec<u8>>, RecoveryArtifactFailure> {
            self.requests.lock().await.push(request.clone());
            Ok(Zeroizing::new(self.bytes.clone()))
        }
    }

    fn claim(run_id: &str) -> ClaimReceipt {
        ClaimReceipt {
            run_id: run_id.into(),
            tenant_id: "tenant-a".into(),
            principal_id: "principal-a".into(),
            session_id: "session-a".into(),
            fencing_token: 1,
            run_version: 1,
            cancel_generation: 0,
            checkpoint_seq: 0,
            action_frontier_seq: 0,
            action_frontier_hash: ContentHash::sha256(b"[]"),
            lease_deadline: "fixture".into(),
            agent_image_hash: ContentHash::sha256("image"),
            runtime_version: "0.1.0".into(),
            priority: 0,
            immutable_snapshot_hash: ContentHash::sha256("snapshot"),
            immutable_snapshot: json!({}),
            resource_profile: json!({}),
            budgets: json!({}),
            reclaimed: false,
            recovery: RecoveryReceipt {
                state_checkpoint: None,
                episodes: Vec::new(),
                actions: Vec::new(),
            },
        }
    }

    fn run_config() -> RunWorkerConfig {
        RunWorkerConfig {
            worker_id: "worker-a".into(),
            runtime_version: "0.1.0".into(),
            accepted_agent_image_hashes: vec![ContentHash::sha256("image")],
            max_in_flight: 1,
            lease_duration: Duration::from_secs(1),
            heartbeat_interval: Duration::from_millis(50),
            idle_backoff_min: Duration::from_millis(10),
            idle_backoff_max: Duration::from_millis(20),
            error_backoff_min: Duration::from_millis(10),
            error_backoff_max: Duration::from_millis(20),
            recovery_preflight_timeout: Duration::from_millis(500),
            max_recovery_artifact_bytes: 1024 * 1024,
            max_recovery_total_bytes: 4 * 1024 * 1024,
            drain_timeout: Duration::from_millis(500),
            forced_abort_timeout: Duration::from_millis(100),
        }
    }

    #[tokio::test]
    async fn graceful_shutdown_stops_claiming_and_drains_active_run() {
        let store = Arc::new(MockStore::default());
        store
            .claims
            .lock()
            .await
            .extend([claim("run-1"), claim("run-2")]);
        let started = Arc::new(Notify::new());
        let completed = Arc::new(AtomicUsize::new(0));
        let executor = Arc::new(DrainExecutor {
            started: Arc::clone(&started),
            completed: Arc::clone(&completed),
            delay: Duration::from_millis(60),
        });
        let supervisor = Arc::new(
            RunSupervisor::new(
                store.clone(),
                executor,
                Arc::new(FixtureArtifactStore),
                run_config(),
            )
            .unwrap(),
        );
        let shutdown = CancellationToken::new();
        let task = tokio::spawn({
            let supervisor = Arc::clone(&supervisor);
            let shutdown = shutdown.clone();
            async move { supervisor.run(shutdown).await }
        });
        started.notified().await;
        shutdown.cancel();
        task.await.unwrap().unwrap();
        assert_eq!(completed.load(Ordering::SeqCst), 1);
        assert_eq!(store.claims.lock().await.len(), 1);
    }

    #[tokio::test]
    async fn stale_lease_cancels_executor_without_writing_with_old_fence() {
        let store = Arc::new(MockStore::default());
        store.renew_mode.store(1, Ordering::SeqCst);
        let cancelled = Arc::new(Notify::new());
        let executor = Arc::new(LeaseAwareExecutor {
            cancelled: Arc::clone(&cancelled),
        });
        drive_claim(
            store.clone(),
            executor,
            Arc::new(FixtureArtifactStore),
            run_config(),
            claim("run-stale"),
            Duration::ZERO,
            CancellationToken::new(),
        )
        .await;
        timeout(Duration::from_millis(50), cancelled.notified())
            .await
            .unwrap();
        assert!(store.defer_requests.lock().await.is_empty());
    }

    #[tokio::test]
    async fn reclaimed_run_does_not_execute_with_unverified_artifacts() {
        let store = Arc::new(MockStore::default());
        let started = Arc::new(Notify::new());
        let completed = Arc::new(AtomicUsize::new(0));
        let executor = Arc::new(DrainExecutor {
            started,
            completed: Arc::clone(&completed),
            delay: Duration::ZERO,
        });
        let mut receipt = claim("run-recovery");
        receipt.reclaimed = true;
        receipt.recovery.episodes.push(RecoveryEpisodeReceipt {
            episode_hash: ContentHash::sha256("expected bytes"),
            checkpoint_seq: 1,
            episode_artifact_ref: "cas://episode".into(),
        });
        drive_claim(
            store.clone(),
            executor,
            Arc::new(WrongArtifactStore),
            run_config(),
            receipt,
            Duration::ZERO,
            CancellationToken::new(),
        )
        .await;
        assert_eq!(completed.load(Ordering::SeqCst), 0);
        let requests = store.defer_requests.lock().await;
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].reason_code, "recovery_artifact_unavailable");
    }

    #[test]
    fn recovery_receipt_requires_complete_ordered_episode_history() {
        let mut receipt = claim("run-history");
        receipt.checkpoint_seq = 2;
        receipt.recovery.episodes = vec![
            RecoveryEpisodeReceipt {
                episode_hash: ContentHash::sha256("episode-1"),
                checkpoint_seq: 1,
                episode_artifact_ref: "cas://episode-1".into(),
            },
            RecoveryEpisodeReceipt {
                episode_hash: ContentHash::sha256("episode-2"),
                checkpoint_seq: 2,
                episode_artifact_ref: "cas://episode-2".into(),
            },
        ];
        assert!(validate_recovery_receipt(&receipt).is_ok());

        receipt.recovery.episodes.remove(0);
        assert!(validate_recovery_receipt(&receipt).is_err());
    }

    #[test]
    fn accepted_and_rejected_recovery_bind_result_and_finalization_receipts() {
        let mut receipt = claim("run-finalized-action");
        let episode_hash = ContentHash::sha256("episode");
        receipt.checkpoint_seq = 1;
        receipt.recovery.episodes.push(RecoveryEpisodeReceipt {
            episode_hash: episode_hash.clone(),
            checkpoint_seq: 1,
            episode_artifact_ref: "cas://episode".into(),
        });
        receipt.recovery.actions.push(RecoveryActionReceipt {
            action_key: "action-1".into(),
            request_hash: ContentHash::sha256("request"),
            episode_hash,
            tool_call_id: "call-1".into(),
            capability_id: "ontology.query_context".into(),
            input_schema_hash: ContentHash::sha256("input"),
            output_schema_hash: ContentHash::sha256("output"),
            data_release_hash: ContentHash::sha256("release"),
            arguments_artifact_ref: "cas://arguments".into(),
            retryable_read: true,
            stage: "rejected".into(),
            result_hash: Some(ContentHash::sha256("raw-result")),
            result_artifact_ref: Some("cas://raw-result".into()),
            ambiguous_reason_code: None,
            disposition: Some(crate::ActionDisposition::Rejected),
            validation_receipt_hash: Some(ContentHash::sha256("validation")),
            policy_receipt_hash: Some(ContentHash::sha256("policy")),
        });
        assert!(validate_recovery_receipt(&receipt).is_ok());

        receipt.recovery.actions[0].policy_receipt_hash = None;
        assert!(validate_recovery_receipt(&receipt).is_err());
    }

    #[tokio::test]
    async fn runtime_state_preflight_binds_scope_hash_schema_and_exact_size() {
        let bytes = br#"{"schema":"krw-agent-recovery-state/v1"}"#.to_vec();
        let mut receipt = claim("run-state");
        receipt.recovery.state_checkpoint = Some(RecoveryStateCheckpointReceipt {
            state_hash: ContentHash::sha256(&bytes),
            state_artifact_ref: "cas://state".into(),
            state_size_bytes: bytes.len() as u64,
            recovery_schema_hash: ContentHash::sha256("krw-agent-recovery-state/v1"),
            provider_checkpoint_seq: 0,
            action_frontier_seq: 0,
            action_frontier_hash: ContentHash::sha256(b"[]"),
        });
        let store = StateArtifactStore {
            bytes: bytes.clone(),
            requests: Mutex::new(Vec::new()),
        };

        let verified = load_recovery_artifacts(&receipt, &store, 1024, 4096)
            .await
            .unwrap();
        assert_eq!(verified.artifacts().len(), 1);
        assert_eq!(
            verified.artifacts()[0].kind,
            RecoveryArtifactKind::RuntimeState
        );
        assert_eq!(verified.artifacts()[0].bytes(), bytes);
        assert_eq!(
            verified.artifacts()[0].schema_hash,
            receipt
                .recovery
                .state_checkpoint
                .as_ref()
                .map(|state| state.recovery_schema_hash.clone())
        );
        let requests = store.requests.lock().await;
        assert_eq!(requests[0].tenant_id, "tenant-a");
        assert_eq!(requests[0].principal_id, "principal-a");
        assert_eq!(requests[0].run_id, "run-state");
        assert_eq!(requests[0].expected_size_bytes, Some(bytes.len()));
    }

    #[tokio::test]
    async fn runtime_state_preflight_rejects_declared_size_mismatch() {
        let bytes = b"state".to_vec();
        let mut receipt = claim("run-state-size");
        receipt.recovery.state_checkpoint = Some(RecoveryStateCheckpointReceipt {
            state_hash: ContentHash::sha256(&bytes),
            state_artifact_ref: "cas://state".into(),
            state_size_bytes: (bytes.len() + 1) as u64,
            recovery_schema_hash: ContentHash::sha256("krw-agent-recovery-state/v1"),
            provider_checkpoint_seq: 0,
            action_frontier_seq: 0,
            action_frontier_hash: ContentHash::sha256(b"[]"),
        });
        let store = StateArtifactStore {
            bytes,
            requests: Mutex::new(Vec::new()),
        };
        assert!(
            load_recovery_artifacts(&receipt, &store, 1024, 4096)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn lease_mutation_lane_tracks_action_and_runtime_state_frontiers() {
        let receipt = claim("run-frontier");
        let lease = LeaseCoordinator::from_claim(&receipt, Duration::from_secs(30), Duration::ZERO);
        let next_frontier = ContentHash::sha256("frontier-1");
        let mut guard = lease.mutation_guard().await.unwrap();
        guard.observe_action(&ActionAbiReceipt {
            outcome: "begun".into(),
            run_id: receipt.run_id.clone(),
            fencing_token: receipt.fencing_token,
            run_version: 2,
            action_key: "action-1".into(),
            stage: "begun".into(),
            request_hash: ContentHash::sha256("request"),
            result_hash: None,
            retryable_read: true,
            disposition: None,
            validation_receipt_hash: None,
            policy_receipt_hash: None,
            action_frontier_seq: 1,
            action_frontier_hash: next_frontier.clone(),
        });
        assert_eq!(guard.snapshot().action_frontier_seq, 1);
        assert_eq!(guard.snapshot().action_frontier_hash, next_frontier);

        guard.observe_run_state(&CheckpointRunStateResponse {
            outcome: "checkpointed".into(),
            run_id: receipt.run_id,
            fencing_token: receipt.fencing_token,
            run_version: 3,
            provider_checkpoint_seq: 1,
            action_frontier_seq: 1,
            action_frontier_hash: ContentHash::sha256("frontier-1"),
            recovery_schema_hash: ContentHash::sha256("recovery-state/v1"),
            state_hash: ContentHash::sha256("state"),
            state_size_bytes: 5,
        });
        let snapshot = guard.snapshot();
        assert_eq!(snapshot.run_version, 3);
        assert_eq!(snapshot.checkpoint_seq, 1);
        assert_eq!(snapshot.action_frontier_seq, 1);
    }

    #[tokio::test]
    async fn outbox_handler_is_acked_only_after_delivery() {
        let store = Arc::new(MockStore::default());
        store.outbox.lock().await.push_back(OutboxReceipt {
            outbox_id: 1,
            run_id: "run-1".into(),
            event_kind: "presentation".into(),
            dedupe_key: "run-1:presentation".into(),
            payload: json!({"safe": true}),
            delivery_attempts: 1,
            delivery_deadline: "fixture".into(),
        });
        let handler = Arc::new(RecordingOutboxHandler::default());
        let config = OutboxConfig {
            worker_id: "outbox-a".into(),
            batch_size: 4,
            lease_duration: Duration::from_secs(1),
            handler_timeout: Duration::from_millis(500),
            idle_backoff_min: Duration::from_millis(10),
            idle_backoff_max: Duration::from_millis(20),
            error_backoff_min: Duration::from_millis(10),
            error_backoff_max: Duration::from_millis(20),
        };
        let dispatcher =
            Arc::new(OutboxDispatcher::new(store.clone(), handler.clone(), config).unwrap());
        let shutdown = CancellationToken::new();
        let task = tokio::spawn({
            let dispatcher = Arc::clone(&dispatcher);
            let shutdown = shutdown.clone();
            async move { dispatcher.run(shutdown).await }
        });
        timeout(Duration::from_secs(1), async {
            loop {
                if !store.acks.lock().await.is_empty() {
                    break;
                }
                sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        shutdown.cancel();
        task.await.unwrap().unwrap();
        assert_eq!(handler.delivered.load(Ordering::SeqCst), 1);
        let acks = store.acks.lock().await;
        assert_eq!(acks.len(), 1);
        assert!(acks[0].success);
    }

    #[test]
    fn mutation_ids_are_bounded_and_stable_for_retries() {
        let first = mutation_id("renew", "worker", "run", 9, 12);
        let second = mutation_id("renew", "worker", "run", 9, 12);
        assert_eq!(first, second);
        assert!(first.len() <= 128);
        assert_ne!(first, mutation_id("renew", "worker", "run", 10, 12));
    }

    #[test]
    fn claim_errors_from_response_shape_drift_are_retryable() {
        let database = AgentV1Error::Database {
            procedure: AgentV1Procedure::ClaimRun,
            diagnostic_hash: ContentHash::sha256("connection reset"),
        };
        assert!(is_retryable_claim_error(&database));

        let invalid_response = AgentV1Error::InvalidResponse {
            procedure: AgentV1Procedure::ClaimRun,
            reason: "unknown field `mcp_ready`".to_owned(),
        };
        assert!(
            is_retryable_claim_error(&invalid_response),
            "response-shape drift self-heals when the deploy completes; \
             the daemon must stay alive and keep retrying"
        );

        let rejected = AgentV1Error::Rejected {
            procedure: AgentV1Procedure::ClaimRun,
            kind: RejectionKind::InvalidRequest,
            diagnostic_hash: ContentHash::sha256("invalid request"),
        };
        assert!(!is_retryable_claim_error(&rejected));
    }

    #[test]
    fn outbox_debug_redacts_payload_and_identifiers() {
        let event = OutboxEvent {
            outbox_id: 1,
            run_id: "sensitive-run".into(),
            event_kind: "presentation".into(),
            dedupe_key: "sensitive-dedupe".into(),
            payload: json!({"secret": "must-not-log"}),
            delivery_attempts: 1,
        };
        let debug = format!("{event:?}");
        assert!(!debug.contains("sensitive-run"));
        assert!(!debug.contains("sensitive-dedupe"));
        assert!(!debug.contains("must-not-log"));
    }
}
