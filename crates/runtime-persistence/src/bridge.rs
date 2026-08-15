use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use krw_agent_artifact_store::{ArtifactKind, ArtifactScope};
use krw_agent_bounded_child::{
    CancelChildMutation, ChildExecutionReceipt, CompleteChildMutation, InvokeChildMutation,
    ReserveChildMutation,
};
use krw_agent_persistence::agent_v1::{
    ActionAbiReceipt, AgentV1Client, AgentV1Error, BeginActionRequest,
    CheckpointChildExecutionRequest, CheckpointEpisodeRequest, CheckpointRunStateRequest,
    CheckpointRunStateResponse, CheckpointSessionMemorySnapshotRequest,
    CheckpointSessionMemorySnapshotResponse, ChildExecutionAbiReceipt, FinalCommitRequest,
    FinalCommitResponse, FinalizeActionRequest, JsonProcedureExecutor, MarkActionAmbiguousRequest,
    MutationReceipt, ObserveActionRequest, ReadChildExecutionRequest, ReadChildExecutionResponse,
    ReadCommittedOutcomeRequest, ReadCommittedOutcomeResponse, ReadSessionMemoryRequest,
    ReadSessionMemoryResponse, RejectionKind, SessionMemoryReadMode,
};
use krw_agent_persistence::daemon::{
    LeaseCoordinator, RecoveryArtifactKind, RecoveryArtifactRequest, RecoveryArtifactStore,
    VerifiedRecoveryArtifacts,
};
use krw_agent_persistence::{
    ActionFinalizationReceipt, ActionReceipt, ActionStage, FinalizeActionMutation,
};
use krw_agent_protocol::{ALLOWED_MODEL_IDS, BudgetUsage, ContentHash};
use krw_agent_execution_contracts::{
    ActionIntent, DeliveryCertainty, DependencyFailure, DurableActionObservation, DurableEpisode,
    DurableFinal, DurableRecoverySnapshot, DurableRunState, FinalStatus, MarkActionAmbiguous,
    Persistence, RecoveredAction, RecoveredEpisode, RecoveredStateCheckpoint, RecoverySnapshot,
    RunControl, RunIdentity, RunLifecycleStage,
};
use krw_session_memory::{
    MAX_SESSION_MEMORY_SNAPSHOT_BYTES, SessionMemoryDeltaV3, SessionMemorySnapshotV3,
    empty_frontier_hash, empty_source_lineage_hash, next_source_lineage_hash,
};
use serde_json::{Value, json};
use thiserror::Error;

use crate::{ArtifactReceipt, ArtifactRepository, ArtifactRepositoryError};

#[async_trait]
pub trait DurableRunStore: fmt::Debug + Send + Sync {
    async fn checkpoint_episode(
        &self,
        request: &CheckpointEpisodeRequest,
    ) -> Result<MutationReceipt, AgentV1Error>;
    async fn checkpoint_run_state(
        &self,
        request: &CheckpointRunStateRequest,
    ) -> Result<CheckpointRunStateResponse, AgentV1Error>;
    async fn begin_action(
        &self,
        request: &BeginActionRequest,
    ) -> Result<ActionAbiReceipt, AgentV1Error>;
    async fn observe_action(
        &self,
        request: &ObserveActionRequest,
    ) -> Result<ActionAbiReceipt, AgentV1Error>;
    async fn finalize_action(
        &self,
        request: &FinalizeActionRequest,
    ) -> Result<ActionAbiReceipt, AgentV1Error>;
    async fn mark_action_ambiguous(
        &self,
        request: &MarkActionAmbiguousRequest,
    ) -> Result<ActionAbiReceipt, AgentV1Error>;
    async fn checkpoint_child_execution(
        &self,
        request: &CheckpointChildExecutionRequest,
    ) -> Result<ChildExecutionAbiReceipt, AgentV1Error>;
    async fn read_child_execution(
        &self,
        request: &ReadChildExecutionRequest,
    ) -> Result<ReadChildExecutionResponse, AgentV1Error>;
    async fn read_session_memory(
        &self,
        request: &ReadSessionMemoryRequest,
    ) -> Result<ReadSessionMemoryResponse, AgentV1Error>;
    async fn checkpoint_session_memory_snapshot(
        &self,
        request: &CheckpointSessionMemorySnapshotRequest,
    ) -> Result<CheckpointSessionMemorySnapshotResponse, AgentV1Error>;
    async fn commit_final(
        &self,
        request: &FinalCommitRequest,
    ) -> Result<FinalCommitResponse, AgentV1Error>;
    async fn read_committed_outcome(
        &self,
        request: &ReadCommittedOutcomeRequest,
    ) -> Result<ReadCommittedOutcomeResponse, AgentV1Error>;
}

#[async_trait]
impl<E> DurableRunStore for AgentV1Client<E>
where
    E: JsonProcedureExecutor,
{
    async fn checkpoint_episode(
        &self,
        request: &CheckpointEpisodeRequest,
    ) -> Result<MutationReceipt, AgentV1Error> {
        self.execute(request).await
    }

    async fn checkpoint_run_state(
        &self,
        request: &CheckpointRunStateRequest,
    ) -> Result<CheckpointRunStateResponse, AgentV1Error> {
        self.execute(request).await
    }

    async fn begin_action(
        &self,
        request: &BeginActionRequest,
    ) -> Result<ActionAbiReceipt, AgentV1Error> {
        self.execute(request).await
    }

    async fn observe_action(
        &self,
        request: &ObserveActionRequest,
    ) -> Result<ActionAbiReceipt, AgentV1Error> {
        self.execute(request).await
    }

    async fn finalize_action(
        &self,
        request: &FinalizeActionRequest,
    ) -> Result<ActionAbiReceipt, AgentV1Error> {
        self.execute(request).await
    }

    async fn mark_action_ambiguous(
        &self,
        request: &MarkActionAmbiguousRequest,
    ) -> Result<ActionAbiReceipt, AgentV1Error> {
        self.execute(request).await
    }

    async fn checkpoint_child_execution(
        &self,
        request: &CheckpointChildExecutionRequest,
    ) -> Result<ChildExecutionAbiReceipt, AgentV1Error> {
        self.execute(request).await
    }

    async fn read_child_execution(
        &self,
        request: &ReadChildExecutionRequest,
    ) -> Result<ReadChildExecutionResponse, AgentV1Error> {
        self.execute(request).await
    }

    async fn read_session_memory(
        &self,
        request: &ReadSessionMemoryRequest,
    ) -> Result<ReadSessionMemoryResponse, AgentV1Error> {
        self.execute(request).await
    }

    async fn checkpoint_session_memory_snapshot(
        &self,
        request: &CheckpointSessionMemorySnapshotRequest,
    ) -> Result<CheckpointSessionMemorySnapshotResponse, AgentV1Error> {
        self.execute(request).await
    }

    async fn commit_final(
        &self,
        request: &FinalCommitRequest,
    ) -> Result<FinalCommitResponse, AgentV1Error> {
        self.execute(request).await
    }

    async fn read_committed_outcome(
        &self,
        request: &ReadCommittedOutcomeRequest,
    ) -> Result<ReadCommittedOutcomeResponse, AgentV1Error> {
        self.execute(request).await
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinalizationPolicy {
    pub emit_presentation_outbox: bool,
    pub emit_billing_outbox: bool,
}

impl Default for FinalizationPolicy {
    fn default() -> Self {
        Self {
            emit_presentation_outbox: true,
            emit_billing_outbox: true,
        }
    }
}

#[derive(Debug, Error)]
pub enum RuntimePersistenceError {
    #[error("run mutation identity does not match the claimed lease")]
    MutationIdentityMismatch,
    #[error("durable artifact receipt does not match the mutation hash")]
    ArtifactHashMismatch,
    #[error("agent_v1 returned an invalid bounded receipt")]
    InvalidAbiReceipt,
    #[error("run lease is unavailable")]
    LeaseUnavailable,
    #[error("runtime persistence lock was poisoned")]
    LockPoisoned,
    #[error("artifact repository failed: {0}")]
    Artifact(#[from] ArtifactRepositoryError),
    #[error("agent_v1 failed: {0}")]
    Abi(#[from] AgentV1Error),
    #[error("runtime persistence JSON encoding failed")]
    Json,
}

pub struct DurableRunPersistence {
    store: Arc<dyn DurableRunStore>,
    artifacts: ArtifactRepository,
    scope: ArtifactScope,
    receipt: Arc<krw_agent_persistence::agent_v1::ClaimReceipt>,
    lease: LeaseCoordinator,
    recovery: Arc<VerifiedRecoveryArtifacts>,
    current_arguments: Mutex<BTreeMap<String, ArtifactReceipt>>,
    current_results: Mutex<BTreeMap<String, ArtifactReceipt>>,
    finalization: FinalizationPolicy,
}

impl DurableRunPersistence {
    pub fn new(
        store: Arc<dyn DurableRunStore>,
        artifacts: ArtifactRepository,
        receipt: Arc<krw_agent_persistence::agent_v1::ClaimReceipt>,
        lease: LeaseCoordinator,
        recovery: Arc<VerifiedRecoveryArtifacts>,
        finalization: FinalizationPolicy,
    ) -> Result<Self, RuntimePersistenceError> {
        let scope = ArtifactScope::new(
            receipt.tenant_id.clone(),
            receipt.principal_id.clone(),
            receipt.run_id.clone(),
        )
        .map_err(ArtifactRepositoryError::Store)?;
        Ok(Self {
            store,
            artifacts,
            scope,
            receipt,
            lease,
            recovery,
            current_arguments: Mutex::new(BTreeMap::new()),
            current_results: Mutex::new(BTreeMap::new()),
            finalization,
        })
    }

    pub async fn resolve_final_ambiguity(
        &self,
        expected_hash: &ContentHash,
    ) -> Result<Option<FinalStatus>, DependencyFailure> {
        let outcome = self.read_outcome().await?;
        interpret_committed_outcome(&outcome, expected_hash)
    }

    /// Read one bounded, owner-scoped page from the immutable session-memory
    /// snapshot visible to this claimed run. The per-run lane prevents a local
    /// renewal or terminal mutation from racing the fence/version receipt.
    pub async fn read_session_memory(
        &self,
        after_revision: u64,
        limit: u16,
        mode: SessionMemoryReadMode,
    ) -> Result<ReadSessionMemoryResponse, DependencyFailure> {
        let guard = self.lease.mutation_guard().await.map_err(|_| {
            dependency_failure(
                "lease",
                RuntimePersistenceError::LeaseUnavailable,
                true,
                DeliveryCertainty::NotDispatched,
            )
        })?;
        let snapshot = guard.snapshot();
        let response = self
            .store
            .read_session_memory(&ReadSessionMemoryRequest {
                run_id: self.receipt.run_id.clone(),
                tenant_id: self.receipt.tenant_id.clone(),
                principal_id: self.receipt.principal_id.clone(),
                session_id: self.receipt.session_id.clone(),
                fencing_token: snapshot.fencing_token,
                after_revision,
                limit,
                mode,
            })
            .await
            .map_err(|error| {
                if mode == SessionMemoryReadMode::SnapshotTail
                    && matches!(
                        &error,
                        AgentV1Error::Rejected {
                            kind: RejectionKind::SessionMemoryConflict,
                            ..
                        }
                    )
                {
                    return dependency_failure(
                        "session_memory_rebuild_required",
                        error,
                        false,
                        DeliveryCertainty::NotDispatched,
                    );
                }
                map_abi_failure("read_session_memory", &error, false)
            })?;
        validate_session_memory_page(
            &response,
            &SessionMemoryPageExpectation {
                run_id: &self.receipt.run_id,
                session_id: &self.receipt.session_id,
                fencing_token: snapshot.fencing_token,
                run_version: snapshot.run_version,
                after_revision,
                limit,
                mode,
            },
        )
        .map_err(|error| {
            dependency_failure(
                "read_session_memory_receipt",
                error,
                false,
                DeliveryCertainty::NotDispatched,
            )
        })?;
        Ok(response)
    }

    pub async fn checkpoint_session_memory_snapshot(
        &self,
        memory_snapshot: &SessionMemorySnapshotV3,
    ) -> Result<CheckpointSessionMemorySnapshotResponse, DependencyFailure> {
        let canonical = memory_snapshot.canonical_bytes().map_err(|error| {
            dependency_failure(
                "session_memory_snapshot_encoding",
                error,
                false,
                DeliveryCertainty::NotDispatched,
            )
        })?;
        let snapshot_hash = ContentHash::sha256(&canonical);
        let snapshot_size_bytes = u64::try_from(canonical.len()).map_err(|error| {
            dependency_failure(
                "session_memory_snapshot_size",
                error,
                false,
                DeliveryCertainty::NotDispatched,
            )
        })?;
        let snapshot_value = serde_json::to_value(memory_snapshot).map_err(|_| {
            dependency_failure(
                "session_memory_snapshot_encoding",
                RuntimePersistenceError::Json,
                false,
                DeliveryCertainty::NotDispatched,
            )
        })?;
        let mut guard = self.lease.mutation_guard().await.map_err(|_| {
            dependency_failure(
                "lease",
                RuntimePersistenceError::LeaseUnavailable,
                true,
                DeliveryCertainty::NotDispatched,
            )
        })?;
        let lease_snapshot = guard.snapshot();
        let response = self
            .store
            .checkpoint_session_memory_snapshot(&CheckpointSessionMemorySnapshotRequest {
                mutation_id: format!("session-memory-snapshot:{snapshot_hash}"),
                run_id: self.receipt.run_id.clone(),
                tenant_id: self.receipt.tenant_id.clone(),
                principal_id: self.receipt.principal_id.clone(),
                session_id: self.receipt.session_id.clone(),
                fencing_token: lease_snapshot.fencing_token,
                expected_run_version: lease_snapshot.run_version,
                snapshot_revision: memory_snapshot.revision,
                frontier_hash: memory_snapshot.frontier_hash.clone(),
                source_lineage_hash: memory_snapshot.source_lineage_hash.clone(),
                snapshot_hash: snapshot_hash.clone(),
                snapshot_size_bytes,
                snapshot: snapshot_value,
            })
            .await
            .map_err(|error| map_abi_failure("checkpoint_session_memory_snapshot", &error, true))?;
        if response.run_id != self.receipt.run_id
            || response.fencing_token != lease_snapshot.fencing_token
            || response.run_version < lease_snapshot.run_version
            || response.snapshot_revision != memory_snapshot.revision
            || response.frontier_hash != memory_snapshot.frontier_hash
            || response.source_lineage_hash != memory_snapshot.source_lineage_hash
            || response.snapshot_hash != snapshot_hash
            || !matches!(
                response.outcome.as_str(),
                "snapshot_checkpointed" | "already_checkpointed"
            )
        {
            return Err(dependency_failure(
                "checkpoint_session_memory_snapshot_receipt",
                RuntimePersistenceError::InvalidAbiReceipt,
                false,
                DeliveryCertainty::NotDispatched,
            ));
        }
        guard.observe_version(response.run_version, None, None);
        Ok(response)
    }

    /// Finalize raw bytes that were already made durable by `observe_action`.
    /// The database linearizes the disposition and both deterministic receipt
    /// hashes; a rejected action can never be promoted on replay.
    pub async fn finalize_action(
        &self,
        mutation: FinalizeActionMutation,
    ) -> Result<ActionFinalizationReceipt, DependencyFailure> {
        self.validate_identity(&mutation.run_id, mutation.fencing_token)
            .map_err(|error| {
                dependency_failure(
                    "finalize_action_identity",
                    error,
                    false,
                    DeliveryCertainty::NotDispatched,
                )
            })?;
        let mut guard = self.lease.mutation_guard().await.map_err(|_| {
            dependency_failure(
                "lease",
                RuntimePersistenceError::LeaseUnavailable,
                true,
                DeliveryCertainty::NotDispatched,
            )
        })?;
        let snapshot = guard.snapshot();
        let response = self
            .store
            .finalize_action(&FinalizeActionRequest {
                mutation_id: mutation.mutation_id.clone(),
                run_id: self.receipt.run_id.clone(),
                tenant_id: self.receipt.tenant_id.clone(),
                fencing_token: snapshot.fencing_token,
                expected_run_version: snapshot.run_version,
                action_key: mutation.action_key,
                result_hash: mutation.result_hash,
                disposition: mutation.disposition,
                validation_receipt_hash: mutation.validation_receipt_hash.clone(),
                policy_receipt_hash: mutation.policy_receipt_hash.clone(),
            })
            .await
            .map_err(|error| map_abi_failure("finalize_action", &error, true))?;
        validate_action_abi(&response, &self.receipt.run_id, snapshot.fencing_token).map_err(
            |error| {
                dependency_failure(
                    "finalize_action_receipt",
                    error,
                    false,
                    DeliveryCertainty::NotDispatched,
                )
            },
        )?;
        if response.disposition != Some(mutation.disposition)
            || response.stage != action_stage_name(mutation.disposition.stage())
            || response.validation_receipt_hash.as_ref() != Some(&mutation.validation_receipt_hash)
            || response.policy_receipt_hash.as_ref() != Some(&mutation.policy_receipt_hash)
        {
            return Err(dependency_failure(
                "finalize_action_contract",
                RuntimePersistenceError::InvalidAbiReceipt,
                false,
                DeliveryCertainty::NotDispatched,
            ));
        }
        guard.observe_action(&response);
        to_action_finalization_receipt(response, mutation.mutation_id).map_err(|error| {
            dependency_failure(
                "finalize_action_stage",
                error,
                false,
                DeliveryCertainty::NotDispatched,
            )
        })
    }

    async fn read_outcome(&self) -> Result<ReadCommittedOutcomeResponse, DependencyFailure> {
        let response = self
            .store
            .read_committed_outcome(&ReadCommittedOutcomeRequest {
                run_id: self.receipt.run_id.clone(),
                tenant_id: self.receipt.tenant_id.clone(),
            })
            .await
            .map_err(|error| map_abi_failure("read_outcome", &error, false))?;
        if response.run_id != self.receipt.run_id {
            return Err(dependency_failure(
                "read_outcome_receipt",
                RuntimePersistenceError::InvalidAbiReceipt,
                false,
                DeliveryCertainty::NotDispatched,
            ));
        }
        Ok(response)
    }

    fn validate_identity(
        &self,
        run_id: &str,
        fencing_token: u64,
    ) -> Result<(), RuntimePersistenceError> {
        if run_id != self.receipt.run_id || fencing_token != self.receipt.fencing_token {
            return Err(RuntimePersistenceError::MutationIdentityMismatch);
        }
        Ok(())
    }

    async fn load_current_result(
        &self,
        action_key: &str,
        expected_hash: &ContentHash,
    ) -> Result<Option<Vec<u8>>, DependencyFailure> {
        let receipt = self
            .current_results
            .lock()
            .map_err(|_| {
                dependency_failure(
                    "result_receipt_lock",
                    RuntimePersistenceError::LockPoisoned,
                    true,
                    DeliveryCertainty::NotDispatched,
                )
            })?
            .get(action_key)
            .cloned();
        let Some(receipt) = receipt else {
            return Ok(None);
        };
        if &receipt.plaintext_hash != expected_hash {
            return Err(dependency_failure(
                "result_receipt_hash",
                RuntimePersistenceError::ArtifactHashMismatch,
                false,
                DeliveryCertainty::NotDispatched,
            ));
        }
        let request = RecoveryArtifactRequest {
            kind: RecoveryArtifactKind::ActionResult,
            run_id: self.receipt.run_id.clone(),
            tenant_id: self.receipt.tenant_id.clone(),
            principal_id: self.receipt.principal_id.clone(),
            action_key: Some(action_key.to_owned()),
            artifact_ref: receipt.artifact_ref,
            expected_hash: receipt.plaintext_hash,
            expected_size_bytes: usize::try_from(receipt.plaintext_size_bytes).ok(),
            schema_hash: None,
        };
        let bytes = self
            .artifacts
            .load_verified(&request, 16 * 1024 * 1024)
            .await
            .map_err(|error| {
                DependencyFailure::redacted(
                    "artifact_read",
                    format!("{error:?}"),
                    true,
                    DeliveryCertainty::NotDispatched,
                )
            })?;
        Ok(Some(bytes.to_vec()))
    }

    async fn action_arguments_reference(
        &self,
        intent: &ActionIntent,
    ) -> Result<String, DependencyFailure> {
        if let Some(existing) = self
            .receipt
            .recovery
            .actions
            .iter()
            .find(|action| action.action_key == intent.mutation.action_key)
        {
            if existing.request_hash != intent.mutation.request_hash
                || existing.episode_hash != intent.episode_hash
                || existing.tool_call_id != intent.tool_call_id
                || existing.capability_id != intent.capability_id
                || existing.input_schema_hash != intent.input_schema_hash
                || existing.output_schema_hash != intent.output_schema_hash
                || existing.data_release_hash != intent.data_release_hash
                || existing.retryable_read != intent.mutation.retryable_read
            {
                return Err(dependency_failure(
                    "recovered_action_identity",
                    RuntimePersistenceError::InvalidAbiReceipt,
                    false,
                    DeliveryCertainty::NotDispatched,
                ));
            }
            let mut matches = self.recovery.artifacts().iter().filter(|artifact| {
                artifact.kind == RecoveryArtifactKind::ActionArguments
                    && artifact.action_key.as_deref() == Some(&intent.mutation.action_key)
                    && artifact.expected_hash == intent.mutation.request_hash
            });
            let artifact = matches.next().ok_or_else(|| {
                dependency_failure(
                    "recovered_arguments_missing",
                    RuntimePersistenceError::InvalidAbiReceipt,
                    false,
                    DeliveryCertainty::NotDispatched,
                )
            })?;
            if matches.next().is_some() || artifact.bytes() != intent.canonical_arguments.as_slice()
            {
                return Err(dependency_failure(
                    "recovered_arguments_mismatch",
                    RuntimePersistenceError::ArtifactHashMismatch,
                    false,
                    DeliveryCertainty::NotDispatched,
                ));
            }
            return Ok(existing.arguments_artifact_ref.clone());
        }

        let current = self
            .current_arguments
            .lock()
            .map_err(|_| {
                dependency_failure(
                    "arguments_receipt_lock",
                    RuntimePersistenceError::LockPoisoned,
                    true,
                    DeliveryCertainty::NotDispatched,
                )
            })?
            .get(&intent.mutation.action_key)
            .cloned();
        if let Some(current) = current {
            if current.plaintext_hash != intent.mutation.request_hash {
                return Err(dependency_failure(
                    "arguments_receipt_hash",
                    RuntimePersistenceError::ArtifactHashMismatch,
                    false,
                    DeliveryCertainty::NotDispatched,
                ));
            }
            return Ok(current.artifact_ref);
        }

        let artifact = self
            .artifacts
            .put(
                &self.scope,
                ArtifactKind::ActionArguments,
                &intent.canonical_arguments,
            )
            .await
            .map_err(map_artifact_failure)?;
        if artifact.plaintext_hash != intent.mutation.request_hash {
            return Err(dependency_failure(
                "arguments_artifact_hash",
                RuntimePersistenceError::ArtifactHashMismatch,
                false,
                DeliveryCertainty::NotDispatched,
            ));
        }
        let reference = artifact.artifact_ref.clone();
        self.current_arguments
            .lock()
            .map_err(|_| {
                dependency_failure(
                    "arguments_receipt_lock",
                    RuntimePersistenceError::LockPoisoned,
                    true,
                    DeliveryCertainty::NotDispatched,
                )
            })?
            .insert(intent.mutation.action_key.clone(), artifact);
        Ok(reference)
    }

    async fn advance_child_execution(
        &self,
        mutation_id: &str,
        expected_cancel_generation: u64,
        transition: impl FnOnce(
            Option<&ChildExecutionReceipt>,
        ) -> Result<
            ChildExecutionReceipt,
            krw_agent_bounded_child::ChildExecutionError,
        >,
    ) -> Result<ChildExecutionReceipt, DependencyFailure> {
        let mut guard = self.lease.mutation_guard().await.map_err(|_| {
            dependency_failure(
                "lease",
                RuntimePersistenceError::LeaseUnavailable,
                true,
                DeliveryCertainty::NotDispatched,
            )
        })?;
        let snapshot = guard.snapshot();
        let current = self
            .store
            .read_child_execution(&ReadChildExecutionRequest {
                run_id: self.receipt.run_id.clone(),
                tenant_id: self.receipt.tenant_id.clone(),
                fencing_token: snapshot.fencing_token,
            })
            .await
            .map_err(|error| map_abi_failure("read_child_execution", &error, false))?;
        if current.run_id != self.receipt.run_id
            || current.fencing_token != snapshot.fencing_token
            || current.cancel_generation != expected_cancel_generation
            || current.run_version < snapshot.run_version
        {
            return Err(dependency_failure(
                "read_child_execution_receipt",
                RuntimePersistenceError::InvalidAbiReceipt,
                false,
                DeliveryCertainty::NotDispatched,
            ));
        }
        let current_receipt = current
            .child
            .as_ref()
            .map(validate_child_abi)
            .transpose()
            .map_err(|error| {
                dependency_failure(
                    "read_child_execution_contract",
                    error,
                    false,
                    DeliveryCertainty::NotDispatched,
                )
            })?;
        guard.observe_version(current.run_version, Some(current.cancel_generation), None);
        let next = transition(current_receipt.as_ref()).map_err(|error| {
            dependency_failure(
                "bounded_child_transition",
                error,
                false,
                DeliveryCertainty::NotDispatched,
            )
        })?;
        next.validate().map_err(|error| {
            dependency_failure(
                "bounded_child_receipt",
                error,
                false,
                DeliveryCertainty::NotDispatched,
            )
        })?;
        if current_receipt.as_ref() == Some(&next) {
            return Ok(next);
        }
        let expected_receipt_hash = current_receipt
            .as_ref()
            .map(ChildExecutionReceipt::receipt_hash)
            .transpose()
            .map_err(|error| {
                dependency_failure(
                    "bounded_child_current_hash",
                    error,
                    false,
                    DeliveryCertainty::NotDispatched,
                )
            })?;
        let receipt_hash = next.receipt_hash().map_err(|error| {
            dependency_failure(
                "bounded_child_next_hash",
                error,
                false,
                DeliveryCertainty::NotDispatched,
            )
        })?;
        let response = self
            .store
            .checkpoint_child_execution(&CheckpointChildExecutionRequest {
                mutation_id: mutation_id.to_owned(),
                run_id: self.receipt.run_id.clone(),
                tenant_id: self.receipt.tenant_id.clone(),
                fencing_token: snapshot.fencing_token,
                expected_run_version: current.run_version,
                expected_cancel_generation,
                child_id: next.child_id.clone(),
                expected_receipt_hash,
                target_stage: next.stage,
                receipt_hash: receipt_hash.clone(),
                receipt: next.clone(),
            })
            .await
            .map_err(|error| map_abi_failure("checkpoint_child_execution", &error, true))?;
        let verified = validate_child_abi(&response).map_err(|error| {
            dependency_failure(
                "checkpoint_child_execution_receipt",
                error,
                false,
                DeliveryCertainty::NotDispatched,
            )
        })?;
        if response.run_id != self.receipt.run_id
            || response.fencing_token != snapshot.fencing_token
            || response.receipt_hash != receipt_hash
            || verified != next
        {
            return Err(dependency_failure(
                "checkpoint_child_execution_contract",
                RuntimePersistenceError::InvalidAbiReceipt,
                false,
                DeliveryCertainty::NotDispatched,
            ));
        }
        guard.observe_version(response.run_version, None, None);
        Ok(verified)
    }
}

impl fmt::Debug for DurableRunPersistence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DurableRunPersistence")
            .field("run_id_hash", &ContentHash::sha256(&self.receipt.run_id))
            .field("fencing_token", &self.receipt.fencing_token)
            .field("artifacts", &self.artifacts)
            .field("recovery", &self.recovery)
            .field("finalization", &self.finalization)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl Persistence for DurableRunPersistence {
    async fn load_recovery(
        &self,
        run: &RunIdentity,
    ) -> Result<RecoverySnapshot, DependencyFailure> {
        self.validate_identity(&run.run_id, run.fencing_token)
            .map_err(|error| {
                dependency_failure(
                    "recovery_identity",
                    error,
                    false,
                    DeliveryCertainty::NotDispatched,
                )
            })?;
        if run.tenant_id != self.receipt.tenant_id
            || run.expected_cancel_generation != self.receipt.cancel_generation
        {
            return Err(dependency_failure(
                "recovery_scope",
                RuntimePersistenceError::MutationIdentityMismatch,
                false,
                DeliveryCertainty::NotDispatched,
            ));
        }
        let child_response = self
            .store
            .read_child_execution(&ReadChildExecutionRequest {
                run_id: self.receipt.run_id.clone(),
                tenant_id: self.receipt.tenant_id.clone(),
                fencing_token: self.receipt.fencing_token,
            })
            .await
            .map_err(|error| map_abi_failure("read_child_execution", &error, false))?;
        if child_response.run_id != self.receipt.run_id
            || child_response.fencing_token != self.receipt.fencing_token
            || child_response.cancel_generation != self.receipt.cancel_generation
        {
            return Err(dependency_failure(
                "recovery_child_identity",
                RuntimePersistenceError::InvalidAbiReceipt,
                false,
                DeliveryCertainty::NotDispatched,
            ));
        }
        let child = child_response
            .child
            .as_ref()
            .map(validate_child_abi)
            .transpose()
            .map_err(|error| {
                dependency_failure(
                    "recovery_child_receipt",
                    error,
                    false,
                    DeliveryCertainty::NotDispatched,
                )
            })?;
        if self.receipt.recovery.episodes.is_empty()
            && self.receipt.recovery.actions.is_empty()
            && self.receipt.recovery.state_checkpoint.is_none()
            && child.is_none()
        {
            return Ok(RecoverySnapshot::Fresh);
        }
        let state = self
            .receipt
            .recovery
            .state_checkpoint
            .as_ref()
            .map(|receipt| {
                let mut matches = self.recovery.artifacts().iter().filter(|artifact| {
                    artifact.kind == RecoveryArtifactKind::RuntimeState
                        && artifact.expected_hash == receipt.state_hash
                        && artifact.schema_hash.as_ref() == Some(&receipt.recovery_schema_hash)
                });
                let artifact = matches
                    .next()
                    .ok_or(RuntimePersistenceError::InvalidAbiReceipt)?;
                if matches.next().is_some() {
                    return Err(RuntimePersistenceError::InvalidAbiReceipt);
                }
                Ok(RecoveredStateCheckpoint {
                    recovery_schema_hash: receipt.recovery_schema_hash.clone(),
                    provider_checkpoint_seq: receipt.provider_checkpoint_seq,
                    action_frontier_seq: receipt.action_frontier_seq,
                    action_frontier_hash: receipt.action_frontier_hash.clone(),
                    state_hash: receipt.state_hash.clone(),
                    state_bytes: artifact.bytes().to_vec(),
                })
            })
            .transpose()
            .map_err(|error| {
                dependency_failure(
                    "recovery_state",
                    error,
                    false,
                    DeliveryCertainty::NotDispatched,
                )
            })?;
        let episodes = self
            .receipt
            .recovery
            .episodes
            .iter()
            .map(|receipt| {
                let mut matches = self.recovery.artifacts().iter().filter(|artifact| {
                    artifact.kind == RecoveryArtifactKind::ProviderEpisode
                        && artifact.expected_hash == receipt.episode_hash
                });
                let artifact = matches
                    .next()
                    .ok_or(RuntimePersistenceError::InvalidAbiReceipt)?;
                if matches.next().is_some() {
                    return Err(RuntimePersistenceError::InvalidAbiReceipt);
                }
                Ok(RecoveredEpisode {
                    checkpoint_seq: receipt.checkpoint_seq,
                    episode_hash: receipt.episode_hash.clone(),
                    episode_bytes: artifact.bytes().to_vec(),
                })
            })
            .collect::<Result<Vec<_>, RuntimePersistenceError>>()
            .map_err(|error| {
                dependency_failure(
                    "recovery_episodes",
                    error,
                    false,
                    DeliveryCertainty::NotDispatched,
                )
            })?;
        let actions = self
            .receipt
            .recovery
            .actions
            .iter()
            .map(|receipt| {
                let action_stage = parse_action_stage(&receipt.stage)?;
                let result_bytes = match &receipt.result_hash {
                    Some(result_hash) => {
                        let mut matches = self.recovery.artifacts().iter().filter(|artifact| {
                            artifact.kind == RecoveryArtifactKind::ActionResult
                                && artifact.action_key.as_deref() == Some(&receipt.action_key)
                                && artifact.expected_hash == *result_hash
                        });
                        let artifact = matches
                            .next()
                            .ok_or(RuntimePersistenceError::InvalidAbiReceipt)?;
                        if matches.next().is_some() {
                            return Err(RuntimePersistenceError::InvalidAbiReceipt);
                        }
                        Some(artifact.bytes().to_vec())
                    }
                    None => None,
                };
                Ok(RecoveredAction {
                    action_key: receipt.action_key.clone(),
                    request_hash: receipt.request_hash.clone(),
                    episode_hash: receipt.episode_hash.clone(),
                    tool_call_id: receipt.tool_call_id.clone(),
                    capability_id: receipt.capability_id.clone(),
                    input_schema_hash: receipt.input_schema_hash.clone(),
                    output_schema_hash: receipt.output_schema_hash.clone(),
                    data_release_hash: receipt.data_release_hash.clone(),
                    retryable_read: receipt.retryable_read,
                    stage: action_stage,
                    result_hash: receipt.result_hash.clone(),
                    result_bytes,
                })
            })
            .collect::<Result<Vec<_>, RuntimePersistenceError>>()
            .map_err(|error| {
                dependency_failure(
                    "recovery_actions",
                    error,
                    false,
                    DeliveryCertainty::NotDispatched,
                )
            })?;
        Ok(RecoverySnapshot::Durable(Box::new(
            DurableRecoverySnapshot {
                state,
                episodes,
                actions,
                child,
                current_provider_checkpoint_seq: self.receipt.checkpoint_seq,
                current_action_frontier_seq: self.receipt.action_frontier_seq,
                current_action_frontier_hash: self.receipt.action_frontier_hash.clone(),
            },
        )))
    }

    async fn inspect_run(&self, run: &RunIdentity) -> Result<RunControl, DependencyFailure> {
        if run.run_id != self.receipt.run_id || run.tenant_id != self.receipt.tenant_id {
            return Err(dependency_failure(
                "inspect_identity",
                RuntimePersistenceError::MutationIdentityMismatch,
                false,
                DeliveryCertainty::NotDispatched,
            ));
        }
        let response = self.read_outcome().await?;
        match response.state.as_str() {
            "active" => Ok(RunControl::Active {
                fencing_token: response.fencing_token,
                cancel_generation: response.cancel_generation,
            }),
            "cancelled" => Ok(RunControl::Cancelled),
            "final" | "failed" => Ok(RunControl::Finalized),
            _ => Err(DependencyFailure::redacted(
                "run_not_active",
                response.state,
                true,
                DeliveryCertainty::NotDispatched,
            )),
        }
    }

    async fn checkpoint_episode(&self, episode: &DurableEpisode) -> Result<(), DependencyFailure> {
        self.validate_identity(&episode.mutation.run_id, episode.mutation.fencing_token)
            .map_err(|error| {
                dependency_failure(
                    "episode_identity",
                    error,
                    false,
                    DeliveryCertainty::NotDispatched,
                )
            })?;
        if ContentHash::sha256(&episode.episode_bytes) != episode.mutation.episode_hash {
            return Err(dependency_failure(
                "episode_hash",
                RuntimePersistenceError::ArtifactHashMismatch,
                false,
                DeliveryCertainty::NotDispatched,
            ));
        }
        let artifact = self
            .artifacts
            .put(
                &self.scope,
                ArtifactKind::ProviderEpisode,
                &episode.episode_bytes,
            )
            .await
            .map_err(map_artifact_failure)?;
        if artifact.plaintext_hash != episode.mutation.episode_hash {
            return Err(dependency_failure(
                "episode_artifact_hash",
                RuntimePersistenceError::ArtifactHashMismatch,
                false,
                DeliveryCertainty::NotDispatched,
            ));
        }
        let mut guard = self.lease.mutation_guard().await.map_err(|_| {
            dependency_failure(
                "lease",
                RuntimePersistenceError::LeaseUnavailable,
                true,
                DeliveryCertainty::NotDispatched,
            )
        })?;
        let snapshot = guard.snapshot();
        let response = self
            .store
            .checkpoint_episode(&CheckpointEpisodeRequest {
                mutation_id: episode.mutation.mutation_id.clone(),
                run_id: self.receipt.run_id.clone(),
                tenant_id: self.receipt.tenant_id.clone(),
                fencing_token: snapshot.fencing_token,
                expected_run_version: snapshot.run_version,
                expected_checkpoint_seq: snapshot.checkpoint_seq,
                episode_hash: episode.mutation.episode_hash.clone(),
                episode_artifact_ref: artifact.artifact_ref,
            })
            .await
            .map_err(|error| map_abi_failure("checkpoint_episode", &error, true))?;
        validate_mutation_receipt(&response, &self.receipt.run_id, snapshot.fencing_token)
            .map_err(|error| {
                dependency_failure(
                    "checkpoint_episode_receipt",
                    error,
                    false,
                    DeliveryCertainty::NotDispatched,
                )
            })?;
        guard.observe_version(
            response.run_version,
            Some(response.cancel_generation),
            Some(response.checkpoint_seq),
        );
        Ok(())
    }

    async fn checkpoint_run_state(&self, state: &DurableRunState) -> Result<(), DependencyFailure> {
        self.validate_identity(&state.run_id, state.fencing_token)
            .map_err(|error| {
                dependency_failure(
                    "state_identity",
                    error,
                    false,
                    DeliveryCertainty::NotDispatched,
                )
            })?;
        if state.state_bytes.is_empty()
            || ContentHash::sha256(&state.state_bytes) != state.state_hash
        {
            return Err(dependency_failure(
                "state_hash",
                RuntimePersistenceError::ArtifactHashMismatch,
                false,
                DeliveryCertainty::NotDispatched,
            ));
        }
        let artifact = self
            .artifacts
            .put(&self.scope, ArtifactKind::RecoveryState, &state.state_bytes)
            .await
            .map_err(map_artifact_failure)?;
        if artifact.plaintext_hash != state.state_hash {
            return Err(dependency_failure(
                "state_artifact_hash",
                RuntimePersistenceError::ArtifactHashMismatch,
                false,
                DeliveryCertainty::NotDispatched,
            ));
        }
        let mut guard = self.lease.mutation_guard().await.map_err(|_| {
            dependency_failure(
                "lease",
                RuntimePersistenceError::LeaseUnavailable,
                true,
                DeliveryCertainty::NotDispatched,
            )
        })?;
        let snapshot = guard.snapshot();
        let mutation_id = ContentHash::sha256(format!(
            "checkpoint-run-state/v1\0{}\0{}\0{}\0{}\0{}",
            self.receipt.run_id,
            snapshot.fencing_token,
            snapshot.checkpoint_seq,
            snapshot.action_frontier_seq,
            state.state_hash
        ))
        .to_string();
        let response = self
            .store
            .checkpoint_run_state(&CheckpointRunStateRequest {
                mutation_id,
                run_id: self.receipt.run_id.clone(),
                tenant_id: self.receipt.tenant_id.clone(),
                fencing_token: snapshot.fencing_token,
                expected_run_version: snapshot.run_version,
                provider_checkpoint_seq: snapshot.checkpoint_seq,
                action_frontier_seq: snapshot.action_frontier_seq,
                action_frontier_hash: snapshot.action_frontier_hash.clone(),
                recovery_schema_hash: state.recovery_schema_hash.clone(),
                state_hash: state.state_hash.clone(),
                state_artifact_ref: artifact.artifact_ref,
                state_size_bytes: artifact.plaintext_size_bytes,
                lifecycle_stage: state.lifecycle_stage.map(|stage| match stage {
                    RunLifecycleStage::Composing => "composing".to_owned(),
                }),
            })
            .await
            .map_err(|error| map_abi_failure("checkpoint_run_state", &error, true))?;
        if response.run_id != self.receipt.run_id
            || response.fencing_token != snapshot.fencing_token
            || response.provider_checkpoint_seq != snapshot.checkpoint_seq
            || response.action_frontier_seq != snapshot.action_frontier_seq
            || response.action_frontier_hash != snapshot.action_frontier_hash
            || response.recovery_schema_hash != state.recovery_schema_hash
            || response.state_hash != state.state_hash
            || response.state_size_bytes != artifact.plaintext_size_bytes
        {
            return Err(dependency_failure(
                "checkpoint_run_state_receipt",
                RuntimePersistenceError::InvalidAbiReceipt,
                false,
                DeliveryCertainty::NotDispatched,
            ));
        }
        guard.observe_run_state(&response);
        Ok(())
    }

    async fn reserve_child(
        &self,
        mutation: &ReserveChildMutation,
    ) -> Result<ChildExecutionReceipt, DependencyFailure> {
        self.validate_identity(&mutation.run_id, mutation.fencing_token)
            .map_err(|error| {
                dependency_failure(
                    "reserve_child_identity",
                    error,
                    false,
                    DeliveryCertainty::NotDispatched,
                )
            })?;
        self.advance_child_execution(
            &mutation.mutation_id,
            mutation.expected_cancel_generation,
            |current| {
                let proposed = krw_agent_bounded_child::reserve(mutation)?;
                match current {
                    None => Ok(proposed),
                    Some(existing) if existing == &proposed => Ok(existing.clone()),
                    Some(_) => {
                        Err(krw_agent_bounded_child::ChildExecutionError::TransitionConflict)
                    }
                }
            },
        )
        .await
    }

    async fn invoke_child(
        &self,
        mutation: &InvokeChildMutation,
    ) -> Result<ChildExecutionReceipt, DependencyFailure> {
        self.validate_identity(&mutation.run_id, mutation.fencing_token)
            .map_err(|error| {
                dependency_failure(
                    "invoke_child_identity",
                    error,
                    false,
                    DeliveryCertainty::NotDispatched,
                )
            })?;
        self.advance_child_execution(
            &mutation.mutation_id,
            mutation.expected_cancel_generation,
            |current| {
                krw_agent_bounded_child::invoke(
                    current
                        .ok_or(krw_agent_bounded_child::ChildExecutionError::InvalidTransition)?,
                    mutation,
                )
            },
        )
        .await
    }

    async fn complete_child(
        &self,
        mutation: &CompleteChildMutation,
    ) -> Result<ChildExecutionReceipt, DependencyFailure> {
        self.validate_identity(&mutation.run_id, mutation.fencing_token)
            .map_err(|error| {
                dependency_failure(
                    "complete_child_identity",
                    error,
                    false,
                    DeliveryCertainty::NotDispatched,
                )
            })?;
        self.advance_child_execution(
            &mutation.mutation_id,
            mutation.expected_cancel_generation,
            |current| {
                krw_agent_bounded_child::complete(
                    current
                        .ok_or(krw_agent_bounded_child::ChildExecutionError::InvalidTransition)?,
                    mutation,
                )
            },
        )
        .await
    }

    async fn cancel_child(
        &self,
        mutation: &CancelChildMutation,
    ) -> Result<ChildExecutionReceipt, DependencyFailure> {
        self.validate_identity(&mutation.run_id, mutation.fencing_token)
            .map_err(|error| {
                dependency_failure(
                    "cancel_child_identity",
                    error,
                    false,
                    DeliveryCertainty::NotDispatched,
                )
            })?;
        self.advance_child_execution(
            &mutation.mutation_id,
            mutation.expected_cancel_generation,
            |current| {
                krw_agent_bounded_child::cancel(
                    current
                        .ok_or(krw_agent_bounded_child::ChildExecutionError::InvalidTransition)?,
                    mutation,
                )
            },
        )
        .await
    }

    async fn begin_action(
        &self,
        intent: &ActionIntent,
    ) -> Result<ActionReceipt, DependencyFailure> {
        self.validate_identity(&intent.mutation.run_id, intent.mutation.fencing_token)
            .map_err(|error| {
                dependency_failure(
                    "begin_identity",
                    error,
                    false,
                    DeliveryCertainty::NotDispatched,
                )
            })?;
        if ContentHash::sha256(&intent.canonical_arguments) != intent.mutation.request_hash {
            return Err(dependency_failure(
                "arguments_hash",
                RuntimePersistenceError::ArtifactHashMismatch,
                false,
                DeliveryCertainty::NotDispatched,
            ));
        }
        let arguments_artifact_ref = self.action_arguments_reference(intent).await?;
        let mut guard = self.lease.mutation_guard().await.map_err(|_| {
            dependency_failure(
                "lease",
                RuntimePersistenceError::LeaseUnavailable,
                true,
                DeliveryCertainty::NotDispatched,
            )
        })?;
        let snapshot = guard.snapshot();
        let response = self
            .store
            .begin_action(&BeginActionRequest {
                mutation_id: intent.mutation.mutation_id.clone(),
                run_id: self.receipt.run_id.clone(),
                tenant_id: self.receipt.tenant_id.clone(),
                fencing_token: snapshot.fencing_token,
                expected_run_version: snapshot.run_version,
                action_key: intent.mutation.action_key.clone(),
                request_hash: intent.mutation.request_hash.clone(),
                episode_hash: intent.episode_hash.clone(),
                tool_call_id: intent.tool_call_id.clone(),
                capability_id: intent.capability_id.clone(),
                input_schema_hash: intent.input_schema_hash.clone(),
                output_schema_hash: intent.output_schema_hash.clone(),
                data_release_hash: intent.data_release_hash.clone(),
                arguments_artifact_ref,
                retryable_read: intent.mutation.retryable_read,
            })
            .await
            .map_err(|error| map_abi_failure("begin_action", &error, true))?;
        validate_action_abi(&response, &self.receipt.run_id, snapshot.fencing_token).map_err(
            |error| {
                dependency_failure(
                    "begin_action_receipt",
                    error,
                    false,
                    DeliveryCertainty::NotDispatched,
                )
            },
        )?;
        guard.observe_action(&response);
        to_action_receipt(response, intent.mutation.mutation_id.clone()).map_err(|error| {
            dependency_failure(
                "begin_action_stage",
                error,
                false,
                DeliveryCertainty::NotDispatched,
            )
        })
    }

    async fn observe_action(
        &self,
        observation: &DurableActionObservation,
    ) -> Result<ActionReceipt, DependencyFailure> {
        self.validate_identity(
            &observation.mutation.run_id,
            observation.mutation.fencing_token,
        )
        .map_err(|error| {
            dependency_failure(
                "observe_identity",
                error,
                false,
                DeliveryCertainty::NotDispatched,
            )
        })?;
        if ContentHash::sha256(&observation.result_bytes) != observation.mutation.result_hash {
            return Err(dependency_failure(
                "result_hash",
                RuntimePersistenceError::ArtifactHashMismatch,
                false,
                DeliveryCertainty::NotDispatched,
            ));
        }
        let artifact = self
            .artifacts
            .put(
                &self.scope,
                ArtifactKind::ActionResult,
                &observation.result_bytes,
            )
            .await
            .map_err(map_artifact_failure)?;
        if artifact.plaintext_hash != observation.mutation.result_hash {
            return Err(dependency_failure(
                "result_artifact_hash",
                RuntimePersistenceError::ArtifactHashMismatch,
                false,
                DeliveryCertainty::NotDispatched,
            ));
        }
        let mut guard = self.lease.mutation_guard().await.map_err(|_| {
            dependency_failure(
                "lease",
                RuntimePersistenceError::LeaseUnavailable,
                true,
                DeliveryCertainty::NotDispatched,
            )
        })?;
        let snapshot = guard.snapshot();
        let response = self
            .store
            .observe_action(&ObserveActionRequest {
                mutation_id: observation.mutation.mutation_id.clone(),
                run_id: self.receipt.run_id.clone(),
                tenant_id: self.receipt.tenant_id.clone(),
                fencing_token: snapshot.fencing_token,
                expected_run_version: snapshot.run_version,
                action_key: observation.mutation.action_key.clone(),
                result_hash: observation.mutation.result_hash.clone(),
                result_artifact_ref: artifact.artifact_ref.clone(),
            })
            .await
            .map_err(|error| map_abi_failure("observe_action", &error, true))?;
        validate_action_abi(&response, &self.receipt.run_id, snapshot.fencing_token).map_err(
            |error| {
                dependency_failure(
                    "observe_action_receipt",
                    error,
                    false,
                    DeliveryCertainty::NotDispatched,
                )
            },
        )?;
        guard.observe_action(&response);
        self.current_results
            .lock()
            .map_err(|_| {
                dependency_failure(
                    "result_receipt_lock",
                    RuntimePersistenceError::LockPoisoned,
                    true,
                    DeliveryCertainty::NotDispatched,
                )
            })?
            .insert(observation.mutation.action_key.clone(), artifact);
        to_action_receipt(response, observation.mutation.mutation_id.clone()).map_err(|error| {
            dependency_failure(
                "observe_action_stage",
                error,
                false,
                DeliveryCertainty::NotDispatched,
            )
        })
    }

    async fn finalize_action(
        &self,
        mutation: FinalizeActionMutation,
    ) -> Result<ActionFinalizationReceipt, DependencyFailure> {
        DurableRunPersistence::finalize_action(self, mutation).await
    }

    async fn mark_action_ambiguous(
        &self,
        mutation: MarkActionAmbiguous,
    ) -> Result<(), DependencyFailure> {
        self.validate_identity(&mutation.run_id, mutation.fencing_token)
            .map_err(|error| {
                dependency_failure(
                    "ambiguous_identity",
                    error,
                    false,
                    DeliveryCertainty::NotDispatched,
                )
            })?;
        let mut guard = self.lease.mutation_guard().await.map_err(|_| {
            dependency_failure(
                "lease",
                RuntimePersistenceError::LeaseUnavailable,
                true,
                DeliveryCertainty::NotDispatched,
            )
        })?;
        let snapshot = guard.snapshot();
        let response = self
            .store
            .mark_action_ambiguous(&MarkActionAmbiguousRequest {
                mutation_id: mutation.mutation_id,
                run_id: self.receipt.run_id.clone(),
                tenant_id: self.receipt.tenant_id.clone(),
                fencing_token: snapshot.fencing_token,
                expected_run_version: snapshot.run_version,
                action_key: mutation.action_key,
                reason_code: mutation.reason_code,
            })
            .await
            .map_err(|error| map_abi_failure("mark_action_ambiguous", &error, true))?;
        validate_action_abi(&response, &self.receipt.run_id, snapshot.fencing_token).map_err(
            |error| {
                dependency_failure(
                    "ambiguous_receipt",
                    error,
                    false,
                    DeliveryCertainty::NotDispatched,
                )
            },
        )?;
        if !matches!(
            response.stage.as_str(),
            "ambiguous" | "observed" | "accepted" | "rejected" | "committed"
        ) {
            return Err(dependency_failure(
                "ambiguous_stage",
                RuntimePersistenceError::InvalidAbiReceipt,
                false,
                DeliveryCertainty::NotDispatched,
            ));
        }
        guard.observe_action(&response);
        Ok(())
    }

    async fn load_action_result(
        &self,
        run: &RunIdentity,
        action_key: &str,
        expected_hash: &ContentHash,
    ) -> Result<Option<Vec<u8>>, DependencyFailure> {
        self.validate_identity(&run.run_id, run.fencing_token)
            .map_err(|error| {
                dependency_failure(
                    "load_result_identity",
                    error,
                    false,
                    DeliveryCertainty::NotDispatched,
                )
            })?;
        if let Some(bytes) = self.load_current_result(action_key, expected_hash).await? {
            return Ok(Some(bytes));
        }
        let matching_actions = self
            .receipt
            .recovery
            .actions
            .iter()
            .filter(|action| action.action_key == action_key)
            .collect::<Vec<_>>();
        if matching_actions.len() != 1
            || matching_actions[0].result_hash.as_ref() != Some(expected_hash)
        {
            return Ok(None);
        }
        let mut matches = self.recovery.artifacts().iter().filter(|artifact| {
            artifact.kind == RecoveryArtifactKind::ActionResult
                && artifact.action_key.as_deref() == Some(action_key)
                && &artifact.expected_hash == expected_hash
        });
        let Some(artifact) = matches.next() else {
            return Ok(None);
        };
        if matches.next().is_some() {
            return Err(dependency_failure(
                "duplicate_recovery_result",
                RuntimePersistenceError::InvalidAbiReceipt,
                false,
                DeliveryCertainty::NotDispatched,
            ));
        }
        Ok(Some(artifact.bytes().to_vec()))
    }

    async fn commit_final(
        &self,
        final_value: &DurableFinal,
    ) -> Result<FinalStatus, DependencyFailure> {
        self.validate_identity(
            &final_value.mutation.run_id,
            final_value.mutation.fencing_token,
        )
        .map_err(|error| {
            dependency_failure(
                "final_identity",
                error,
                false,
                DeliveryCertainty::NotDispatched,
            )
        })?;
        let bundle_bytes = serde_jcs::to_vec(&final_value.answer_bundle).map_err(|_| {
            dependency_failure(
                "final_encoding",
                RuntimePersistenceError::Json,
                false,
                DeliveryCertainty::NotDispatched,
            )
        })?;
        if ContentHash::sha256(bundle_bytes) != final_value.mutation.answer_bundle_hash {
            return Err(dependency_failure(
                "final_bundle_hash",
                RuntimePersistenceError::ArtifactHashMismatch,
                false,
                DeliveryCertainty::NotDispatched,
            ));
        }
        let (session_memory_delta, memory_revision) = validate_durable_session_memory(final_value)
            .map_err(|error| {
                dependency_failure(
                    "final_session_memory",
                    error,
                    false,
                    DeliveryCertainty::NotDispatched,
                )
            })?;
        let mut guard = self.lease.mutation_guard().await.map_err(|_| {
            dependency_failure(
                "lease",
                RuntimePersistenceError::LeaseUnavailable,
                true,
                DeliveryCertainty::NotDispatched,
            )
        })?;
        let snapshot = guard.snapshot();
        let model_id = receipt_model_id(&self.receipt).map_err(|error| {
            dependency_failure(
                "final_model_id",
                error,
                false,
                DeliveryCertainty::NotDispatched,
            )
        })?;
        let outbox_payloads = final_outbox_payloads(
            &self.finalization,
            &self.receipt.run_id,
            &model_id,
            final_value,
        );
        let response = self
            .store
            .commit_final(&FinalCommitRequest {
                mutation_id: final_value.mutation.mutation_id.clone(),
                run_id: self.receipt.run_id.clone(),
                tenant_id: self.receipt.tenant_id.clone(),
                fencing_token: snapshot.fencing_token,
                expected_run_version: snapshot.run_version,
                expected_cancel_generation: final_value.mutation.expected_cancel_generation,
                answer_bundle_hash: final_value.mutation.answer_bundle_hash.clone(),
                final_output_hash: final_value.final_output_hash.clone(),
                rendered_message_hash: final_value.rendered_message_hash.clone(),
                answer_bundle: final_value.answer_bundle.clone(),
                usage: serde_json::to_value(&final_value.usage).map_err(|_| {
                    dependency_failure(
                        "usage_encoding",
                        RuntimePersistenceError::Json,
                        false,
                        DeliveryCertainty::NotDispatched,
                    )
                })?,
                settlement: settlement_payload(&final_value.usage),
                outbox_payloads,
                session_memory_delta_hash: final_value.mutation.session_memory_delta_hash.clone(),
                session_memory_delta,
                next_memory_frontier_hash: final_value.next_memory_frontier_hash.clone(),
            })
            .await;
        let response = match response {
            Ok(response) => response,
            Err(AgentV1Error::Rejected {
                kind: RejectionKind::CancelledBeforeFinal | RejectionKind::CancelGenerationMismatch,
                ..
            }) => {
                if matches!(
                    self.resolve_final_ambiguity(&final_value.mutation.answer_bundle_hash)
                        .await?,
                    Some(FinalStatus::Cancelled)
                ) {
                    guard.mark_terminal();
                    return Ok(FinalStatus::Cancelled);
                }
                return Err(DependencyFailure::redacted(
                    "final_cancel_race",
                    "cancel generation changed without a terminal cancel",
                    true,
                    DeliveryCertainty::NotDispatched,
                ));
            }
            Err(error) => return Err(map_abi_failure("commit_final", &error, true)),
        };
        if response.run_id != self.receipt.run_id
            || response.fencing_token != snapshot.fencing_token
            || response.answer_bundle_hash != final_value.mutation.answer_bundle_hash
            || response.memory_revision != memory_revision
            || response.memory_frontier_hash != final_value.next_memory_frontier_hash
            || response.memory_source_lineage_hash.is_some() != memory_revision.is_some()
        {
            return Err(dependency_failure(
                "final_receipt",
                RuntimePersistenceError::InvalidAbiReceipt,
                false,
                DeliveryCertainty::NotDispatched,
            ));
        }
        let status = match response.outcome.as_str() {
            "final_committed" => FinalStatus::Committed,
            "already_final" => FinalStatus::AlreadyCommitted,
            _ => {
                return Err(dependency_failure(
                    "final_outcome",
                    RuntimePersistenceError::InvalidAbiReceipt,
                    false,
                    DeliveryCertainty::NotDispatched,
                ));
            }
        };
        guard.observe_version(response.run_version, None, None);
        guard.mark_terminal();
        Ok(status)
    }
}

fn final_outbox_payloads(
    policy: &FinalizationPolicy,
    run_id: &str,
    model_id: &str,
    value: &DurableFinal,
) -> BTreeMap<String, Value> {
    let mut payloads = BTreeMap::new();
    if policy.emit_presentation_outbox {
        payloads.insert(
            "presentation".into(),
            json!({
                "schema_version": 1,
                "run_id": run_id,
                "answer_bundle_hash": value.mutation.answer_bundle_hash,
                "final_output_hash": value.final_output_hash,
                "rendered_message_hash": value.rendered_message_hash,
            }),
        );
    }
    if policy.emit_billing_outbox {
        payloads.insert(
            "billing".into(),
            json!({
                "schema_version": 1,
                "run_id": run_id,
                "answer_bundle_hash": value.mutation.answer_bundle_hash,
                "model_id": model_id,
                "usage": value.usage,
            }),
        );
    }
    payloads
}

/// The model is read from the already admitted immutable claim rather than
/// from mutable process configuration. This keeps billing tied to the exact
/// provider release that executed the run, including after a GLM/DeepSeek
/// rollout.
fn receipt_model_id(
    receipt: &krw_agent_persistence::agent_v1::ClaimReceipt,
) -> Result<String, RuntimePersistenceError> {
    let model_id = receipt
        .immutable_snapshot
        .get("execution")
        .and_then(Value::as_object)
        .and_then(|execution| execution.get("resolved_model"))
        .and_then(Value::as_str)
        .ok_or(RuntimePersistenceError::InvalidAbiReceipt)?;
    if !ALLOWED_MODEL_IDS.contains(&model_id) {
        return Err(RuntimePersistenceError::InvalidAbiReceipt);
    }
    Ok(model_id.to_owned())
}

fn settlement_payload(usage: &BudgetUsage) -> Value {
    json!({
        "schema_version": 1,
        "kind": "usage_settled",
        "usage": usage,
    })
}

fn validate_mutation_receipt(
    receipt: &MutationReceipt,
    run_id: &str,
    fence: u64,
) -> Result<(), RuntimePersistenceError> {
    if receipt.run_id != run_id
        || receipt.fencing_token != fence
        || receipt.run_version == 0
        || receipt.outcome.is_empty()
    {
        return Err(RuntimePersistenceError::InvalidAbiReceipt);
    }
    Ok(())
}

fn validate_action_abi(
    receipt: &ActionAbiReceipt,
    run_id: &str,
    fence: u64,
) -> Result<(), RuntimePersistenceError> {
    if receipt.run_id != run_id
        || receipt.fencing_token != fence
        || receipt.run_version == 0
        || receipt.action_key.is_empty()
        || receipt.outcome.is_empty()
    {
        return Err(RuntimePersistenceError::InvalidAbiReceipt);
    }
    Ok(())
}

fn validate_child_abi(
    receipt: &ChildExecutionAbiReceipt,
) -> Result<ChildExecutionReceipt, RuntimePersistenceError> {
    receipt
        .receipt
        .validate()
        .map_err(|_| RuntimePersistenceError::InvalidAbiReceipt)?;
    let observed_hash = receipt
        .receipt
        .receipt_hash()
        .map_err(|_| RuntimePersistenceError::InvalidAbiReceipt)?;
    if receipt.run_id != receipt.receipt.run_id
        || receipt.child_id != receipt.receipt.child_id
        || receipt.stage != receipt.receipt.stage
        || receipt.fencing_token < receipt.receipt.fencing_token
        || receipt.run_version == 0
        || receipt.receipt_hash != observed_hash
        || receipt.outcome.is_empty()
    {
        return Err(RuntimePersistenceError::InvalidAbiReceipt);
    }
    Ok(receipt.receipt.clone())
}

struct SessionMemoryPageExpectation<'a> {
    run_id: &'a str,
    session_id: &'a str,
    fencing_token: u64,
    run_version: u64,
    after_revision: u64,
    limit: u16,
    mode: SessionMemoryReadMode,
}

fn validate_session_memory_page(
    page: &ReadSessionMemoryResponse,
    expected: &SessionMemoryPageExpectation<'_>,
) -> Result<(), RuntimePersistenceError> {
    let SessionMemoryPageExpectation {
        run_id,
        session_id,
        fencing_token,
        run_version,
        after_revision,
        limit,
        mode,
    } = expected;
    if !(1..=8).contains(limit)
        || *after_revision > i64::MAX as u64
        || page.run_id != *run_id
        || page.fencing_token != *fencing_token
        || page.run_version != *run_version
        || page.memory_revision > i64::MAX as u64
        || page.memory_revision < *after_revision
        || page.deltas.len() > usize::from(*limit)
    {
        return Err(RuntimePersistenceError::InvalidAbiReceipt);
    }

    let mut effective_after = *after_revision;
    let expected_session_hash = ContentHash::sha256(*session_id);
    match (&page.snapshot, *mode, *after_revision, page.memory_revision) {
        (Some(receipt), SessionMemoryReadMode::SnapshotTail, 0, revision) if revision > 0 => {
            let snapshot: SessionMemorySnapshotV3 =
                serde_json::from_value(receipt.snapshot.clone())
                    .map_err(|_| RuntimePersistenceError::InvalidAbiReceipt)?;
            let canonical = snapshot
                .canonical_bytes()
                .map_err(|_| RuntimePersistenceError::InvalidAbiReceipt)?;
            if canonical.len() > MAX_SESSION_MEMORY_SNAPSHOT_BYTES
                || u64::try_from(canonical.len()).ok() != Some(receipt.snapshot_size_bytes)
                || ContentHash::sha256(&canonical) != receipt.snapshot_hash
                || snapshot.revision != receipt.revision
                || snapshot.frontier_hash != receipt.frontier_hash
                || snapshot.source_lineage_hash != receipt.source_lineage_hash
                || snapshot.session_id_hash != expected_session_hash
                || receipt.revision > page.memory_revision
                || page.memory_revision - receipt.revision > 32
            {
                return Err(RuntimePersistenceError::InvalidAbiReceipt);
            }
            effective_after = receipt.revision;
        }
        (None, SessionMemoryReadMode::SnapshotTail, 0, 0)
        | (None, SessionMemoryReadMode::SnapshotTail, 1.., _)
        | (None, SessionMemoryReadMode::AuditRebuild, _, _) => {}
        _ => return Err(RuntimePersistenceError::InvalidAbiReceipt),
    }

    let mut expected_revision = effective_after
        .checked_add(1)
        .ok_or(RuntimePersistenceError::InvalidAbiReceipt)?;
    let mut previous_frontier = page
        .snapshot
        .as_ref()
        .map(|snapshot| snapshot.frontier_hash.clone())
        .or_else(|| (effective_after == 0).then(empty_frontier_hash));
    let mut previous_source_lineage = page
        .snapshot
        .as_ref()
        .map(|snapshot| snapshot.source_lineage_hash.clone())
        .or_else(|| (effective_after == 0).then(empty_source_lineage_hash));
    for receipt in &page.deltas {
        let delta: SessionMemoryDeltaV3 = serde_json::from_value(receipt.delta.clone())
            .map_err(|_| RuntimePersistenceError::InvalidAbiReceipt)?;
        let delta_hash = delta
            .content_hash()
            .map_err(|_| RuntimePersistenceError::InvalidAbiReceipt)?;
        let next_frontier_hash = delta
            .next_frontier_hash()
            .map_err(|_| RuntimePersistenceError::InvalidAbiReceipt)?;
        if receipt.revision != expected_revision
            || delta.revision != receipt.revision
            || delta.session_id_hash != expected_session_hash
            || delta.parent_frontier_hash != receipt.parent_frontier_hash
            || delta_hash != receipt.delta_hash
            || next_frontier_hash != receipt.next_frontier_hash
            || delta.source.run_id != receipt.source_run_id
            || previous_frontier
                .as_ref()
                .is_some_and(|frontier| frontier != &receipt.parent_frontier_hash)
            || previous_source_lineage.as_ref().is_some_and(|lineage| {
                next_source_lineage_hash(
                    lineage,
                    receipt.revision,
                    &ContentHash::sha256(&receipt.source_run_id),
                ) != receipt.source_lineage_hash
            })
        {
            return Err(RuntimePersistenceError::InvalidAbiReceipt);
        }
        previous_frontier = Some(receipt.next_frontier_hash.clone());
        previous_source_lineage = Some(receipt.source_lineage_hash.clone());
        expected_revision = expected_revision
            .checked_add(1)
            .ok_or(RuntimePersistenceError::InvalidAbiReceipt)?;
    }

    let expected_next = page
        .deltas
        .last()
        .map_or(effective_after, |receipt| receipt.revision);
    if page.next_after_revision != expected_next
        || page.next_after_revision > page.memory_revision
        || (page.has_more
            && (page.deltas.is_empty() || page.next_after_revision >= page.memory_revision))
        || (!page.has_more && page.next_after_revision != page.memory_revision)
        || (page.memory_revision == 0 && page.memory_frontier_hash != empty_frontier_hash())
        || (page.memory_revision == 0
            && page.memory_source_lineage_hash != empty_source_lineage_hash())
        || (!page.has_more
            && previous_source_lineage
                .as_ref()
                .is_some_and(|lineage| lineage != &page.memory_source_lineage_hash))
    {
        return Err(RuntimePersistenceError::InvalidAbiReceipt);
    }
    Ok(())
}

fn validate_durable_session_memory(
    final_value: &DurableFinal,
) -> Result<(Option<Value>, Option<u64>), RuntimePersistenceError> {
    match &final_value.session_memory_delta {
        None => {
            if final_value.mutation.session_memory_delta_hash.is_some()
                || final_value.next_memory_frontier_hash.is_some()
            {
                return Err(RuntimePersistenceError::InvalidAbiReceipt);
            }
            Ok((None, None))
        }
        Some(delta) => {
            let delta_hash = delta
                .content_hash()
                .map_err(|_| RuntimePersistenceError::InvalidAbiReceipt)?;
            let next_frontier_hash = delta
                .next_frontier_hash()
                .map_err(|_| RuntimePersistenceError::InvalidAbiReceipt)?;
            if final_value.mutation.session_memory_delta_hash.as_ref() != Some(&delta_hash)
                || final_value.next_memory_frontier_hash.as_ref() != Some(&next_frontier_hash)
            {
                return Err(RuntimePersistenceError::InvalidAbiReceipt);
            }
            let value = serde_json::to_value(delta).map_err(|_| RuntimePersistenceError::Json)?;
            Ok((Some(value), Some(delta.revision)))
        }
    }
}

fn to_action_receipt(
    receipt: ActionAbiReceipt,
    mutation_id: String,
) -> Result<ActionReceipt, RuntimePersistenceError> {
    let stage = parse_action_stage(&receipt.stage)?;
    Ok(ActionReceipt {
        action_key: receipt.action_key,
        mutation_id,
        request_hash: receipt.request_hash,
        result_hash: receipt.result_hash,
        stage,
        retryable_read: receipt.retryable_read,
    })
}

fn to_action_finalization_receipt(
    receipt: ActionAbiReceipt,
    mutation_id: String,
) -> Result<ActionFinalizationReceipt, RuntimePersistenceError> {
    let disposition = receipt
        .disposition
        .ok_or(RuntimePersistenceError::InvalidAbiReceipt)?;
    let validation_receipt_hash = receipt
        .validation_receipt_hash
        .clone()
        .ok_or(RuntimePersistenceError::InvalidAbiReceipt)?;
    let policy_receipt_hash = receipt
        .policy_receipt_hash
        .clone()
        .ok_or(RuntimePersistenceError::InvalidAbiReceipt)?;
    let action = to_action_receipt(receipt, mutation_id)?;
    if action.stage != disposition.stage() {
        return Err(RuntimePersistenceError::InvalidAbiReceipt);
    }
    Ok(ActionFinalizationReceipt {
        action,
        disposition,
        validation_receipt_hash,
        policy_receipt_hash,
    })
}

fn parse_action_stage(stage: &str) -> Result<ActionStage, RuntimePersistenceError> {
    Ok(match stage {
        "begun" => ActionStage::Begun,
        "observed" => ActionStage::Observed,
        "accepted" => ActionStage::Accepted,
        "rejected" => ActionStage::Rejected,
        "ambiguous" => ActionStage::Ambiguous,
        _ => return Err(RuntimePersistenceError::InvalidAbiReceipt),
    })
}

const fn action_stage_name(stage: ActionStage) -> &'static str {
    match stage {
        ActionStage::Begun => "begun",
        ActionStage::Observed => "observed",
        ActionStage::Accepted => "accepted",
        ActionStage::Rejected => "rejected",
        ActionStage::Ambiguous => "ambiguous",
    }
}

fn map_artifact_failure(error: ArtifactRepositoryError) -> DependencyFailure {
    dependency_failure(
        "artifact_write",
        error,
        true,
        DeliveryCertainty::NotDispatched,
    )
}

fn map_abi_failure(code: &'static str, error: &AgentV1Error, mutation: bool) -> DependencyFailure {
    tracing::warn!(component = code, error = ?error, "agent persistence ABI call failed");
    let (retryable, delivery) = match &error {
        AgentV1Error::Database { .. } => (
            true,
            if mutation {
                DeliveryCertainty::MayHaveDispatched
            } else {
                DeliveryCertainty::NotDispatched
            },
        ),
        AgentV1Error::Rejected {
            kind: RejectionKind::RunVersionMismatch | RejectionKind::LeaseLost,
            ..
        } => (true, DeliveryCertainty::NotDispatched),
        AgentV1Error::Rejected { .. }
        | AgentV1Error::Encode(_)
        | AgentV1Error::RequestMustBeObject(_)
        | AgentV1Error::InvalidRequest { .. }
        | AgentV1Error::InvalidResponse { .. } => (false, DeliveryCertainty::NotDispatched),
    };
    DependencyFailure::redacted(code, format!("{error:?}"), retryable, delivery)
}

fn dependency_failure(
    code: &'static str,
    error: impl fmt::Debug,
    retryable: bool,
    delivery: DeliveryCertainty,
) -> DependencyFailure {
    DependencyFailure::redacted(code, format!("{error:?}"), retryable, delivery)
}

fn interpret_committed_outcome(
    outcome: &ReadCommittedOutcomeResponse,
    expected_hash: &ContentHash,
) -> Result<Option<FinalStatus>, DependencyFailure> {
    match outcome.state.as_str() {
        "final" => {
            let observed = outcome
                .terminal_outcome
                .as_ref()
                .and_then(|value| value.get("answer_bundle_hash"))
                .and_then(Value::as_str)
                .and_then(|value| ContentHash::parse(value).ok());
            if observed.as_ref() == Some(expected_hash) {
                Ok(Some(FinalStatus::AlreadyCommitted))
            } else {
                Err(dependency_failure(
                    "final_outcome_conflict",
                    RuntimePersistenceError::InvalidAbiReceipt,
                    false,
                    DeliveryCertainty::NotDispatched,
                ))
            }
        }
        "cancelled" => Ok(Some(FinalStatus::Cancelled)),
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use krw_agent_persistence::agent_v1::AgentV1Procedure;

    use super::*;

    fn outcome(state: &str, terminal_outcome: Option<Value>) -> ReadCommittedOutcomeResponse {
        ReadCommittedOutcomeResponse {
            run_id: "run-a".into(),
            state: state.into(),
            fencing_token: 7,
            run_version: 11,
            cancel_generation: 0,
            terminal_outcome,
        }
    }

    #[test]
    fn final_commit_ack_loss_is_resolved_only_by_the_exact_durable_hash() {
        let expected = ContentHash::sha256("answer-a");
        let committed = outcome(
            "final",
            Some(json!({"answer_bundle_hash": expected.as_str()})),
        );
        assert_eq!(
            interpret_committed_outcome(&committed, &expected).unwrap(),
            Some(FinalStatus::AlreadyCommitted)
        );

        let conflicting = outcome(
            "final",
            Some(json!({"answer_bundle_hash": ContentHash::sha256("answer-b").as_str()})),
        );
        assert!(interpret_committed_outcome(&conflicting, &expected).is_err());
        assert_eq!(
            interpret_committed_outcome(&outcome("active", None), &expected).unwrap(),
            None
        );
    }

    #[test]
    fn terminal_cancel_wins_an_ambiguous_final_race() {
        assert_eq!(
            interpret_committed_outcome(
                &outcome("cancelled", Some(json!({"reason": "user_cancel"}))),
                &ContentHash::sha256("answer"),
            )
            .unwrap(),
            Some(FinalStatus::Cancelled)
        );
    }

    #[test]
    fn stale_fence_is_never_classified_as_a_dispatched_mutation() {
        let failure = map_abi_failure(
            "finalize_action",
            &AgentV1Error::Rejected {
                procedure: AgentV1Procedure::FinalizeAction,
                kind: RejectionKind::StaleFence,
                diagnostic_hash: ContentHash::sha256("stale"),
            },
            true,
        );
        assert!(!failure.retryable);
        assert_eq!(failure.delivery, DeliveryCertainty::NotDispatched);
    }

    #[test]
    fn finalization_receipt_preserves_rejected_stage_and_exact_hashes() {
        let validation_receipt_hash = ContentHash::sha256("validation");
        let policy_receipt_hash = ContentHash::sha256("policy");
        let receipt = to_action_finalization_receipt(
            ActionAbiReceipt {
                outcome: "rejected".into(),
                run_id: "run-a".into(),
                fencing_token: 7,
                run_version: 12,
                action_key: "action-a".into(),
                stage: "rejected".into(),
                request_hash: ContentHash::sha256("request"),
                result_hash: Some(ContentHash::sha256("raw-result")),
                retryable_read: true,
                disposition: Some(krw_agent_persistence::ActionDisposition::Rejected),
                validation_receipt_hash: Some(validation_receipt_hash.clone()),
                policy_receipt_hash: Some(policy_receipt_hash.clone()),
                action_frontier_seq: 3,
                action_frontier_hash: ContentHash::sha256("frontier"),
            },
            "finalize-a".into(),
        )
        .unwrap();

        assert_eq!(receipt.action.stage, ActionStage::Rejected);
        assert_eq!(receipt.validation_receipt_hash, validation_receipt_hash);
        assert_eq!(receipt.policy_receipt_hash, policy_receipt_hash);
    }

    #[test]
    fn receipt_less_committed_action_stage_is_rejected() {
        assert!(parse_action_stage("committed").is_err());
    }
}
