//! Versioned Postgres procedure ABI.
//!
//! The daemon is intentionally unable to submit arbitrary SQL through this
//! adapter. A database integration supplies [`JsonProcedureExecutor`], while
//! this module owns the finite procedure inventory, request hashing, and
//! response decoding.

use std::collections::BTreeMap;
use std::fmt;

use async_trait::async_trait;
use krw_agent_protocol::ContentHash;
use krw_session_memory::{
    MAX_SESSION_MEMORY_SNAPSHOT_BYTES, SessionMemoryDeltaV3, SessionMemorySnapshotV3,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use thiserror::Error;

use crate::ActionDisposition;
use krw_agent_bounded_child::{ChildExecutionReceipt, ChildStage};

pub const ABI_VERSION: u16 = 1;
const MAX_MUTATION_REQUEST_BYTES: usize = 16 * 1024 * 1024;
pub const INITIAL_MIGRATION_SQL: &str = include_str!("../../../migrations/0001_agent_v1.sql");
pub const ACTION_FINALIZATION_MIGRATION_SQL: &str =
    include_str!("../../../migrations/0002_action_finalization.sql");
pub const BOUNDED_CHILD_MIGRATION_SQL: &str =
    include_str!("../../../migrations/0003_bounded_child.sql");
pub const SESSION_MEMORY_MIGRATION_SQL: &str =
    include_str!("../../../migrations/0004_session_memory.sql");
pub const SESSION_MEMORY_SNAPSHOT_MIGRATION_SQL: &str =
    include_str!("../../../migrations/0005_session_memory_snapshot.sql");
pub const FINAL_OUTPUT_READ_MIGRATION_SQL: &str =
    include_str!("../../../migrations/0006_read_final_output.sql");
pub const SESSION_MEMORY_RETENTION_MIGRATION_SQL: &str =
    include_str!("../../../migrations/0016_session_memory_source_retention.sql");
pub const DAEMON_HEARTBEAT_MIGRATION_SQL: &str =
    include_str!("../../../migrations/0018_daemon_heartbeat.sql");
pub const DAEMON_MCP_READINESS_MIGRATION_SQL: &str =
    include_str!("../../../migrations/0022_daemon_mcp_readiness.sql");
pub const LIFECYCLE_OUTBOX_MIGRATION_SQL: &str =
    include_str!("../../../migrations/0019_lifecycle_outbox.sql");

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AgentV1Procedure {
    EnqueueRun,
    ClaimRun,
    RenewLease,
    CheckpointEpisode,
    CheckpointRunState,
    BeginAction,
    ObserveAction,
    FinalizeAction,
    MarkActionAmbiguous,
    CheckpointChildExecution,
    ReadChildExecution,
    ReadSessionMemory,
    CheckpointSessionMemorySnapshot,
    CommitFinal,
    RequestCancel,
    FailOrDefer,
    ClaimOutbox,
    AckOutbox,
    ReadCommittedOutcome,
    ReadFinalOutput,
    ReapRetainedRuns,
    RetireSessionMemory,
    HeartbeatDaemon,
}

impl AgentV1Procedure {
    pub const fn name(self) -> &'static str {
        match self {
            Self::EnqueueRun => "agent_v1.enqueue_run",
            Self::ClaimRun => "agent_v1.claim_run",
            Self::RenewLease => "agent_v1.renew_lease",
            Self::CheckpointEpisode => "agent_v1.checkpoint_episode",
            Self::CheckpointRunState => "agent_v1.checkpoint_run_state",
            Self::BeginAction => "agent_v1.begin_action",
            Self::ObserveAction => "agent_v1.observe_action",
            Self::FinalizeAction => "agent_v1.finalize_action",
            Self::MarkActionAmbiguous => "agent_v1.mark_action_ambiguous",
            Self::CheckpointChildExecution => "agent_v1.checkpoint_child_execution",
            Self::ReadChildExecution => "agent_v1.read_child_execution",
            Self::ReadSessionMemory => "agent_v1.read_session_memory",
            Self::CheckpointSessionMemorySnapshot => "agent_v1.checkpoint_session_memory_snapshot",
            Self::CommitFinal => "agent_v1.commit_final",
            Self::RequestCancel => "agent_v1.request_cancel",
            Self::FailOrDefer => "agent_v1.fail_or_defer",
            Self::ClaimOutbox => "agent_v1.claim_outbox",
            Self::AckOutbox => "agent_v1.ack_outbox",
            Self::ReadCommittedOutcome => "agent_v1.read_committed_outcome",
            Self::ReadFinalOutput => "agent_v1.read_final_output",
            Self::ReapRetainedRuns => "agent_v1.reap_retained_runs",
            Self::RetireSessionMemory => "agent_v1.retire_session_memory",
            Self::HeartbeatDaemon => "agent_v1.heartbeat_daemon",
        }
    }

    /// A fixed statement inventory prevents product/table SQL from entering
    /// the daemon persistence adapter.
    pub const fn statement(self) -> &'static str {
        match self {
            Self::EnqueueRun => "SELECT agent_v1.enqueue_run($1::jsonb)",
            Self::ClaimRun => "SELECT agent_v1.claim_run($1::jsonb)",
            Self::RenewLease => "SELECT agent_v1.renew_lease($1::jsonb)",
            Self::CheckpointEpisode => "SELECT agent_v1.checkpoint_episode($1::jsonb)",
            Self::CheckpointRunState => "SELECT agent_v1.checkpoint_run_state($1::jsonb)",
            Self::BeginAction => "SELECT agent_v1.begin_action($1::jsonb)",
            Self::ObserveAction => "SELECT agent_v1.observe_action($1::jsonb)",
            Self::FinalizeAction => "SELECT agent_v1.finalize_action($1::jsonb)",
            Self::MarkActionAmbiguous => "SELECT agent_v1.mark_action_ambiguous($1::jsonb)",
            Self::CheckpointChildExecution => {
                "SELECT agent_v1.checkpoint_child_execution($1::jsonb)"
            }
            Self::ReadChildExecution => "SELECT agent_v1.read_child_execution($1::jsonb)",
            Self::ReadSessionMemory => "SELECT agent_v1.read_session_memory($1::jsonb)",
            Self::CheckpointSessionMemorySnapshot => {
                "SELECT agent_v1.checkpoint_session_memory_snapshot($1::jsonb)"
            }
            Self::CommitFinal => "SELECT agent_v1.commit_final($1::jsonb)",
            Self::RequestCancel => "SELECT agent_v1.request_cancel($1::jsonb)",
            Self::FailOrDefer => "SELECT agent_v1.fail_or_defer($1::jsonb)",
            Self::ClaimOutbox => "SELECT agent_v1.claim_outbox($1::jsonb)",
            Self::AckOutbox => "SELECT agent_v1.ack_outbox($1::jsonb)",
            Self::ReadCommittedOutcome => "SELECT agent_v1.read_committed_outcome($1::jsonb)",
            Self::ReadFinalOutput => "SELECT agent_v1.read_final_output($1::jsonb)",
            Self::ReapRetainedRuns => "SELECT agent_v1.reap_retained_runs($1::jsonb)",
            Self::RetireSessionMemory => "SELECT agent_v1.retire_session_memory($1::jsonb)",
            Self::HeartbeatDaemon => "SELECT agent_v1.heartbeat_daemon($1::jsonb)",
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct DatabaseFailure {
    pub sqlstate: Option<String>,
    pub diagnostic_hash: ContentHash,
}

impl DatabaseFailure {
    pub fn redacted(sqlstate: Option<String>, diagnostic: impl AsRef<[u8]>) -> Self {
        Self {
            sqlstate,
            diagnostic_hash: ContentHash::sha256(diagnostic),
        }
    }
}

impl fmt::Debug for DatabaseFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DatabaseFailure")
            .field("sqlstate", &self.sqlstate)
            .field("diagnostic_hash", &self.diagnostic_hash)
            .finish()
    }
}

#[async_trait]
pub trait JsonProcedureExecutor: fmt::Debug + Send + Sync {
    /// Execute exactly the supplied inventory entry and return its single
    /// JSON result column. Implementations should prepare/cache
    /// `procedure.statement()` and never interpolate identifiers.
    async fn execute_json(
        &self,
        procedure: AgentV1Procedure,
        request: Value,
    ) -> Result<Value, DatabaseFailure>;
}

#[derive(Debug)]
pub struct AgentV1Client<E> {
    executor: E,
}

impl<E> AgentV1Client<E>
where
    E: JsonProcedureExecutor,
{
    pub const fn new(executor: E) -> Self {
        Self { executor }
    }

    pub fn executor(&self) -> &E {
        &self.executor
    }

    pub async fn execute<R>(&self, request: &R) -> Result<R::Response, AgentV1Error>
    where
        R: AgentV1Request,
    {
        let procedure = R::PROCEDURE;
        let encoded = encode_request(procedure, request)?;
        let response = self
            .executor
            .execute_json(procedure, encoded)
            .await
            .map_err(|failure| AgentV1Error::from_database_for(procedure, failure))?;
        serde_json::from_value(response).map_err(|error| AgentV1Error::InvalidResponse {
            procedure,
            reason: error.to_string(),
        })
    }
}

mod sealed {
    pub trait Sealed {}
}

pub trait AgentV1Request: sealed::Sealed + fmt::Debug + Serialize + Sync {
    type Response: fmt::Debug + DeserializeOwned;
    const PROCEDURE: AgentV1Procedure;
}

fn encode_request<R>(procedure: AgentV1Procedure, request: &R) -> Result<Value, AgentV1Error>
where
    R: Serialize,
{
    let Value::Object(mut object) = serde_json::to_value(request)? else {
        return Err(AgentV1Error::RequestMustBeObject(procedure));
    };
    object.insert("abi_version".into(), Value::from(ABI_VERSION));
    validate_request_object(procedure, &object)?;

    if object.contains_key("mutation_id") {
        #[derive(Serialize)]
        struct MutationHashInput<'a> {
            procedure: &'a str,
            request: &'a Map<String, Value>,
        }
        let hash_input = MutationHashInput {
            procedure: procedure.name(),
            request: &object,
        };
        let canonical_request = serde_jcs::to_vec(&hash_input)?;
        if canonical_request.len() > MAX_MUTATION_REQUEST_BYTES {
            return Err(AgentV1Error::InvalidRequest {
                procedure,
                reason: "mutation request byte limit exceeded",
            });
        }
        let mutation_hash = ContentHash::sha256(canonical_request);
        object.insert(
            "mutation_hash".into(),
            Value::String(mutation_hash.to_string()),
        );
        return Ok(Value::Object(object));
    }

    Ok(Value::Object(object))
}

fn validate_request_object(
    procedure: AgentV1Procedure,
    object: &Map<String, Value>,
) -> Result<(), AgentV1Error> {
    for (key, max_length) in [
        ("run_id", 128),
        ("tenant_id", 128),
        ("principal_id", 128),
        ("session_id", 128),
        ("mutation_id", 128),
        ("worker_id", 128),
        ("daemon_id", 128),
        ("provider", 16),
        ("action_key", 128),
        ("child_id", 160),
        ("tool_call_id", 128),
        ("capability_id", 128),
        ("runtime_version", 64),
        ("reason_code", 64),
        ("disposition", 16),
        ("episode_artifact_ref", 2048),
        ("state_artifact_ref", 2048),
        ("arguments_artifact_ref", 2048),
        ("result_artifact_ref", 2048),
        ("lifecycle_stage", 32),
    ] {
        if let Some(value) = object.get(key) {
            let Some(value) = value.as_str() else {
                return Err(AgentV1Error::InvalidRequest {
                    procedure,
                    reason: "bounded identifier must be a string",
                });
            };
            if value.is_empty() || value.chars().count() > max_length {
                return Err(AgentV1Error::InvalidRequest {
                    procedure,
                    reason: "bounded identifier length exceeded",
                });
            }
        }
    }
    for key in [
        "fencing_token",
        "expected_run_version",
        "expected_cancel_generation",
        "expected_checkpoint_seq",
        "provider_checkpoint_seq",
        "action_frontier_seq",
        "state_size_bytes",
        "lease_ms",
        "retry_delay_ms",
        "limit",
        "after_revision",
        "snapshot_revision",
        "snapshot_size_bytes",
        "outbox_id",
        "heartbeat_ttl_ms",
    ] {
        if object
            .get(key)
            .and_then(Value::as_u64)
            .is_some_and(|value| value > i64::MAX as u64)
        {
            return Err(AgentV1Error::InvalidRequest {
                procedure,
                reason: "integer exceeds PostgreSQL bigint",
            });
        }
    }
    if let Some(images) = object.get("accepted_agent_image_hashes") {
        let Some(images) = images.as_array() else {
            return Err(AgentV1Error::InvalidRequest {
                procedure,
                reason: "accepted image hashes must be an array",
            });
        };
        if images.is_empty() || images.len() > 64 {
            return Err(AgentV1Error::InvalidRequest {
                procedure,
                reason: "accepted image hash count is outside bounds",
            });
        }
    }
    if object
        .get("outbox_payloads")
        .and_then(Value::as_object)
        .is_some_and(|payloads| payloads.len() > 3)
    {
        return Err(AgentV1Error::InvalidRequest {
            procedure,
            reason: "outbox payload count exceeded",
        });
    }
    if let Some(payloads) = object.get("outbox_payloads").and_then(Value::as_object)
        && payloads
            .keys()
            .any(|key| !matches!(key.as_str(), "presentation" | "billing" | "notification"))
    {
        return Err(AgentV1Error::InvalidRequest {
            procedure,
            reason: "unknown outbox payload kind",
        });
    }
    if let Some(lease_ms) = object.get("lease_ms").and_then(Value::as_u64)
        && !(1_000..=600_000).contains(&lease_ms)
    {
        return Err(AgentV1Error::InvalidRequest {
            procedure,
            reason: "lease duration is outside bounds",
        });
    }
    if procedure == AgentV1Procedure::HeartbeatDaemon {
        let valid_provider = matches!(
            object.get("provider").and_then(Value::as_str),
            Some("glm" | "deepseek")
        );
        let valid_ttl = matches!(
            object.get("heartbeat_ttl_ms").and_then(Value::as_u64),
            Some(30_000..=120_000)
        );
        let valid_mcp_ready = object.get("mcp_ready").and_then(Value::as_bool).is_some();
        if !valid_provider || !valid_ttl || !valid_mcp_ready {
            return Err(AgentV1Error::InvalidRequest {
                procedure,
                reason: "daemon heartbeat contract is invalid",
            });
        }
    }
    if let Some(limit) = object.get("limit").and_then(Value::as_u64)
        && !(1..=100).contains(&limit)
    {
        return Err(AgentV1Error::InvalidRequest {
            procedure,
            reason: "batch limit is outside bounds",
        });
    }
    if procedure == AgentV1Procedure::ReadSessionMemory
        && object
            .get("limit")
            .and_then(Value::as_u64)
            .is_none_or(|limit| !(1..=8).contains(&limit))
    {
        return Err(AgentV1Error::InvalidRequest {
            procedure,
            reason: "session memory page limit is outside bounds",
        });
    }
    if procedure == AgentV1Procedure::ReadSessionMemory
        && object
            .get("mode")
            .and_then(Value::as_str)
            .is_none_or(|mode| !matches!(mode, "snapshot_tail" | "audit_rebuild"))
    {
        return Err(AgentV1Error::InvalidRequest {
            procedure,
            reason: "session memory read mode is invalid",
        });
    }
    if object
        .get("retry_delay_ms")
        .and_then(Value::as_u64)
        .is_some_and(|delay| delay > 86_400_000)
    {
        return Err(AgentV1Error::InvalidRequest {
            procedure,
            reason: "retry delay is outside bounds",
        });
    }
    if object
        .get("state_size_bytes")
        .and_then(Value::as_u64)
        .is_some_and(|size| !(1..=8 * 1024 * 1024).contains(&size))
    {
        return Err(AgentV1Error::InvalidRequest {
            procedure,
            reason: "runtime state artifact size is outside bounds",
        });
    }
    if procedure == AgentV1Procedure::CheckpointRunState
        && object
            .get("lifecycle_stage")
            .is_some_and(|stage| stage.as_str() != Some("composing"))
    {
        return Err(AgentV1Error::InvalidRequest {
            procedure,
            reason: "runtime lifecycle stage is invalid",
        });
    }
    if procedure == AgentV1Procedure::AckOutbox {
        let success = object.get("success").and_then(Value::as_bool);
        let has_error = object.get("error_hash").is_some();
        if success.is_none() || success == Some(has_error) {
            return Err(AgentV1Error::InvalidRequest {
                procedure,
                reason: "outbox acknowledgement error contract is invalid",
            });
        }
    }
    if procedure == AgentV1Procedure::CheckpointChildExecution {
        let receipt_value = object.get("receipt").ok_or(AgentV1Error::InvalidRequest {
            procedure,
            reason: "child receipt is required",
        })?;
        let receipt_bytes = serde_jcs::to_vec(receipt_value)?;
        if receipt_bytes.len() > 1024 * 1024 {
            return Err(AgentV1Error::InvalidRequest {
                procedure,
                reason: "child receipt byte limit exceeded",
            });
        }
        let receipt: ChildExecutionReceipt = serde_json::from_value(receipt_value.clone())
            .map_err(|_| AgentV1Error::InvalidRequest {
                procedure,
                reason: "child receipt shape is invalid",
            })?;
        receipt
            .validate()
            .map_err(|_| AgentV1Error::InvalidRequest {
                procedure,
                reason: "child receipt contract is invalid",
            })?;
        let declared_hash = object.get("receipt_hash").and_then(Value::as_str).ok_or(
            AgentV1Error::InvalidRequest {
                procedure,
                reason: "child receipt hash is required",
            },
        )?;
        let target_stage = object.get("target_stage").and_then(Value::as_str).ok_or(
            AgentV1Error::InvalidRequest {
                procedure,
                reason: "child target stage is required",
            },
        )?;
        let expected_stage = match receipt.stage {
            ChildStage::Reserved => "reserved",
            ChildStage::Invoked => "invoked",
            ChildStage::Completed => "completed",
            ChildStage::Cancelled => "cancelled",
        };
        if receipt
            .receipt_hash()
            .map_or(true, |hash| hash.as_str() != declared_hash)
            || target_stage != expected_stage
            || object.get("run_id").and_then(Value::as_str) != Some(receipt.run_id.as_str())
            || object.get("child_id").and_then(Value::as_str) != Some(receipt.child_id.as_str())
            || object.get("fencing_token").and_then(Value::as_u64) != Some(receipt.fencing_token)
            || object
                .get("expected_cancel_generation")
                .and_then(Value::as_u64)
                != Some(receipt.cancel_generation)
        {
            return Err(AgentV1Error::InvalidRequest {
                procedure,
                reason: "child receipt hash or identity mismatch",
            });
        }
    }
    if procedure == AgentV1Procedure::CommitFinal {
        validate_final_memory_contract(procedure, object)?;
    }
    if procedure == AgentV1Procedure::CheckpointSessionMemorySnapshot {
        validate_session_memory_snapshot_contract(procedure, object)?;
    }
    Ok(())
}

fn validate_session_memory_snapshot_contract(
    procedure: AgentV1Procedure,
    object: &Map<String, Value>,
) -> Result<(), AgentV1Error> {
    let snapshot_value = object.get("snapshot").ok_or(AgentV1Error::InvalidRequest {
        procedure,
        reason: "session memory snapshot is required",
    })?;
    let canonical = serde_jcs::to_vec(snapshot_value)?;
    let declared_size = object
        .get("snapshot_size_bytes")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or(AgentV1Error::InvalidRequest {
            procedure,
            reason: "session memory snapshot size is invalid",
        })?;
    if canonical.is_empty()
        || canonical.len() > MAX_SESSION_MEMORY_SNAPSHOT_BYTES
        || declared_size != canonical.len()
    {
        return Err(AgentV1Error::InvalidRequest {
            procedure,
            reason: "session memory snapshot byte contract is invalid",
        });
    }
    let declared_hash = object
        .get("snapshot_hash")
        .and_then(Value::as_str)
        .and_then(|value| ContentHash::parse(value.to_owned()).ok())
        .ok_or(AgentV1Error::InvalidRequest {
            procedure,
            reason: "session memory snapshot hash is invalid",
        })?;
    if ContentHash::sha256(&canonical) != declared_hash {
        return Err(AgentV1Error::InvalidRequest {
            procedure,
            reason: "session memory snapshot hash mismatch",
        });
    }
    let snapshot: SessionMemorySnapshotV3 = serde_json::from_value(snapshot_value.clone())
        .map_err(|_| AgentV1Error::InvalidRequest {
            procedure,
            reason: "session memory snapshot shape is invalid",
        })?;
    snapshot
        .validate()
        .map_err(|_| AgentV1Error::InvalidRequest {
            procedure,
            reason: "session memory snapshot contract is invalid",
        })?;
    if object.get("snapshot_revision").and_then(Value::as_u64) != Some(snapshot.revision)
        || object.get("frontier_hash").and_then(Value::as_str)
            != Some(snapshot.frontier_hash.as_str())
        || object.get("source_lineage_hash").and_then(Value::as_str)
            != Some(snapshot.source_lineage_hash.as_str())
    {
        return Err(AgentV1Error::InvalidRequest {
            procedure,
            reason: "session memory snapshot anchor mismatch",
        });
    }
    Ok(())
}

fn validate_final_memory_contract(
    procedure: AgentV1Procedure,
    object: &Map<String, Value>,
) -> Result<(), AgentV1Error> {
    let delta_hash = object.get("session_memory_delta_hash");
    let delta = object.get("session_memory_delta");
    let next_frontier = object.get("next_memory_frontier_hash");
    if object
        .get("outbox_payloads")
        .is_some_and(outbox_contains_session_memory)
    {
        return Err(AgentV1Error::InvalidRequest {
            procedure,
            reason: "session memory body is forbidden in outbox payloads",
        });
    }
    let non_null = [delta_hash, delta, next_frontier]
        .into_iter()
        .filter(|value| value.is_some_and(|value| !value.is_null()))
        .count();
    if non_null == 0 {
        return Ok(());
    }
    if non_null != 3 {
        return Err(AgentV1Error::InvalidRequest {
            procedure,
            reason: "session memory fields must be all null or all non-null",
        });
    }
    let declared_delta_hash = ContentHash::parse(
        delta_hash
            .and_then(Value::as_str)
            .ok_or(AgentV1Error::InvalidRequest {
                procedure,
                reason: "session memory delta hash is invalid",
            })?
            .to_owned(),
    )
    .map_err(|_| AgentV1Error::InvalidRequest {
        procedure,
        reason: "session memory delta hash is invalid",
    })?;
    let declared_next = ContentHash::parse(
        next_frontier
            .and_then(Value::as_str)
            .ok_or(AgentV1Error::InvalidRequest {
                procedure,
                reason: "session memory frontier hash is invalid",
            })?
            .to_owned(),
    )
    .map_err(|_| AgentV1Error::InvalidRequest {
        procedure,
        reason: "session memory frontier hash is invalid",
    })?;
    let delta = delta.ok_or(AgentV1Error::InvalidRequest {
        procedure,
        reason: "session memory delta is required",
    })?;
    if !delta.is_object() {
        return Err(AgentV1Error::InvalidRequest {
            procedure,
            reason: "session memory delta must be an object",
        });
    }
    let canonical_delta = serde_jcs::to_vec(delta)?;
    if canonical_delta.is_empty() || canonical_delta.len() > 1024 * 1024 {
        return Err(AgentV1Error::InvalidRequest {
            procedure,
            reason: "session memory delta byte limit exceeded",
        });
    }
    if ContentHash::sha256(&canonical_delta) != declared_delta_hash {
        return Err(AgentV1Error::InvalidRequest {
            procedure,
            reason: "session memory delta hash mismatch",
        });
    }
    let typed_delta: SessionMemoryDeltaV3 =
        serde_json::from_value(delta.clone()).map_err(|_| AgentV1Error::InvalidRequest {
            procedure,
            reason: "session memory delta shape is invalid",
        })?;
    typed_delta
        .validate()
        .map_err(|_| AgentV1Error::InvalidRequest {
            procedure,
            reason: "session memory delta contract is invalid",
        })?;
    if typed_delta.revision > i64::MAX as u64 {
        return Err(AgentV1Error::InvalidRequest {
            procedure,
            reason: "session memory revision is invalid",
        });
    }
    if session_memory_frontier_hash(
        &typed_delta.parent_frontier_hash,
        typed_delta.revision,
        &declared_delta_hash,
    ) != declared_next
    {
        return Err(AgentV1Error::InvalidRequest {
            procedure,
            reason: "session memory next frontier hash mismatch",
        });
    }
    Ok(())
}

fn outbox_contains_session_memory(value: &Value) -> bool {
    match value {
        Value::Array(values) => values.iter().any(outbox_contains_session_memory),
        Value::Object(values) => values.iter().any(|(key, value)| {
            matches!(
                key.as_str(),
                "session_memory_delta" | "session_memory_delta_hash" | "next_memory_frontier_hash"
            ) || outbox_contains_session_memory(value)
        }),
        _ => false,
    }
}

/// Exact cross-language session-memory frontier transition.
pub fn session_memory_frontier_hash(
    parent_hash: &ContentHash,
    revision: u64,
    delta_hash: &ContentHash,
) -> ContentHash {
    let revision = revision.to_string();
    let mut preimage = Vec::with_capacity(
        "krw.session-memory/frontier-v2".len()
            + parent_hash.as_str().len()
            + revision.len()
            + delta_hash.as_str().len()
            + 3,
    );
    preimage.extend_from_slice(b"krw.session-memory/frontier-v2\0");
    preimage.extend_from_slice(parent_hash.as_str().as_bytes());
    preimage.push(0);
    preimage.extend_from_slice(revision.as_bytes());
    preimage.push(0);
    preimage.extend_from_slice(delta_hash.as_str().as_bytes());
    ContentHash::sha256(preimage)
}

/// `RejectionKind` is intentionally coarse-grained — multiple SQL procedures
/// may raise the same K-code for procedure-local conditions. Callers requiring
/// procedure-scoped semantics must inspect the error message, not just the
/// typed variant.
///
/// Known collisions: K1024/K1025/K1026 are shared between session-memory
/// procedures (0004/0005/0008) and `read_final_output`/`read_final_projection`
/// (0006/0011). K1027/K1028 are presently unique to `read_final_projection`
/// (0011) principal/session ownership checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectionKind {
    InvalidRequest,
    UnknownRun,
    StaleFence,
    TerminalRun,
    MutationConflict,
    ActionConflict,
    UnknownAction,
    ObservationConflict,
    CommitWithoutObservation,
    CancelGenerationMismatch,
    CancelledBeforeFinal,
    FinalConflict,
    RunVersionMismatch,
    LeaseLost,
    EpisodeNotCommitted,
    CheckpointSequenceMismatch,
    PendingAction,
    TenantMismatch,
    RecoveryReceiptLimitExceeded,
    UnknownOutboxEvent,
    OutboxLeaseLost,
    ActionFrontierMismatch,
    FairQueueTagOverflow,
    ActionFinalizationConflict,
    ChildExecutionConflict,
    SessionMemoryConflict,
    SessionMemoryOwnershipMismatch,
    PrincipalMismatch,
    SessionMismatch,
}

impl RejectionKind {
    pub fn from_sqlstate(sqlstate: &str) -> Option<Self> {
        Some(match sqlstate {
            "K1000" => Self::InvalidRequest,
            "K1001" => Self::UnknownRun,
            "K1002" => Self::StaleFence,
            "K1003" => Self::TerminalRun,
            "K1004" => Self::MutationConflict,
            "K1005" => Self::ActionConflict,
            "K1006" => Self::UnknownAction,
            "K1007" => Self::ObservationConflict,
            "K1008" => Self::CommitWithoutObservation,
            "K1009" => Self::CancelGenerationMismatch,
            "K1010" => Self::CancelledBeforeFinal,
            "K1011" => Self::FinalConflict,
            "K1012" => Self::RunVersionMismatch,
            "K1013" => Self::LeaseLost,
            "K1014" => Self::EpisodeNotCommitted,
            "K1015" => Self::CheckpointSequenceMismatch,
            "K1016" => Self::PendingAction,
            "K1017" => Self::TenantMismatch,
            "K1018" => Self::RecoveryReceiptLimitExceeded,
            "K1019" => Self::UnknownOutboxEvent,
            "K1020" => Self::OutboxLeaseLost,
            "K1021" => Self::ActionFrontierMismatch,
            "K1022" => Self::FairQueueTagOverflow,
            "K1023" => Self::ActionFinalizationConflict,
            "K1024" => Self::ChildExecutionConflict,
            "K1025" => Self::SessionMemoryConflict,
            "K1026" => Self::SessionMemoryOwnershipMismatch,
            "K1027" => Self::PrincipalMismatch,
            "K1028" => Self::SessionMismatch,
            _ => return None,
        })
    }
}

#[derive(Debug, Error)]
pub enum AgentV1Error {
    #[error("could not encode agent_v1 request: {0}")]
    Encode(#[from] serde_json::Error),
    #[error("agent_v1 request for {0:?} must serialize to an object")]
    RequestMustBeObject(AgentV1Procedure),
    #[error("invalid request for {procedure:?}: {reason}")]
    InvalidRequest {
        procedure: AgentV1Procedure,
        reason: &'static str,
    },
    #[error("agent_v1 rejected {procedure:?}: {kind:?} ({diagnostic_hash})")]
    Rejected {
        procedure: AgentV1Procedure,
        kind: RejectionKind,
        diagnostic_hash: ContentHash,
    },
    #[error("agent_v1 database failure in {procedure:?} ({diagnostic_hash})")]
    Database {
        procedure: AgentV1Procedure,
        diagnostic_hash: ContentHash,
    },
    #[error("invalid response from {procedure:?}: {reason}")]
    InvalidResponse {
        procedure: AgentV1Procedure,
        reason: String,
    },
}

impl AgentV1Error {
    fn from_database_for(procedure: AgentV1Procedure, failure: DatabaseFailure) -> Self {
        if let Some(kind) = failure
            .sqlstate
            .as_deref()
            .and_then(RejectionKind::from_sqlstate)
        {
            return Self::Rejected {
                procedure,
                kind,
                diagnostic_hash: failure.diagnostic_hash,
            };
        }
        Self::Database {
            procedure,
            diagnostic_hash: failure.diagnostic_hash,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EnqueueRunRequest {
    pub mutation_id: String,
    pub run_id: String,
    pub tenant_id: String,
    pub principal_id: String,
    pub session_id: String,
    pub agent_image_hash: ContentHash,
    pub runtime_version: String,
    pub priority: i16,
    pub immutable_snapshot_hash: ContentHash,
    pub immutable_snapshot: Value,
    pub resource_profile: Value,
    pub budgets: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnqueueRunResponse {
    pub outcome: String,
    pub run_id: String,
    pub run_version: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimRunRequest {
    pub worker_id: String,
    pub lease_ms: u64,
    pub runtime_version: String,
    pub accepted_agent_image_hashes: Vec<ContentHash>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimRunResponse {
    pub claimed: bool,
    pub receipt: Option<ClaimReceipt>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimReceipt {
    pub run_id: String,
    pub tenant_id: String,
    pub principal_id: String,
    pub session_id: String,
    pub fencing_token: u64,
    pub run_version: u64,
    pub cancel_generation: u64,
    pub checkpoint_seq: u64,
    pub action_frontier_seq: u64,
    pub action_frontier_hash: ContentHash,
    pub lease_deadline: String,
    pub agent_image_hash: ContentHash,
    pub runtime_version: String,
    pub priority: i16,
    pub immutable_snapshot_hash: ContentHash,
    pub immutable_snapshot: Value,
    pub resource_profile: Value,
    pub budgets: Value,
    pub reclaimed: bool,
    pub recovery: RecoveryReceipt,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryReceipt {
    pub state_checkpoint: Option<RecoveryStateCheckpointReceipt>,
    pub episodes: Vec<RecoveryEpisodeReceipt>,
    pub actions: Vec<RecoveryActionReceipt>,
}

#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryStateCheckpointReceipt {
    pub state_hash: ContentHash,
    pub state_artifact_ref: String,
    pub state_size_bytes: u64,
    pub recovery_schema_hash: ContentHash,
    pub provider_checkpoint_seq: u64,
    pub action_frontier_seq: u64,
    pub action_frontier_hash: ContentHash,
}

impl fmt::Debug for RecoveryStateCheckpointReceipt {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RecoveryStateCheckpointReceipt")
            .field("state_hash", &self.state_hash)
            .field("state_artifact_ref", &"[REDACTED]")
            .field("state_size_bytes", &self.state_size_bytes)
            .field("recovery_schema_hash", &self.recovery_schema_hash)
            .field("provider_checkpoint_seq", &self.provider_checkpoint_seq)
            .field("action_frontier_seq", &self.action_frontier_seq)
            .field("action_frontier_hash", &self.action_frontier_hash)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryEpisodeReceipt {
    pub episode_hash: ContentHash,
    pub checkpoint_seq: u64,
    pub episode_artifact_ref: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryActionReceipt {
    pub action_key: String,
    pub request_hash: ContentHash,
    pub episode_hash: ContentHash,
    pub tool_call_id: String,
    pub capability_id: String,
    pub input_schema_hash: ContentHash,
    pub output_schema_hash: ContentHash,
    pub data_release_hash: ContentHash,
    pub arguments_artifact_ref: String,
    pub retryable_read: bool,
    pub stage: String,
    pub result_hash: Option<ContentHash>,
    pub result_artifact_ref: Option<String>,
    pub ambiguous_reason_code: Option<String>,
    pub disposition: Option<ActionDisposition>,
    pub validation_receipt_hash: Option<ContentHash>,
    pub policy_receipt_hash: Option<ContentHash>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RenewLeaseRequest {
    pub mutation_id: String,
    pub run_id: String,
    pub tenant_id: String,
    pub fencing_token: u64,
    pub expected_run_version: u64,
    pub worker_id: String,
    pub lease_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CheckpointEpisodeRequest {
    pub mutation_id: String,
    pub run_id: String,
    pub tenant_id: String,
    pub fencing_token: u64,
    pub expected_run_version: u64,
    pub expected_checkpoint_seq: u64,
    pub episode_hash: ContentHash,
    pub episode_artifact_ref: String,
}

/// Metadata for a typed runtime-state artifact that was already durably
/// written to encrypted CAS. `PostgreSQL` stores only this bounded receipt.
#[derive(Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CheckpointRunStateRequest {
    pub mutation_id: String,
    pub run_id: String,
    pub tenant_id: String,
    pub fencing_token: u64,
    pub expected_run_version: u64,
    pub provider_checkpoint_seq: u64,
    pub action_frontier_seq: u64,
    pub action_frontier_hash: ContentHash,
    pub recovery_schema_hash: ContentHash,
    pub state_hash: ContentHash,
    pub state_artifact_ref: String,
    pub state_size_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lifecycle_stage: Option<String>,
}

impl fmt::Debug for CheckpointRunStateRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CheckpointRunStateRequest")
            .field("mutation_id_hash", &ContentHash::sha256(&self.mutation_id))
            .field("run_id_hash", &ContentHash::sha256(&self.run_id))
            .field("tenant_id_hash", &ContentHash::sha256(&self.tenant_id))
            .field("fencing_token", &self.fencing_token)
            .field("expected_run_version", &self.expected_run_version)
            .field("provider_checkpoint_seq", &self.provider_checkpoint_seq)
            .field("action_frontier_seq", &self.action_frontier_seq)
            .field("action_frontier_hash", &self.action_frontier_hash)
            .field("recovery_schema_hash", &self.recovery_schema_hash)
            .field("state_hash", &self.state_hash)
            .field("state_artifact_ref", &"[REDACTED]")
            .field("state_size_bytes", &self.state_size_bytes)
            .field("lifecycle_stage", &self.lifecycle_stage)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckpointRunStateResponse {
    pub outcome: String,
    pub run_id: String,
    pub fencing_token: u64,
    pub run_version: u64,
    pub provider_checkpoint_seq: u64,
    pub action_frontier_seq: u64,
    pub action_frontier_hash: ContentHash,
    pub recovery_schema_hash: ContentHash,
    pub state_hash: ContentHash,
    pub state_size_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BeginActionRequest {
    pub mutation_id: String,
    pub run_id: String,
    pub tenant_id: String,
    pub fencing_token: u64,
    pub expected_run_version: u64,
    pub action_key: String,
    pub request_hash: ContentHash,
    pub episode_hash: ContentHash,
    pub tool_call_id: String,
    pub capability_id: String,
    pub input_schema_hash: ContentHash,
    pub output_schema_hash: ContentHash,
    pub data_release_hash: ContentHash,
    pub arguments_artifact_ref: String,
    pub retryable_read: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ObserveActionRequest {
    pub mutation_id: String,
    pub run_id: String,
    pub tenant_id: String,
    pub fencing_token: u64,
    pub expected_run_version: u64,
    pub action_key: String,
    pub result_hash: ContentHash,
    pub result_artifact_ref: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FinalizeActionRequest {
    pub mutation_id: String,
    pub run_id: String,
    pub tenant_id: String,
    pub fencing_token: u64,
    pub expected_run_version: u64,
    pub action_key: String,
    pub result_hash: ContentHash,
    pub disposition: ActionDisposition,
    pub validation_receipt_hash: ContentHash,
    pub policy_receipt_hash: ContentHash,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MarkActionAmbiguousRequest {
    pub mutation_id: String,
    pub run_id: String,
    pub tenant_id: String,
    pub fencing_token: u64,
    pub expected_run_version: u64,
    pub action_key: String,
    pub reason_code: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MutationReceipt {
    pub outcome: String,
    pub run_id: String,
    pub fencing_token: u64,
    pub run_version: u64,
    pub cancel_generation: u64,
    pub checkpoint_seq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionAbiReceipt {
    pub outcome: String,
    pub run_id: String,
    pub fencing_token: u64,
    pub run_version: u64,
    pub action_key: String,
    pub stage: String,
    pub request_hash: ContentHash,
    pub result_hash: Option<ContentHash>,
    pub retryable_read: bool,
    pub disposition: Option<ActionDisposition>,
    pub validation_receipt_hash: Option<ContentHash>,
    pub policy_receipt_hash: Option<ContentHash>,
    pub action_frontier_seq: u64,
    pub action_frontier_hash: ContentHash,
}

/// Compare-and-swap of the one closed child-execution receipt owned by a run.
/// The receipt is generated and fully validated by the bounded child state
/// machine before this request is constructed. `PostgreSQL` linearizes only the
/// hash-bound transition; it never interprets provider text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CheckpointChildExecutionRequest {
    pub mutation_id: String,
    pub run_id: String,
    pub tenant_id: String,
    pub fencing_token: u64,
    pub expected_run_version: u64,
    pub expected_cancel_generation: u64,
    pub child_id: String,
    pub expected_receipt_hash: Option<ContentHash>,
    pub target_stage: ChildStage,
    pub receipt_hash: ContentHash,
    pub receipt: ChildExecutionReceipt,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChildExecutionAbiReceipt {
    pub outcome: String,
    pub run_id: String,
    pub fencing_token: u64,
    pub run_version: u64,
    pub child_id: String,
    pub stage: ChildStage,
    pub receipt_hash: ContentHash,
    pub receipt: ChildExecutionReceipt,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReadChildExecutionRequest {
    pub run_id: String,
    pub tenant_id: String,
    pub fencing_token: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadChildExecutionResponse {
    pub run_id: String,
    pub fencing_token: u64,
    pub run_version: u64,
    pub cancel_generation: u64,
    pub child: Option<ChildExecutionAbiReceipt>,
}

#[derive(Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FinalCommitRequest {
    pub mutation_id: String,
    pub run_id: String,
    pub tenant_id: String,
    pub fencing_token: u64,
    pub expected_run_version: u64,
    pub expected_cancel_generation: u64,
    pub answer_bundle_hash: ContentHash,
    pub final_output_hash: ContentHash,
    pub rendered_message_hash: ContentHash,
    pub answer_bundle: Value,
    pub usage: Value,
    pub settlement: Value,
    pub outbox_payloads: BTreeMap<String, Value>,
    pub session_memory_delta_hash: Option<ContentHash>,
    pub session_memory_delta: Option<Value>,
    pub next_memory_frontier_hash: Option<ContentHash>,
}

impl fmt::Debug for FinalCommitRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FinalCommitRequest")
            .field("mutation_id_hash", &ContentHash::sha256(&self.mutation_id))
            .field("run_id_hash", &ContentHash::sha256(&self.run_id))
            .field("tenant_id_hash", &ContentHash::sha256(&self.tenant_id))
            .field("fencing_token", &self.fencing_token)
            .field("expected_run_version", &self.expected_run_version)
            .field(
                "expected_cancel_generation",
                &self.expected_cancel_generation,
            )
            .field("answer_bundle_hash", &self.answer_bundle_hash)
            .field("final_output_hash", &self.final_output_hash)
            .field("rendered_message_hash", &self.rendered_message_hash)
            .field("answer_bundle", &"[REDACTED]")
            .field("usage", &"[REDACTED]")
            .field("settlement", &"[REDACTED]")
            .field("outbox_payload_count", &self.outbox_payloads.len())
            .field("session_memory_delta_hash", &self.session_memory_delta_hash)
            .field("session_memory_delta", &"[REDACTED]")
            .field("next_memory_frontier_hash", &self.next_memory_frontier_hash)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FinalCommitResponse {
    pub outcome: String,
    pub run_id: String,
    pub fencing_token: u64,
    pub run_version: u64,
    pub answer_bundle_hash: ContentHash,
    pub memory_revision: Option<u64>,
    pub memory_frontier_hash: Option<ContentHash>,
    pub memory_source_lineage_hash: Option<ContentHash>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReadSessionMemoryRequest {
    pub run_id: String,
    pub tenant_id: String,
    pub principal_id: String,
    pub session_id: String,
    pub fencing_token: u64,
    pub after_revision: u64,
    pub limit: u16,
    pub mode: SessionMemoryReadMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionMemoryReadMode {
    SnapshotTail,
    AuditRebuild,
}

#[derive(Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CheckpointSessionMemorySnapshotRequest {
    pub mutation_id: String,
    pub run_id: String,
    pub tenant_id: String,
    pub principal_id: String,
    pub session_id: String,
    pub fencing_token: u64,
    pub expected_run_version: u64,
    pub snapshot_revision: u64,
    pub frontier_hash: ContentHash,
    pub source_lineage_hash: ContentHash,
    pub snapshot_hash: ContentHash,
    pub snapshot_size_bytes: u64,
    pub snapshot: Value,
}

impl fmt::Debug for CheckpointSessionMemorySnapshotRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CheckpointSessionMemorySnapshotRequest")
            .field("mutation_id_hash", &ContentHash::sha256(&self.mutation_id))
            .field("run_id_hash", &ContentHash::sha256(&self.run_id))
            .field("tenant_id_hash", &ContentHash::sha256(&self.tenant_id))
            .field(
                "principal_id_hash",
                &ContentHash::sha256(&self.principal_id),
            )
            .field("session_id_hash", &ContentHash::sha256(&self.session_id))
            .field("fencing_token", &self.fencing_token)
            .field("expected_run_version", &self.expected_run_version)
            .field("snapshot_revision", &self.snapshot_revision)
            .field("frontier_hash", &self.frontier_hash)
            .field("source_lineage_hash", &self.source_lineage_hash)
            .field("snapshot_hash", &self.snapshot_hash)
            .field("snapshot_size_bytes", &self.snapshot_size_bytes)
            .field("snapshot", &"[REDACTED]")
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckpointSessionMemorySnapshotResponse {
    pub outcome: String,
    pub run_id: String,
    pub fencing_token: u64,
    pub run_version: u64,
    pub snapshot_revision: u64,
    pub frontier_hash: ContentHash,
    pub source_lineage_hash: ContentHash,
    pub snapshot_hash: ContentHash,
}

#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionMemoryDeltaReceipt {
    pub revision: u64,
    pub parent_frontier_hash: ContentHash,
    pub delta_hash: ContentHash,
    pub next_frontier_hash: ContentHash,
    pub source_lineage_hash: ContentHash,
    pub source_run_id: String,
    pub delta: Value,
}

#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionMemorySnapshotReceipt {
    pub revision: u64,
    pub frontier_hash: ContentHash,
    pub source_lineage_hash: ContentHash,
    pub snapshot_hash: ContentHash,
    pub snapshot_size_bytes: u64,
    pub snapshot: Value,
}

impl fmt::Debug for SessionMemorySnapshotReceipt {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SessionMemorySnapshotReceipt")
            .field("revision", &self.revision)
            .field("frontier_hash", &self.frontier_hash)
            .field("source_lineage_hash", &self.source_lineage_hash)
            .field("snapshot_hash", &self.snapshot_hash)
            .field("snapshot_size_bytes", &self.snapshot_size_bytes)
            .field("snapshot", &"[REDACTED]")
            .finish()
    }
}

impl fmt::Debug for SessionMemoryDeltaReceipt {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SessionMemoryDeltaReceipt")
            .field("revision", &self.revision)
            .field("parent_frontier_hash", &self.parent_frontier_hash)
            .field("delta_hash", &self.delta_hash)
            .field("next_frontier_hash", &self.next_frontier_hash)
            .field("source_lineage_hash", &self.source_lineage_hash)
            .field(
                "source_run_id_hash",
                &ContentHash::sha256(&self.source_run_id),
            )
            .field("delta", &"[REDACTED]")
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadSessionMemoryResponse {
    pub run_id: String,
    pub fencing_token: u64,
    pub run_version: u64,
    pub memory_revision: u64,
    pub memory_frontier_hash: ContentHash,
    pub memory_source_lineage_hash: ContentHash,
    pub snapshot: Option<SessionMemorySnapshotReceipt>,
    pub deltas: Vec<SessionMemoryDeltaReceipt>,
    pub next_after_revision: u64,
    pub has_more: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CancelRequest {
    pub mutation_id: String,
    pub run_id: String,
    pub tenant_id: String,
    pub reason_code: String,
    pub release: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CancelResponse {
    pub outcome: String,
    pub run_id: String,
    pub fencing_token: u64,
    pub run_version: u64,
    pub cancel_generation: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureDisposition {
    Defer,
    Fail,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FailOrDeferRequest {
    pub mutation_id: String,
    pub run_id: String,
    pub tenant_id: String,
    pub fencing_token: u64,
    pub expected_run_version: u64,
    pub disposition: FailureDisposition,
    pub reason_code: String,
    pub retry_delay_ms: u64,
    pub release: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FailOrDeferResponse {
    pub outcome: String,
    pub state: String,
    pub run_id: String,
    pub fencing_token: u64,
    pub run_version: u64,
    pub cancel_generation: u64,
    pub checkpoint_seq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimOutboxRequest {
    pub worker_id: String,
    pub limit: u16,
    pub lease_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimOutboxResponse {
    pub events: Vec<OutboxReceipt>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutboxReceipt {
    pub outbox_id: u64,
    pub run_id: String,
    pub event_kind: String,
    pub dedupe_key: String,
    pub payload: Value,
    pub delivery_attempts: u32,
    pub delivery_deadline: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AckOutboxRequest {
    pub worker_id: String,
    pub outbox_id: u64,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_hash: Option<ContentHash>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AckOutboxResponse {
    pub outcome: String,
    pub outbox_id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReadCommittedOutcomeRequest {
    pub run_id: String,
    pub tenant_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadCommittedOutcomeResponse {
    pub run_id: String,
    pub state: String,
    pub fencing_token: u64,
    pub run_version: u64,
    pub cancel_generation: u64,
    pub terminal_outcome: Option<Value>,
}

/// A daemon-owned, non-secret liveness receipt. It is deliberately separate
/// from session/run state: a front end can reject new work when no exact
/// daemon release is available without inspecting customer content or model
/// transcripts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HeartbeatDaemonRequest {
    pub daemon_id: String,
    pub provider: String,
    pub descriptor_artifact_hash: ContentHash,
    pub release_set_hash: ContentHash,
    pub runtime_version: String,
    pub heartbeat_ttl_ms: u64,
    pub mcp_ready: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeartbeatDaemonResponse {
    pub ready: bool,
    /// Echo of the daemon's MCP readiness (migration 0022 requires the
    /// heartbeat to carry it; the response confirms what was persisted).
    pub mcp_ready: bool,
    pub heartbeat_expires_at: String,
}

macro_rules! impl_request {
    ($request:ty, $response:ty, $procedure:expr) => {
        impl sealed::Sealed for $request {}
        impl AgentV1Request for $request {
            type Response = $response;
            const PROCEDURE: AgentV1Procedure = $procedure;
        }
    };
}

impl_request!(
    EnqueueRunRequest,
    EnqueueRunResponse,
    AgentV1Procedure::EnqueueRun
);
impl_request!(
    ClaimRunRequest,
    ClaimRunResponse,
    AgentV1Procedure::ClaimRun
);
impl_request!(
    RenewLeaseRequest,
    MutationReceipt,
    AgentV1Procedure::RenewLease
);
impl_request!(
    CheckpointEpisodeRequest,
    MutationReceipt,
    AgentV1Procedure::CheckpointEpisode
);
impl_request!(
    CheckpointRunStateRequest,
    CheckpointRunStateResponse,
    AgentV1Procedure::CheckpointRunState
);
impl_request!(
    BeginActionRequest,
    ActionAbiReceipt,
    AgentV1Procedure::BeginAction
);
impl_request!(
    ObserveActionRequest,
    ActionAbiReceipt,
    AgentV1Procedure::ObserveAction
);
impl_request!(
    FinalizeActionRequest,
    ActionAbiReceipt,
    AgentV1Procedure::FinalizeAction
);
impl_request!(
    MarkActionAmbiguousRequest,
    ActionAbiReceipt,
    AgentV1Procedure::MarkActionAmbiguous
);
impl_request!(
    CheckpointChildExecutionRequest,
    ChildExecutionAbiReceipt,
    AgentV1Procedure::CheckpointChildExecution
);
impl_request!(
    ReadChildExecutionRequest,
    ReadChildExecutionResponse,
    AgentV1Procedure::ReadChildExecution
);
impl_request!(
    ReadSessionMemoryRequest,
    ReadSessionMemoryResponse,
    AgentV1Procedure::ReadSessionMemory
);
impl_request!(
    CheckpointSessionMemorySnapshotRequest,
    CheckpointSessionMemorySnapshotResponse,
    AgentV1Procedure::CheckpointSessionMemorySnapshot
);
impl_request!(
    FinalCommitRequest,
    FinalCommitResponse,
    AgentV1Procedure::CommitFinal
);
impl_request!(
    CancelRequest,
    CancelResponse,
    AgentV1Procedure::RequestCancel
);
impl_request!(
    FailOrDeferRequest,
    FailOrDeferResponse,
    AgentV1Procedure::FailOrDefer
);
impl_request!(
    ClaimOutboxRequest,
    ClaimOutboxResponse,
    AgentV1Procedure::ClaimOutbox
);
impl_request!(
    AckOutboxRequest,
    AckOutboxResponse,
    AgentV1Procedure::AckOutbox
);
impl_request!(
    ReadCommittedOutcomeRequest,
    ReadCommittedOutcomeResponse,
    AgentV1Procedure::ReadCommittedOutcome
);
impl_request!(
    HeartbeatDaemonRequest,
    HeartbeatDaemonResponse,
    AgentV1Procedure::HeartbeatDaemon
);

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use krw_agent_bounded_child::{
        ChildBudgetLimits, ReserveChildMutation, SealedInputRef, reserve,
    };
    use krw_agent_protocol::BudgetUsage;

    use super::*;

    #[derive(Debug)]
    struct FixtureExecutor {
        calls: Mutex<Vec<(AgentV1Procedure, Value)>>,
        response: Value,
        failure: Option<DatabaseFailure>,
    }

    #[async_trait]
    impl JsonProcedureExecutor for FixtureExecutor {
        async fn execute_json(
            &self,
            procedure: AgentV1Procedure,
            request: Value,
        ) -> Result<Value, DatabaseFailure> {
            self.calls.lock().unwrap().push((procedure, request));
            self.failure
                .clone()
                .map_or_else(|| Ok(self.response.clone()), Err)
        }
    }

    fn claim_request() -> ClaimRunRequest {
        ClaimRunRequest {
            worker_id: "worker-a".into(),
            lease_ms: 30_000,
            runtime_version: "0.1.0".into(),
            accepted_agent_image_hashes: vec![ContentHash::sha256("image")],
        }
    }

    fn enqueue_request() -> EnqueueRunRequest {
        EnqueueRunRequest {
            mutation_id: "enqueue-1".into(),
            run_id: "run-1".into(),
            tenant_id: "tenant-1".into(),
            principal_id: "principal-1".into(),
            session_id: "session-1".into(),
            agent_image_hash: ContentHash::sha256("image"),
            runtime_version: "0.1.0".into(),
            priority: 0,
            immutable_snapshot_hash: ContentHash::sha256("snapshot"),
            immutable_snapshot: serde_json::json!({"pinned": true}),
            resource_profile: serde_json::json!({"class": "small"}),
            budgets: serde_json::json!({"max_provider_turns": 8}),
        }
    }

    fn reserved_child_receipt() -> ChildExecutionReceipt {
        let capability_call_limits = BTreeMap::from([
            ("ontology.query_context".into(), 1),
            ("ontology.query".into(), 1),
            ("ontology.trace".into(), 1),
        ]);
        reserve(&ReserveChildMutation {
            mutation_id: "reserve-child".into(),
            run_id: "run-1".into(),
            child_id: "child-1".into(),
            role_id: "company_evidence_researcher".into(),
            fencing_token: 7,
            expected_cancel_generation: 0,
            parent_depth: 0,
            policy_hash: ContentHash::sha256("policy"),
            reservation: ChildBudgetLimits {
                max_provider_turns: 4,
                max_capability_calls: 3,
                max_input_tokens: 10_000,
                max_output_tokens: 2_000,
                max_evidence_bytes: 1_048_576,
                capability_call_limits: capability_call_limits.clone(),
            },
            allowed_capabilities: capability_call_limits.keys().cloned().collect(),
            sealed_inputs: vec![SealedInputRef {
                name: "sealed_brief".into(),
                contract_id: "krw-guru-company-brief-result/v1".into(),
                content_hash: ContentHash::sha256("sealed-brief"),
            }],
            baseline_usage: BudgetUsage::default(),
            baseline_capability_calls_by_id: capability_call_limits
                .keys()
                .map(|capability| (capability.clone(), 0))
                .collect(),
            baseline_ledger_hash: ContentHash::sha256("ledger"),
            baseline_evidence_ids: Vec::new(),
        })
        .unwrap()
    }

    #[test]
    fn child_checkpoint_abi_hash_binds_the_validated_receipt() {
        let receipt = reserved_child_receipt();
        let request = CheckpointChildExecutionRequest {
            mutation_id: "checkpoint-child".into(),
            run_id: receipt.run_id.clone(),
            tenant_id: "tenant-1".into(),
            fencing_token: receipt.fencing_token,
            expected_run_version: 1,
            expected_cancel_generation: receipt.cancel_generation,
            child_id: receipt.child_id.clone(),
            expected_receipt_hash: None,
            target_stage: receipt.stage,
            receipt_hash: receipt.receipt_hash().unwrap(),
            receipt,
        };
        let encoded = encode_request(AgentV1Procedure::CheckpointChildExecution, &request).unwrap();
        assert!(encoded.get("mutation_hash").is_some());
        assert_eq!(encoded["target_stage"], "reserved");
        assert!(encoded["receipt"].get("transcript").is_none());

        let mut mismatched = request;
        mismatched.receipt_hash = ContentHash::sha256("wrong-receipt");
        assert!(matches!(
            encode_request(AgentV1Procedure::CheckpointChildExecution, &mismatched),
            Err(AgentV1Error::InvalidRequest { .. })
        ));
    }

    #[test]
    fn bounded_child_migration_is_closed_receipt_only_and_fenced() {
        for required in [
            "CREATE TABLE IF NOT EXISTS agent_store.child_executions",
            "PRIMARY KEY REFERENCES agent_store.runs(run_id)",
            "CREATE OR REPLACE FUNCTION agent_v1.checkpoint_child_execution",
            "CREATE OR REPLACE FUNCTION agent_v1.read_child_execution",
            "child_transition_conflict",
            "expected_cancel_generation",
            "NOT receipt ? 'transcript'",
            "NOT receipt ? 'messages'",
            "REVOKE ALL ON TABLE agent_store.child_executions FROM PUBLIC",
        ] {
            assert!(
                BOUNDED_CHILD_MIGRATION_SQL.contains(required),
                "missing bounded-child migration contract: {required}"
            );
        }
    }

    #[test]
    fn procedure_inventory_contains_only_single_json_calls() {
        let procedures = [
            AgentV1Procedure::EnqueueRun,
            AgentV1Procedure::ClaimRun,
            AgentV1Procedure::RenewLease,
            AgentV1Procedure::CheckpointEpisode,
            AgentV1Procedure::CheckpointRunState,
            AgentV1Procedure::BeginAction,
            AgentV1Procedure::ObserveAction,
            AgentV1Procedure::FinalizeAction,
            AgentV1Procedure::MarkActionAmbiguous,
            AgentV1Procedure::CheckpointChildExecution,
            AgentV1Procedure::ReadChildExecution,
            AgentV1Procedure::ReadSessionMemory,
            AgentV1Procedure::CheckpointSessionMemorySnapshot,
            AgentV1Procedure::CommitFinal,
            AgentV1Procedure::RequestCancel,
            AgentV1Procedure::FailOrDefer,
            AgentV1Procedure::ClaimOutbox,
            AgentV1Procedure::AckOutbox,
            AgentV1Procedure::ReadCommittedOutcome,
            AgentV1Procedure::HeartbeatDaemon,
        ];
        for procedure in procedures {
            assert_eq!(
                procedure.statement(),
                format!("SELECT {}($1::jsonb)", procedure.name())
            );
            assert!(!procedure.statement().contains(';'));
            assert!(!procedure.statement().contains("agent_store"));
        }
    }

    #[test]
    fn mutation_hash_binds_the_full_request_and_procedure() {
        let base = BeginActionRequest {
            mutation_id: "mutation-1".into(),
            run_id: "run-1".into(),
            tenant_id: "tenant-1".into(),
            fencing_token: 3,
            expected_run_version: 5,
            action_key: "action-1".into(),
            request_hash: ContentHash::sha256("request"),
            episode_hash: ContentHash::sha256("episode"),
            tool_call_id: "call-1".into(),
            capability_id: "ontology.query_context".into(),
            input_schema_hash: ContentHash::sha256("input"),
            output_schema_hash: ContentHash::sha256("output"),
            data_release_hash: ContentHash::sha256("release"),
            arguments_artifact_ref: "cas://arguments".into(),
            retryable_read: true,
        };
        let first = encode_request(AgentV1Procedure::BeginAction, &base).unwrap();
        let mut changed = base;
        changed.capability_id = "ontology.query".into();
        let second = encode_request(AgentV1Procedure::BeginAction, &changed).unwrap();
        assert_ne!(first["mutation_hash"], second["mutation_hash"]);
        assert_eq!(first["abi_version"], ABI_VERSION);
    }

    #[test]
    fn finalize_action_hash_binds_disposition_and_both_receipts() {
        let base = FinalizeActionRequest {
            mutation_id: "finalize-1".into(),
            run_id: "run-1".into(),
            tenant_id: "tenant-1".into(),
            fencing_token: 3,
            expected_run_version: 5,
            action_key: "action-1".into(),
            result_hash: ContentHash::sha256("raw-result"),
            disposition: ActionDisposition::Accepted,
            validation_receipt_hash: ContentHash::sha256("validation"),
            policy_receipt_hash: ContentHash::sha256("policy"),
        };
        let accepted = encode_request(AgentV1Procedure::FinalizeAction, &base).unwrap();
        let mut rejected = base.clone();
        rejected.disposition = ActionDisposition::Rejected;
        let rejected = encode_request(AgentV1Procedure::FinalizeAction, &rejected).unwrap();
        let mut different_policy = base;
        different_policy.policy_receipt_hash = ContentHash::sha256("different-policy");
        let different_policy =
            encode_request(AgentV1Procedure::FinalizeAction, &different_policy).unwrap();

        assert_ne!(accepted["mutation_hash"], rejected["mutation_hash"]);
        assert_ne!(accepted["mutation_hash"], different_policy["mutation_hash"]);
    }

    #[test]
    fn runtime_state_checkpoint_is_bounded_hashed_and_redacted() {
        let request = CheckpointRunStateRequest {
            mutation_id: "state-1".into(),
            run_id: "run-1".into(),
            tenant_id: "tenant-1".into(),
            fencing_token: 7,
            expected_run_version: 12,
            provider_checkpoint_seq: 3,
            action_frontier_seq: 5,
            action_frontier_hash: ContentHash::sha256("frontier"),
            recovery_schema_hash: ContentHash::sha256("recovery-state/v1"),
            state_hash: ContentHash::sha256("private runtime state"),
            state_artifact_ref: "cas://private-state-reference".into(),
            state_size_bytes: 21,
            lifecycle_stage: None,
        };
        let encoded = encode_request(AgentV1Procedure::CheckpointRunState, &request).unwrap();
        assert_eq!(encoded["abi_version"], ABI_VERSION);
        assert!(encoded.get("mutation_hash").is_some());
        assert_eq!(encoded["provider_checkpoint_seq"], 3);
        assert_eq!(encoded["action_frontier_seq"], 5);
        assert!(encoded.get("lifecycle_stage").is_none());
        let debug = format!("{request:?}");
        assert!(!debug.contains("cas://private-state-reference"));

        let mut composing = request.clone();
        composing.lifecycle_stage = Some("composing".into());
        let encoded = encode_request(AgentV1Procedure::CheckpointRunState, &composing).unwrap();
        assert_eq!(encoded["lifecycle_stage"], "composing");

        let mut oversized = request;
        oversized.state_size_bytes = 8 * 1024 * 1024 + 1;
        assert!(matches!(
            encode_request(AgentV1Procedure::CheckpointRunState, &oversized),
            Err(AgentV1Error::InvalidRequest { .. })
        ));
    }

    #[test]
    fn enqueue_identity_explicitly_binds_principal_scope() {
        let base = enqueue_request();
        let first = encode_request(AgentV1Procedure::EnqueueRun, &base).unwrap();
        assert_eq!(first["principal_id"], "principal-1");

        let mut changed = base;
        changed.principal_id = "principal-2".into();
        let second = encode_request(AgentV1Procedure::EnqueueRun, &changed).unwrap();
        assert_ne!(first["mutation_hash"], second["mutation_hash"]);
    }

    #[test]
    fn adapter_rejects_unbounded_inventory_before_database_io() {
        let mut request = claim_request();
        request.accepted_agent_image_hashes = (0..65)
            .map(|index| ContentHash::sha256(index.to_string()))
            .collect();
        assert!(matches!(
            encode_request(AgentV1Procedure::ClaimRun, &request),
            Err(AgentV1Error::InvalidRequest { .. })
        ));

        let ack = AckOutboxRequest {
            worker_id: "outbox-1".into(),
            outbox_id: 1,
            success: false,
            error_hash: None,
        };
        assert!(matches!(
            encode_request(AgentV1Procedure::AckOutbox, &ack),
            Err(AgentV1Error::InvalidRequest { .. })
        ));
    }

    #[test]
    fn external_cancel_contract_does_not_depend_on_worker_fence() {
        let request = CancelRequest {
            mutation_id: "cancel-1".into(),
            run_id: "run-1".into(),
            tenant_id: "tenant-1".into(),
            reason_code: "user".into(),
            release: serde_json::json!({}),
        };
        let encoded = encode_request(AgentV1Procedure::RequestCancel, &request).unwrap();
        assert!(encoded.get("fencing_token").is_none());
        assert!(encoded.get("expected_run_version").is_none());
        assert!(encoded.get("mutation_hash").is_some());
    }

    fn valid_memory_delta(run_id: &str, session_id: &str, revision: u64) -> Value {
        let user_content = "삼성전자 실적을 다시 확인해 주세요.";
        let answer_content = "근거를 확인한 답변입니다.";
        serde_json::json!({
            "schema_version": 3,
            "authority": "context_only",
            "session_id_hash": ContentHash::sha256(session_id),
            "parent_frontier_hash": krw_session_memory::empty_frontier_hash(),
            "revision": revision,
            "source": {
                "run_id": run_id,
                "revision": revision,
                "tickers": ["005930"],
                "user_message_id": krw_session_memory::deterministic_message_id(run_id, "user"),
                "assistant_message_id": krw_session_memory::deterministic_message_id(run_id, "assistant"),
                "final_commit_intent_hash": ContentHash::sha256("final-intent"),
                "answer_bundle_hash": ContentHash::sha256("answer-bundle"),
                "final_output_hash": ContentHash::sha256("final-output"),
            },
            "tickers": ["005930"],
            "constraints": [],
            "claims": [],
            "unresolved_goals": [],
            "supersessions": [],
            "resolved_goals": [],
            "recent_turn": {
                "source_run_id": run_id,
                "tickers": ["005930"],
                "user_content_hash": ContentHash::sha256(user_content),
                "user_content": user_content,
                "user_content_original_bytes": user_content.len(),
                "user_content_truncated": false,
                "answer_content_hash": ContentHash::sha256(answer_content),
                "answer_content": answer_content,
                "answer_content_original_bytes": answer_content.len(),
                "answer_content_truncated": false,
            }
        })
    }

    fn final_request_with_memory(delta: Option<Value>) -> FinalCommitRequest {
        let (delta_hash, next_frontier_hash) = delta.as_ref().map_or((None, None), |delta| {
            let typed: SessionMemoryDeltaV3 = serde_json::from_value(delta.clone()).unwrap();
            let delta_hash = ContentHash::sha256(serde_jcs::to_vec(delta).unwrap());
            let next = session_memory_frontier_hash(
                &typed.parent_frontier_hash,
                typed.revision,
                &delta_hash,
            );
            (Some(delta_hash), Some(next))
        });
        FinalCommitRequest {
            mutation_id: "final-1".into(),
            run_id: "run-1".into(),
            tenant_id: "tenant-1".into(),
            fencing_token: 3,
            expected_run_version: 5,
            expected_cancel_generation: 0,
            answer_bundle_hash: ContentHash::sha256("answer-bundle"),
            final_output_hash: ContentHash::sha256("final-output"),
            rendered_message_hash: ContentHash::sha256("rendered"),
            answer_bundle: serde_json::json!({"private": "answer"}),
            usage: serde_json::json!({"provider_turns": 2}),
            settlement: serde_json::json!({"kind": "settled"}),
            outbox_payloads: BTreeMap::new(),
            session_memory_delta_hash: delta_hash,
            session_memory_delta: delta,
            next_memory_frontier_hash: next_frontier_hash,
        }
    }

    #[test]
    fn final_memory_contract_is_typed_hash_bound_and_not_exportable() {
        let delta = valid_memory_delta("run-1", "session-1", 1);
        let request = final_request_with_memory(Some(delta));
        let encoded = encode_request(AgentV1Procedure::CommitFinal, &request).unwrap();
        assert_eq!(encoded["session_memory_delta"]["schema_version"], 3);
        let debug = format!("{request:?}");
        assert!(!debug.contains("삼성전자"));
        assert!(!debug.contains("private"));

        let mut partial = request.clone();
        partial.next_memory_frontier_hash = None;
        assert!(matches!(
            encode_request(AgentV1Procedure::CommitFinal, &partial),
            Err(AgentV1Error::InvalidRequest { .. })
        ));

        let mut wrong_delta_hash = request.clone();
        wrong_delta_hash.session_memory_delta_hash = Some(ContentHash::sha256("wrong"));
        assert!(matches!(
            encode_request(AgentV1Procedure::CommitFinal, &wrong_delta_hash),
            Err(AgentV1Error::InvalidRequest { .. })
        ));

        let mut wrong_frontier = request.clone();
        wrong_frontier.next_memory_frontier_hash = Some(ContentHash::sha256("wrong"));
        assert!(matches!(
            encode_request(AgentV1Procedure::CommitFinal, &wrong_frontier),
            Err(AgentV1Error::InvalidRequest { .. })
        ));

        let mut outbox_leak = request;
        outbox_leak.outbox_payloads.insert(
            "notification".into(),
            serde_json::json!({"nested": {"session_memory_delta": {"secret": true}}}),
        );
        assert!(matches!(
            encode_request(AgentV1Procedure::CommitFinal, &outbox_leak),
            Err(AgentV1Error::InvalidRequest { .. })
        ));
    }

    #[test]
    fn final_without_session_memory_has_no_memory_fields() {
        let request = final_request_with_memory(None);
        let encoded = encode_request(AgentV1Procedure::CommitFinal, &request).unwrap();
        assert!(encoded["session_memory_delta_hash"].is_null());
        assert!(encoded["session_memory_delta"].is_null());
        assert!(encoded["next_memory_frontier_hash"].is_null());
    }

    #[test]
    fn session_memory_read_contract_is_owner_scoped_and_bounded() {
        let request = ReadSessionMemoryRequest {
            run_id: "run-2".into(),
            tenant_id: "tenant-1".into(),
            principal_id: "principal-1".into(),
            session_id: "session-1".into(),
            fencing_token: 4,
            after_revision: 0,
            limit: 8,
            mode: SessionMemoryReadMode::SnapshotTail,
        };
        let encoded = encode_request(AgentV1Procedure::ReadSessionMemory, &request).unwrap();
        assert_eq!(encoded["principal_id"], "principal-1");
        assert_eq!(encoded["session_id"], "session-1");
        assert!(encoded.get("mutation_hash").is_none());

        let mut too_large = request.clone();
        too_large.limit = 9;
        assert!(matches!(
            encode_request(AgentV1Procedure::ReadSessionMemory, &too_large),
            Err(AgentV1Error::InvalidRequest { .. })
        ));
        let mut overflow = request;
        overflow.after_revision = i64::MAX as u64 + 1;
        assert!(matches!(
            encode_request(AgentV1Procedure::ReadSessionMemory, &overflow),
            Err(AgentV1Error::InvalidRequest { .. })
        ));
    }

    #[test]
    fn frontier_hash_matches_the_canonical_session_memory_implementation() {
        for revision in [1, 2, 17, 1_000_000, i64::MAX as u64] {
            let parent = ContentHash::sha256(format!("parent-{revision}"));
            let delta = ContentHash::sha256(format!("delta-{revision}"));
            assert_eq!(
                session_memory_frontier_hash(&parent, revision, &delta),
                krw_session_memory::next_frontier_hash(&parent, revision, &delta)
            );
        }
    }

    #[test]
    fn typescript_host_mutation_hash_vector_matches_rust_jcs() {
        let request = CancelRequest {
            mutation_id: "cancel-01".into(),
            run_id: "run-01".into(),
            tenant_id: "tenant-01".into(),
            reason_code: "user_cancelled".into(),
            release: serde_json::json!({"reservation_id": "reserve-01"}),
        };
        let encoded = encode_request(AgentV1Procedure::RequestCancel, &request).unwrap();
        assert_eq!(
            encoded["mutation_hash"],
            "sha256:4d17c9c238b8f131e7b3dae533c2445a99c334e43eab5310b983cfb8c81d2f8f"
        );
    }

    #[tokio::test]
    async fn client_decodes_typed_claim_and_injects_abi_version() {
        let response = serde_json::json!({
            "claimed": false,
            "receipt": null
        });
        let executor = FixtureExecutor {
            calls: Mutex::new(Vec::new()),
            response,
            failure: None,
        };
        let client = AgentV1Client::new(executor);
        let receipt = client.execute(&claim_request()).await.unwrap();
        assert!(!receipt.claimed);
        let calls = client.executor().calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, AgentV1Procedure::ClaimRun);
        assert_eq!(calls[0].1["abi_version"], ABI_VERSION);
        assert!(calls[0].1.get("mutation_hash").is_none());
    }

    #[tokio::test]
    async fn named_sqlstate_becomes_typed_rejection_without_raw_diagnostic() {
        let failure = DatabaseFailure::redacted(Some("K1002".into()), b"secret db detail");
        let expected_hash = failure.diagnostic_hash.clone();
        let executor = FixtureExecutor {
            calls: Mutex::new(Vec::new()),
            response: Value::Null,
            failure: Some(failure),
        };
        let client = AgentV1Client::new(executor);
        let error = client.execute(&claim_request()).await.unwrap_err();
        assert!(matches!(
            error,
            AgentV1Error::Rejected {
                kind: RejectionKind::StaleFence,
                ref diagnostic_hash,
                ..
            } if diagnostic_hash == &expected_hash
        ));
        assert!(!format!("{error:?}").contains("secret db detail"));
    }

    #[test]
    fn embedded_migration_contains_security_and_atomicity_contracts() {
        for required in [
            "SCHEMA IF NOT EXISTS agent_store",
            "SCHEMA IF NOT EXISTS agent_v1",
            "SECURITY DEFINER",
            "REVOKE ALL ON ALL TABLES IN SCHEMA agent_store FROM PUBLIC",
            "FUNCTION agent_v1.begin_action",
            "FUNCTION agent_v1.checkpoint_episode",
            "FUNCTION agent_v1.checkpoint_run_state",
            "FUNCTION agent_v1.commit_final",
            "FUNCTION agent_v1.request_cancel",
            "FUNCTION agent_v1.fail_or_defer",
            "FUNCTION agent_v1.claim_outbox",
            "FUNCTION agent_v1.ack_outbox",
            "TABLE IF NOT EXISTS agent_store.outbox",
            "TABLE IF NOT EXISTS agent_store.run_state_checkpoints",
            "FUNCTION agent_store.advance_action_frontier",
            "UNIQUE INDEX IF NOT EXISTS runs_one_active_per_session_idx",
            "'recovery', jsonb_build_object(",
            "principal_id text NOT NULL",
            "'episodes', COALESCE((",
            "'state_checkpoint', (",
            "action_frontier_hash text NOT NULL",
            "krw-agent-action-frontier/v1|",
        ] {
            assert!(
                INITIAL_MIGRATION_SQL.contains(required),
                "missing {required}"
            );
        }
        assert!(!INITIAL_MIGRATION_SQL.contains("answer_bundle_hash text NOT NULL UNIQUE"));
    }

    #[test]
    fn action_finalization_migration_replaces_legacy_commit_and_is_deny_monotone() {
        for required in [
            "CREATE OR REPLACE FUNCTION agent_v1.finalize_action",
            "validation_receipt_hash",
            "policy_receipt_hash",
            "stage IN ('accepted', 'rejected')",
            "action_finalization_conflict",
            "DROP FUNCTION IF EXISTS agent_v1.commit_action(jsonb)",
            "CREATE OR REPLACE FUNCTION agent_v1.mark_action_ambiguous",
        ] {
            assert!(
                ACTION_FINALIZATION_MIGRATION_SQL.contains(required),
                "missing {required}"
            );
        }
        assert!(
            !ACTION_FINALIZATION_MIGRATION_SQL
                .contains("CREATE OR REPLACE FUNCTION agent_v1.commit_action")
        );
    }

    #[test]
    fn session_memory_migration_is_v3_append_only_atomic_and_execute_only() {
        for required in [
            "CREATE TABLE agent_store.session_memory_frontiers",
            "CREATE TABLE agent_store.session_memory_deltas",
            "UNIQUE (source_run_id)",
            "CREATE OR REPLACE FUNCTION agent_v1.commit_final",
            "CREATE FUNCTION agent_v1.read_session_memory",
            "session_memory_delta_is_append_only",
            "session_memory_frontier_conflict",
            "session_memory_next_frontier_mismatch",
            "session_memory_owner_mismatch",
            "p_request->'session_memory_delta'->>'schema_version' IS DISTINCT FROM '3'",
            "p_request->'session_memory_delta'->'source'->>'revision'",
            "final_commit_intent_hash",
            "resolved_goals",
            "memory_revision",
            "memory_frontier_hash",
            "LIMIT v_limit",
            "PRIMARY KEY (tenant_id, principal_id, session_id)",
            "REVOKE ALL ON TABLE agent_store.session_memory_deltas FROM PUBLIC",
            "REVOKE ALL ON FUNCTION agent_v1.read_session_memory(jsonb) FROM PUBLIC",
        ] {
            assert!(
                SESSION_MEMORY_MIGRATION_SQL.contains(required),
                "missing session-memory contract: {required}"
            );
        }
        assert!(
            SESSION_MEMORY_MIGRATION_SQL.contains(
                "sha256:a4e1b26a1a1e27b135aa73cb802316af633c59cc50e92358bca73d6c4e6a4fbb"
            )
        );
        assert!(!SESSION_MEMORY_MIGRATION_SQL.contains("krw.session-memory/empty-v1"));
        assert!(
            !SESSION_MEMORY_MIGRATION_SQL.contains(
                "p_request->'session_memory_delta'->>'schema_version' IS DISTINCT FROM '1'"
            )
        );
        let committed_event_start = SESSION_MEMORY_MIGRATION_SQL
            .find("'answer.committed'")
            .expect("answer.committed outbox insert");
        let committed_event_end = SESSION_MEMORY_MIGRATION_SQL[committed_event_start..]
            .find("FOR v_entry")
            .map(|offset| committed_event_start + offset)
            .expect("custom outbox loop");
        let committed_event =
            &SESSION_MEMORY_MIGRATION_SQL[committed_event_start..committed_event_end];
        assert!(!committed_event.contains("session_memory_delta"));
    }

    #[test]
    fn embedded_migration_contains_principal_fair_queue_contracts() {
        for required in [
            "TABLE IF NOT EXISTS agent_store.fair_queue_clock",
            "TABLE IF NOT EXISTS agent_store.principal_fair_queue",
            "PRIMARY KEY (tenant_id, principal_id)",
            "queue_start_tag bigint NOT NULL",
            "queue_finish_tag bigint NOT NULL",
            "CHECK (queue_finish_tag = queue_start_tag + 1)",
            "FUNCTION agent_store.allocate_fair_queue_tag",
            "GREATEST(v_virtual_start_tag, v_tail_finish_tag)",
            "ERRCODE='K1022',MESSAGE='fair_queue_tag_overflow'",
            "INDEX IF NOT EXISTS runs_expired_active_claim_idx",
            "INDEX IF NOT EXISTS runs_fair_claim_order_idx",
            "INDEX IF NOT EXISTS runs_fair_claim_available_idx",
            "ORDER BY r.lease_deadline, r.created_at, r.run_id",
            "ORDER BY r.queue_start_tag, r.queue_finish_tag,",
            "SET virtual_start_tag = GREATEST(virtual_start_tag, v_run.queue_start_tag)",
            "queue_start_tag=v_queue_start_tag,queue_finish_tag=v_queue_finish_tag",
            "runs_one_active_per_session_idx",
            "ON agent_store.runs (tenant_id, session_id)",
        ] {
            assert!(
                INITIAL_MIGRATION_SQL.contains(required),
                "missing fair-queue contract: {required}"
            );
        }
        assert!(
            INITIAL_MIGRATION_SQL
                .match_indices("agent_store.allocate_fair_queue_tag(")
                .count()
                >= 3,
            "enqueue and deferral must both allocate a transactionally fresh tag"
        );
        assert!(!INITIAL_MIGRATION_SQL.contains("r.priority DESC"));
        assert_eq!(
            RejectionKind::from_sqlstate("K1022"),
            Some(RejectionKind::FairQueueTagOverflow)
        );
    }

    #[test]
    fn session_memory_retention_uses_hash_tombstones_and_trusted_retirement() {
        for required in [
            "CREATE TABLE agent_store.session_memory_sources",
            "session_memory_frontiers_source_tombstone_fk",
            "session_memory_deltas_source_tombstone_fk",
            "session_memory_snapshots_checkpoint_tombstone_fk",
            "CREATE FUNCTION agent_v1.retire_session_memory",
            "product_hard_purge",
            "session_memory_retirement_busy",
            "retirement_mutation_conflict",
            "session_memory_maintenance",
            "ON DELETE RESTRICT",
            "GRANT EXECUTE ON FUNCTION agent_v1.retire_session_memory(jsonb)",
        ] {
            assert!(
                SESSION_MEMORY_RETENTION_MIGRATION_SQL.contains(required),
                "missing session-memory retention contract: {required}"
            );
        }
        assert!(!SESSION_MEMORY_RETENTION_MIGRATION_SQL.contains("DROP SCHEMA"));
    }

    #[test]
    fn lifecycle_outbox_migration_is_content_free_and_non_blocking() {
        for required in [
            "agent_store.enqueue_lifecycle_event",
            "run.started",
            "run.progress",
            "trg_agent_store_lifecycle_started",
            "trg_agent_store_lifecycle_researching",
            "lifecycle_stage",
            "WHEN OTHERS THEN",
            "ON CONFLICT (event_kind, dedupe_key) DO NOTHING",
        ] {
            assert!(
                LIFECYCLE_OUTBOX_MIGRATION_SQL.contains(required),
                "missing lifecycle contract: {required}"
            );
        }
        assert!(!LIFECYCLE_OUTBOX_MIGRATION_SQL.contains("provider_content"));
        assert!(!LIFECYCLE_OUTBOX_MIGRATION_SQL.contains("prompt"));
    }

    #[test]
    fn daemon_mcp_readiness_migration_is_explicit_and_fail_closed() {
        for required in [
            "ADD COLUMN IF NOT EXISTS mcp_ready boolean NOT NULL DEFAULT false",
            "'heartbeat_ttl_ms','mcp_ready'",
            "agent_store.request_boolean(p_request, 'mcp_ready')",
            "mcp_ready = EXCLUDED.mcp_ready",
            "'mcp_ready', agent_store.request_boolean(p_request, 'mcp_ready')",
        ] {
            assert!(
                DAEMON_MCP_READINESS_MIGRATION_SQL.contains(required),
                "missing daemon MCP readiness contract: {required}"
            );
        }
        assert!(!DAEMON_MCP_READINESS_MIGRATION_SQL.contains("DEFAULT true"));
    }
}
