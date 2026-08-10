//! Persistence ABI and an exhaustive-test-friendly in-memory receipt model.

pub mod agent_v1;
pub mod daemon;
pub mod metrics;
#[cfg(feature = "postgres")]
pub mod postgres;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

use krw_agent_bounded_child::{
    CancelChildMutation, ChildExecutionReceipt, CompleteChildMutation, InvokeChildMutation,
    ReserveChildMutation, cancel as cancel_child_transition, complete as complete_child_transition,
    invoke as invoke_child_transition, reserve as reserve_child_transition,
};
use krw_agent_protocol::ContentHash;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunReceipt {
    pub run_id: String,
    pub fencing_token: u64,
    pub run_version: u64,
    pub cancel_generation: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionStage {
    Begun,
    Observed,
    Accepted,
    Rejected,
    Ambiguous,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionDisposition {
    Accepted,
    Rejected,
}

impl ActionDisposition {
    pub const fn stage(self) -> ActionStage {
        match self {
            Self::Accepted => ActionStage::Accepted,
            Self::Rejected => ActionStage::Rejected,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionReceipt {
    pub action_key: String,
    pub mutation_id: String,
    pub request_hash: ContentHash,
    pub result_hash: Option<ContentHash>,
    pub stage: ActionStage,
    pub retryable_read: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BeginActionMutation {
    pub run_id: String,
    pub fencing_token: u64,
    pub mutation_id: String,
    pub action_key: String,
    pub request_hash: ContentHash,
    pub retryable_read: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ObserveActionMutation {
    pub run_id: String,
    pub fencing_token: u64,
    pub mutation_id: String,
    pub action_key: String,
    pub result_hash: ContentHash,
}

/// Finalizes an already observed action result after schema/provenance
/// validation and the deny-monotone `AfterAction` policy phase completed.
///
/// Both receipt hashes are required for either disposition. A rejected result
/// remains recoverable for deterministic replay, but cannot later be promoted
/// to accepted under the same action identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FinalizeActionMutation {
    pub run_id: String,
    pub fencing_token: u64,
    pub mutation_id: String,
    pub action_key: String,
    pub result_hash: ContentHash,
    pub disposition: ActionDisposition,
    pub validation_receipt_hash: ContentHash,
    pub policy_receipt_hash: ContentHash,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionFinalizationReceipt {
    pub action: ActionReceipt,
    pub disposition: ActionDisposition,
    pub validation_receipt_hash: ContentHash,
    pub policy_receipt_hash: ContentHash,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FinalCommitMutation {
    pub run_id: String,
    pub fencing_token: u64,
    pub expected_cancel_generation: u64,
    pub mutation_id: String,
    pub answer_bundle_hash: ContentHash,
    pub session_memory_delta_hash: Option<ContentHash>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CancelMutation {
    pub run_id: String,
    pub fencing_token: u64,
    pub mutation_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CheckpointEpisodeMutation {
    pub run_id: String,
    pub fencing_token: u64,
    pub mutation_id: String,
    pub episode_hash: ContentHash,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalOutcome {
    Final { answer_bundle_hash: ContentHash },
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommitOutcome {
    Committed(TerminalOutcome),
    AlreadyCommitted(TerminalOutcome),
}

#[derive(Debug)]
struct StoredRun {
    receipt: RunReceipt,
    mutations: BTreeMap<String, ContentHash>,
    actions: BTreeMap<String, StoredAction>,
    child: Option<ChildExecutionReceipt>,
    episode_hashes: BTreeSet<ContentHash>,
    terminal: Option<TerminalOutcome>,
}

#[derive(Debug)]
struct StoredAction {
    receipt: ActionReceipt,
    finalization: Option<ActionFinalizationReceipt>,
}

#[derive(Debug, Default)]
pub struct InMemoryPersistence {
    runs: Mutex<BTreeMap<String, StoredRun>>,
}

impl InMemoryPersistence {
    pub fn insert_run(&self, receipt: RunReceipt) -> Result<(), PersistenceError> {
        let mut runs = self.runs.lock().map_err(|_| PersistenceError::Poisoned)?;
        if runs.contains_key(&receipt.run_id) {
            return Err(PersistenceError::RunAlreadyExists(receipt.run_id));
        }
        runs.insert(
            receipt.run_id.clone(),
            StoredRun {
                receipt,
                mutations: BTreeMap::new(),
                actions: BTreeMap::new(),
                child: None,
                episode_hashes: BTreeSet::new(),
                terminal: None,
            },
        );
        Ok(())
    }

    pub fn checkpoint_episode(
        &self,
        mutation: CheckpointEpisodeMutation,
    ) -> Result<ContentHash, PersistenceError> {
        let payload_hash = mutation_payload_hash("checkpoint_episode", &mutation)?;
        let mut runs = self.runs.lock().map_err(|_| PersistenceError::Poisoned)?;
        let run = checked_run(&mut runs, &mutation.run_id, mutation.fencing_token)?;
        let replay = validate_mutation(run, &mutation.mutation_id, &payload_hash)?;
        if replay && run.episode_hashes.contains(&mutation.episode_hash) {
            return Ok(mutation.episode_hash);
        }
        if run.terminal.is_some() {
            return Err(PersistenceError::TerminalRun);
        }
        run.episode_hashes.insert(mutation.episode_hash.clone());
        record_mutation(run, &mutation.mutation_id, payload_hash);
        Ok(mutation.episode_hash)
    }

    pub fn has_episode(
        &self,
        run_id: &str,
        episode_hash: &ContentHash,
    ) -> Result<bool, PersistenceError> {
        let runs = self.runs.lock().map_err(|_| PersistenceError::Poisoned)?;
        Ok(runs
            .get(run_id)
            .ok_or_else(|| PersistenceError::UnknownRun(run_id.into()))?
            .episode_hashes
            .contains(episode_hash))
    }

    pub fn begin_action(
        &self,
        mutation: BeginActionMutation,
    ) -> Result<ActionReceipt, PersistenceError> {
        let payload_hash = mutation_payload_hash("begin_action", &mutation)?;
        let mut runs = self.runs.lock().map_err(|_| PersistenceError::Poisoned)?;
        let run = checked_run(&mut runs, &mutation.run_id, mutation.fencing_token)?;
        let replay = validate_mutation(run, &mutation.mutation_id, &payload_hash)?;
        if run.terminal.is_some() && !replay {
            return Err(PersistenceError::TerminalRun);
        }
        if let Some(existing) = run.actions.get(&mutation.action_key) {
            if existing.receipt.request_hash != mutation.request_hash {
                return Err(PersistenceError::ActionConflict(mutation.action_key));
            }
            let receipt = existing.receipt.clone();
            if !replay {
                record_mutation(run, &mutation.mutation_id, payload_hash);
            }
            return Ok(receipt);
        }
        if replay {
            return Err(PersistenceError::MutationReplayStateMissing(
                mutation.mutation_id,
            ));
        }
        let receipt = ActionReceipt {
            action_key: mutation.action_key.clone(),
            mutation_id: mutation.mutation_id.clone(),
            request_hash: mutation.request_hash,
            result_hash: None,
            stage: ActionStage::Begun,
            retryable_read: mutation.retryable_read,
        };
        run.actions.insert(
            mutation.action_key,
            StoredAction {
                receipt: receipt.clone(),
                finalization: None,
            },
        );
        record_mutation(run, &mutation.mutation_id, payload_hash);
        Ok(receipt)
    }

    pub fn reserve_child(
        &self,
        mutation: ReserveChildMutation,
    ) -> Result<ChildExecutionReceipt, PersistenceError> {
        let payload_hash = mutation_payload_hash("reserve_child", &mutation)?;
        let mut runs = self.runs.lock().map_err(|_| PersistenceError::Poisoned)?;
        let run = checked_run(&mut runs, &mutation.run_id, mutation.fencing_token)?;
        let replay = validate_mutation(run, &mutation.mutation_id, &payload_hash)?;
        if run.receipt.cancel_generation != mutation.expected_cancel_generation {
            return Err(PersistenceError::CancelGenerationMismatch {
                expected: mutation.expected_cancel_generation,
                observed: run.receipt.cancel_generation,
            });
        }
        if run.terminal.is_some() && !replay {
            return Err(PersistenceError::TerminalRun);
        }
        let proposed = reserve_child_transition(&mutation)
            .map_err(|error| PersistenceError::ChildTransition(error.to_string()))?;
        if let Some(existing) = &run.child {
            if existing != &proposed {
                return Err(PersistenceError::ChildConflict);
            }
            let receipt = existing.clone();
            if !replay {
                record_mutation(run, &mutation.mutation_id, payload_hash);
            }
            return Ok(receipt);
        }
        if replay {
            return Err(PersistenceError::MutationReplayStateMissing(
                mutation.mutation_id,
            ));
        }
        run.child = Some(proposed.clone());
        record_mutation(run, &mutation.mutation_id, payload_hash);
        Ok(proposed)
    }

    pub fn invoke_child(
        &self,
        mutation: &InvokeChildMutation,
    ) -> Result<ChildExecutionReceipt, PersistenceError> {
        let payload_hash = mutation_payload_hash("invoke_child", &mutation)?;
        self.transition_child(
            &mutation.mutation_id,
            payload_hash,
            &mutation.run_id,
            mutation.fencing_token,
            mutation.expected_cancel_generation,
            |receipt| invoke_child_transition(receipt, mutation),
        )
    }

    pub fn complete_child(
        &self,
        mutation: &CompleteChildMutation,
    ) -> Result<ChildExecutionReceipt, PersistenceError> {
        let payload_hash = mutation_payload_hash("complete_child", &mutation)?;
        self.transition_child(
            &mutation.mutation_id,
            payload_hash,
            &mutation.run_id,
            mutation.fencing_token,
            mutation.expected_cancel_generation,
            |receipt| complete_child_transition(receipt, mutation),
        )
    }

    pub fn cancel_child(
        &self,
        mutation: &CancelChildMutation,
    ) -> Result<ChildExecutionReceipt, PersistenceError> {
        let payload_hash = mutation_payload_hash("cancel_child", &mutation)?;
        self.transition_child(
            &mutation.mutation_id,
            payload_hash,
            &mutation.run_id,
            mutation.fencing_token,
            mutation.expected_cancel_generation,
            |receipt| cancel_child_transition(receipt, mutation),
        )
    }

    fn transition_child(
        &self,
        mutation_id: &str,
        payload_hash: ContentHash,
        run_id: &str,
        fencing_token: u64,
        expected_cancel_generation: u64,
        transition: impl FnOnce(
            &ChildExecutionReceipt,
        ) -> Result<
            ChildExecutionReceipt,
            krw_agent_bounded_child::ChildExecutionError,
        >,
    ) -> Result<ChildExecutionReceipt, PersistenceError> {
        let mut runs = self.runs.lock().map_err(|_| PersistenceError::Poisoned)?;
        let run = checked_run(&mut runs, run_id, fencing_token)?;
        let replay = validate_mutation(run, mutation_id, &payload_hash)?;
        if run.receipt.cancel_generation != expected_cancel_generation {
            return Err(PersistenceError::CancelGenerationMismatch {
                expected: expected_cancel_generation,
                observed: run.receipt.cancel_generation,
            });
        }
        if run.terminal.is_some() && !replay {
            return Err(PersistenceError::TerminalRun);
        }
        let existing = run.child.as_ref().ok_or(PersistenceError::UnknownChild)?;
        let next = transition(existing)
            .map_err(|error| PersistenceError::ChildTransition(error.to_string()))?;
        run.child = Some(next.clone());
        if !replay {
            record_mutation(run, mutation_id, payload_hash);
        }
        Ok(next)
    }

    pub fn read_child(
        &self,
        run_id: &str,
    ) -> Result<Option<ChildExecutionReceipt>, PersistenceError> {
        let runs = self.runs.lock().map_err(|_| PersistenceError::Poisoned)?;
        Ok(runs
            .get(run_id)
            .ok_or_else(|| PersistenceError::UnknownRun(run_id.into()))?
            .child
            .clone())
    }

    pub fn observe_action(
        &self,
        mutation: ObserveActionMutation,
    ) -> Result<ActionReceipt, PersistenceError> {
        let payload_hash = mutation_payload_hash("observe_action", &mutation)?;
        let mut runs = self.runs.lock().map_err(|_| PersistenceError::Poisoned)?;
        let run = checked_run(&mut runs, &mutation.run_id, mutation.fencing_token)?;
        let replay = validate_mutation(run, &mutation.mutation_id, &payload_hash)?;
        if run.terminal.is_some() && !replay {
            return Err(PersistenceError::TerminalRun);
        }
        let receipt = {
            let action = run
                .actions
                .get_mut(&mutation.action_key)
                .ok_or_else(|| PersistenceError::UnknownAction(mutation.action_key.clone()))?;
            if let Some(existing) = &action.receipt.result_hash {
                if existing != &mutation.result_hash {
                    return Err(PersistenceError::ObservationConflict(mutation.action_key));
                }
                action.receipt.clone()
            } else {
                if replay {
                    return Err(PersistenceError::MutationReplayStateMissing(
                        mutation.mutation_id,
                    ));
                }
                action.receipt.result_hash = Some(mutation.result_hash);
                action.receipt.stage = ActionStage::Observed;
                action.receipt.clone()
            }
        };
        if !replay {
            record_mutation(run, &mutation.mutation_id, payload_hash);
        }
        Ok(receipt)
    }

    pub fn finalize_action(
        &self,
        mutation: FinalizeActionMutation,
    ) -> Result<ActionFinalizationReceipt, PersistenceError> {
        let payload_hash = mutation_payload_hash("finalize_action", &mutation)?;
        let mut runs = self.runs.lock().map_err(|_| PersistenceError::Poisoned)?;
        let run = checked_run(&mut runs, &mutation.run_id, mutation.fencing_token)?;
        let replay = validate_mutation(run, &mutation.mutation_id, &payload_hash)?;
        if run.terminal.is_some() && !replay {
            return Err(PersistenceError::TerminalRun);
        }
        let receipt = {
            let action = run
                .actions
                .get_mut(&mutation.action_key)
                .ok_or_else(|| PersistenceError::UnknownAction(mutation.action_key.clone()))?;
            if action.receipt.result_hash.as_ref() != Some(&mutation.result_hash) {
                return Err(PersistenceError::FinalizeWithoutMatchingObservation(
                    mutation.action_key,
                ));
            }

            if let Some(existing) = &action.finalization {
                if existing.disposition != mutation.disposition
                    || existing.validation_receipt_hash != mutation.validation_receipt_hash
                    || existing.policy_receipt_hash != mutation.policy_receipt_hash
                {
                    return Err(PersistenceError::ActionFinalizationConflict(
                        mutation.action_key,
                    ));
                }
                existing.clone()
            } else {
                if replay {
                    return Err(PersistenceError::MutationReplayStateMissing(
                        mutation.mutation_id,
                    ));
                }
                match action.receipt.stage {
                    ActionStage::Observed => {}
                    ActionStage::Accepted
                    | ActionStage::Rejected
                    | ActionStage::Begun
                    | ActionStage::Ambiguous => {
                        return Err(PersistenceError::ActionFinalizationConflict(
                            mutation.action_key,
                        ));
                    }
                }
                action.receipt.stage = mutation.disposition.stage();
                let finalized = ActionFinalizationReceipt {
                    action: action.receipt.clone(),
                    disposition: mutation.disposition,
                    validation_receipt_hash: mutation.validation_receipt_hash,
                    policy_receipt_hash: mutation.policy_receipt_hash,
                };
                action.finalization = Some(finalized.clone());
                finalized
            }
        };
        if !replay {
            record_mutation(run, &mutation.mutation_id, payload_hash);
        }
        Ok(receipt)
    }

    pub fn read_action_finalization(
        &self,
        run_id: &str,
        action_key: &str,
    ) -> Result<Option<ActionFinalizationReceipt>, PersistenceError> {
        let runs = self.runs.lock().map_err(|_| PersistenceError::Poisoned)?;
        let run = runs
            .get(run_id)
            .ok_or_else(|| PersistenceError::UnknownRun(run_id.into()))?;
        Ok(run
            .actions
            .get(action_key)
            .and_then(|action| action.finalization.clone()))
    }

    pub fn commit_final(
        &self,
        mutation: FinalCommitMutation,
    ) -> Result<CommitOutcome, PersistenceError> {
        let payload_hash = mutation_payload_hash("commit_final", &mutation)?;
        let mut runs = self.runs.lock().map_err(|_| PersistenceError::Poisoned)?;
        let run = checked_run(&mut runs, &mutation.run_id, mutation.fencing_token)?;
        let replay = validate_mutation(run, &mutation.mutation_id, &payload_hash)?;
        if let Some(outcome) = &run.terminal {
            let result = match outcome {
                TerminalOutcome::Final { answer_bundle_hash }
                    if answer_bundle_hash == &mutation.answer_bundle_hash =>
                {
                    Ok(CommitOutcome::AlreadyCommitted(outcome.clone()))
                }
                TerminalOutcome::Cancelled => Err(PersistenceError::CancelledBeforeFinal),
                TerminalOutcome::Final { .. } => Err(PersistenceError::FinalConflict),
            };
            if result.is_ok() && !replay {
                record_mutation(run, &mutation.mutation_id, payload_hash);
            }
            return result;
        }
        if run.receipt.cancel_generation != mutation.expected_cancel_generation {
            return Err(PersistenceError::CancelGenerationMismatch {
                expected: mutation.expected_cancel_generation,
                observed: run.receipt.cancel_generation,
            });
        }
        let outcome = TerminalOutcome::Final {
            answer_bundle_hash: mutation.answer_bundle_hash,
        };
        run.terminal = Some(outcome.clone());
        record_mutation(run, &mutation.mutation_id, payload_hash);
        Ok(CommitOutcome::Committed(outcome))
    }

    pub fn request_cancel(
        &self,
        mutation: &CancelMutation,
    ) -> Result<CommitOutcome, PersistenceError> {
        let payload_hash = mutation_payload_hash("request_cancel", mutation)?;
        let mut runs = self.runs.lock().map_err(|_| PersistenceError::Poisoned)?;
        let run = checked_run(&mut runs, &mutation.run_id, mutation.fencing_token)?;
        let replay = validate_mutation(run, &mutation.mutation_id, &payload_hash)?;
        if let Some(outcome) = &run.terminal {
            let outcome = outcome.clone();
            if !replay {
                record_mutation(run, &mutation.mutation_id, payload_hash);
            }
            return Ok(CommitOutcome::AlreadyCommitted(outcome));
        }
        run.receipt.cancel_generation = run.receipt.cancel_generation.saturating_add(1);
        run.receipt.run_version = run.receipt.run_version.saturating_add(1);
        run.terminal = Some(TerminalOutcome::Cancelled);
        record_mutation(run, &mutation.mutation_id, payload_hash);
        Ok(CommitOutcome::Committed(TerminalOutcome::Cancelled))
    }

    pub fn action_count(&self, run_id: &str) -> Result<usize, PersistenceError> {
        let runs = self.runs.lock().map_err(|_| PersistenceError::Poisoned)?;
        Ok(runs
            .get(run_id)
            .ok_or_else(|| PersistenceError::UnknownRun(run_id.into()))?
            .actions
            .len())
    }

    pub fn read_terminal(&self, run_id: &str) -> Result<Option<TerminalOutcome>, PersistenceError> {
        let runs = self.runs.lock().map_err(|_| PersistenceError::Poisoned)?;
        Ok(runs
            .get(run_id)
            .ok_or_else(|| PersistenceError::UnknownRun(run_id.into()))?
            .terminal
            .clone())
    }
}

fn checked_run<'a>(
    runs: &'a mut BTreeMap<String, StoredRun>,
    run_id: &str,
    fencing_token: u64,
) -> Result<&'a mut StoredRun, PersistenceError> {
    let run = runs
        .get_mut(run_id)
        .ok_or_else(|| PersistenceError::UnknownRun(run_id.into()))?;
    if run.receipt.fencing_token != fencing_token {
        return Err(PersistenceError::StaleFence {
            expected: run.receipt.fencing_token,
            observed: fencing_token,
        });
    }
    Ok(run)
}

fn mutation_payload_hash<T: Serialize>(
    operation: &str,
    mutation: &T,
) -> Result<ContentHash, PersistenceError> {
    serde_jcs::to_vec(&(operation, mutation))
        .map(ContentHash::sha256)
        .map_err(|error| PersistenceError::Serialization(error.to_string()))
}

fn validate_mutation(
    run: &StoredRun,
    mutation_id: &str,
    payload_hash: &ContentHash,
) -> Result<bool, PersistenceError> {
    match run.mutations.get(mutation_id) {
        Some(existing) if existing == payload_hash => Ok(true),
        Some(_) => Err(PersistenceError::MutationConflict(mutation_id.into())),
        None => Ok(false),
    }
}

fn record_mutation(run: &mut StoredRun, mutation_id: &str, payload_hash: ContentHash) {
    run.mutations.insert(mutation_id.into(), payload_hash);
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum PersistenceError {
    #[error("persistence mutex was poisoned")]
    Poisoned,
    #[error("run already exists: {0}")]
    RunAlreadyExists(String),
    #[error("unknown run: {0}")]
    UnknownRun(String),
    #[error("stale fence: expected {expected}, observed {observed}")]
    StaleFence { expected: u64, observed: u64 },
    #[error("run is already terminal")]
    TerminalRun,
    #[error("mutation id reused with a different payload: {0}")]
    MutationConflict(String),
    #[error("successful mutation replay has no corresponding stored state: {0}")]
    MutationReplayStateMissing(String),
    #[error("could not serialize persistence mutation: {0}")]
    Serialization(String),
    #[error("action key reused with a different request: {0}")]
    ActionConflict(String),
    #[error("unknown action: {0}")]
    UnknownAction(String),
    #[error("action observation differs from committed observation: {0}")]
    ObservationConflict(String),
    #[error("action finalization has no matching observed result: {0}")]
    FinalizeWithoutMatchingObservation(String),
    #[error("action finalization conflicts with its durable disposition or receipts: {0}")]
    ActionFinalizationConflict(String),
    #[error("bounded child already exists with a different contract")]
    ChildConflict,
    #[error("bounded child does not exist")]
    UnknownChild,
    #[error("bounded child transition failed: {0}")]
    ChildTransition(String),
    #[error("cancel generation mismatch: expected {expected}, observed {observed}")]
    CancelGenerationMismatch { expected: u64, observed: u64 },
    #[error("cancel won before final commit")]
    CancelledBeforeFinal,
    #[error("final mutation conflicts with the committed outcome")]
    FinalConflict,
}

#[cfg(test)]
mod tests {
    use krw_agent_bounded_child::{
        ChildBudgetLimits, ChildBudgetUsage, ChildStage, EvidenceLedgerDeltaRef, SealedInputRef,
        TypedArtifactRef,
    };
    use krw_agent_protocol::BudgetUsage;

    use super::*;

    fn store() -> InMemoryPersistence {
        let store = InMemoryPersistence::default();
        store
            .insert_run(RunReceipt {
                run_id: "run-1".into(),
                fencing_token: 7,
                run_version: 1,
                cancel_generation: 0,
            })
            .unwrap();
        store
    }

    fn child_reservation() -> ReserveChildMutation {
        let capability_call_limits = BTreeMap::from([
            ("ontology.query_context".into(), 1),
            ("ontology.query".into(), 1),
            ("ontology.trace".into(), 1),
        ]);
        ReserveChildMutation {
            mutation_id: "reserve-child".into(),
            run_id: "run-1".into(),
            child_id: "child-1".into(),
            role_id: "company_evidence_researcher".into(),
            fencing_token: 7,
            expected_cancel_generation: 0,
            parent_depth: 0,
            policy_hash: ContentHash::sha256("child-policy"),
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
            baseline_ledger_hash: ContentHash::sha256("baseline-ledger"),
            baseline_evidence_ids: Vec::new(),
        }
    }

    fn invoke_child(receipt: &ChildExecutionReceipt) -> InvokeChildMutation {
        InvokeChildMutation {
            mutation_id: "invoke-child".into(),
            run_id: "run-1".into(),
            child_id: receipt.child_id.clone(),
            fencing_token: 7,
            expected_cancel_generation: 0,
            request_hash: ContentHash::sha256("isolated-request"),
            sealed_input_set_hash: receipt.sealed_input_set_hash.clone(),
        }
    }

    fn complete_child(receipt: &ChildExecutionReceipt) -> CompleteChildMutation {
        CompleteChildMutation {
            mutation_id: "complete-child".into(),
            run_id: "run-1".into(),
            child_id: receipt.child_id.clone(),
            fencing_token: 7,
            expected_cancel_generation: 0,
            output: TypedArtifactRef {
                contract_id: "krw-guru-agent-evidence-analysis/v1".into(),
                content_hash: ContentHash::sha256("typed-child-output"),
            },
            evidence_delta: EvidenceLedgerDeltaRef {
                baseline_ledger_hash: receipt.baseline_ledger_hash.clone(),
                completed_ledger_hash: ContentHash::sha256("completed-ledger"),
                delta_hash: ContentHash::sha256("ledger-delta"),
                evidence_ids: vec!["evidence-1".into()],
            },
            usage: ChildBudgetUsage {
                provider_turns: 2,
                capability_calls: 1,
                input_tokens: 500,
                output_tokens: 100,
                evidence_bytes: 256,
                capability_calls_by_id: BTreeMap::from([
                    ("ontology.query_context".into(), 1),
                    ("ontology.query".into(), 0),
                    ("ontology.trace".into(), 0),
                ]),
            },
        }
    }

    #[test]
    fn child_receipts_survive_each_crash_boundary_and_replay_idempotently() {
        let store = store();
        let reservation = child_reservation();
        let reserved = store.reserve_child(reservation.clone()).unwrap();
        assert_eq!(store.reserve_child(reservation).unwrap(), reserved);
        assert_eq!(store.read_child("run-1").unwrap(), Some(reserved.clone()));

        let invocation = invoke_child(&reserved);
        let invoked = store.invoke_child(&invocation).unwrap();
        assert_eq!(invoked.stage, ChildStage::Invoked);
        assert_eq!(store.invoke_child(&invocation).unwrap(), invoked);
        assert_eq!(store.read_child("run-1").unwrap(), Some(invoked.clone()));

        let completion = complete_child(&invoked);
        let completed = store.complete_child(&completion).unwrap();
        assert_eq!(completed.stage, ChildStage::Completed);
        assert_eq!(store.complete_child(&completion).unwrap(), completed);
        assert_eq!(store.read_child("run-1").unwrap(), Some(completed));
    }

    #[test]
    fn child_cancel_and_stale_fence_are_fail_closed() {
        let store = store();
        let reserved = store.reserve_child(child_reservation()).unwrap();
        let invocation = invoke_child(&reserved);
        let invoked = store.invoke_child(&invocation).unwrap();
        let stale = InvokeChildMutation {
            mutation_id: "stale-child-invoke".into(),
            fencing_token: 6,
            ..invoke_child(&reserved)
        };
        assert!(matches!(
            store.invoke_child(&stale),
            Err(PersistenceError::StaleFence { .. })
        ));

        let cancelled = store
            .cancel_child(&CancelChildMutation {
                mutation_id: "cancel-child".into(),
                run_id: "run-1".into(),
                child_id: invoked.child_id.clone(),
                fencing_token: 7,
                expected_cancel_generation: 0,
                reason_hash: ContentHash::sha256("parent-cancelled"),
            })
            .unwrap();
        assert_eq!(cancelled.stage, ChildStage::Cancelled);
        assert!(matches!(
            store.complete_child(&complete_child(&invoked)),
            Err(PersistenceError::ChildTransition(_))
        ));
    }

    #[test]
    fn receipt_less_committed_action_stage_is_not_deserializable() {
        assert!(serde_json::from_str::<ActionStage>(r#""committed""#).is_err());
    }

    #[test]
    fn same_action_mutation_is_idempotent_but_divergent_payload_conflicts() {
        let store = store();
        let mutation = BeginActionMutation {
            run_id: "run-1".into(),
            fencing_token: 7,
            mutation_id: "m1".into(),
            action_key: "a1".into(),
            request_hash: ContentHash::sha256("request"),
            retryable_read: true,
        };
        let first = store.begin_action(mutation.clone()).unwrap();
        let second = store.begin_action(mutation).unwrap();
        assert_eq!(first, second);
        assert_eq!(store.action_count("run-1").unwrap(), 1);

        let conflict = store.begin_action(BeginActionMutation {
            run_id: "run-1".into(),
            fencing_token: 7,
            mutation_id: "m1".into(),
            action_key: "a1".into(),
            request_hash: ContentHash::sha256("different"),
            retryable_read: true,
        });
        assert!(matches!(
            conflict,
            Err(PersistenceError::MutationConflict(_))
        ));

        let same_hash_different_action = store.begin_action(BeginActionMutation {
            run_id: "run-1".into(),
            fencing_token: 7,
            mutation_id: "m1".into(),
            action_key: "a2".into(),
            request_hash: ContentHash::sha256("request"),
            retryable_read: true,
        });
        assert!(matches!(
            same_hash_different_action,
            Err(PersistenceError::MutationConflict(_))
        ));
    }

    #[test]
    fn rejected_mutation_does_not_reserve_its_id() {
        let store = store();
        let rejected = store.observe_action(ObserveActionMutation {
            run_id: "run-1".into(),
            fencing_token: 7,
            mutation_id: "reusable".into(),
            action_key: "missing".into(),
            result_hash: ContentHash::sha256("result"),
        });
        assert!(matches!(rejected, Err(PersistenceError::UnknownAction(_))));

        let accepted = store.begin_action(BeginActionMutation {
            run_id: "run-1".into(),
            fencing_token: 7,
            mutation_id: "reusable".into(),
            action_key: "a1".into(),
            request_hash: ContentHash::sha256("request"),
            retryable_read: true,
        });
        assert!(accepted.is_ok());
    }

    #[test]
    fn cancel_and_final_linearize_to_one_terminal_outcome() {
        let store = store();
        let cancel = store
            .request_cancel(&CancelMutation {
                run_id: "run-1".into(),
                fencing_token: 7,
                mutation_id: "cancel-1".into(),
            })
            .unwrap();
        assert_eq!(cancel, CommitOutcome::Committed(TerminalOutcome::Cancelled));
        let final_result = store.commit_final(FinalCommitMutation {
            run_id: "run-1".into(),
            fencing_token: 7,
            expected_cancel_generation: 0,
            mutation_id: "final-1".into(),
            answer_bundle_hash: ContentHash::sha256("answer"),
            session_memory_delta_hash: None,
        });
        assert_eq!(final_result, Err(PersistenceError::CancelledBeforeFinal));
        assert_eq!(
            store.read_terminal("run-1").unwrap(),
            Some(TerminalOutcome::Cancelled)
        );
    }

    #[test]
    fn stale_fence_rejects_mutation() {
        let store = store();
        let result = store.begin_action(BeginActionMutation {
            run_id: "run-1".into(),
            fencing_token: 6,
            mutation_id: "m1".into(),
            action_key: "a1".into(),
            request_hash: ContentHash::sha256("request"),
            retryable_read: true,
        });
        assert_eq!(
            result,
            Err(PersistenceError::StaleFence {
                expected: 7,
                observed: 6
            })
        );
    }

    fn observed_action(store: &InMemoryPersistence) -> ContentHash {
        let result_hash = ContentHash::sha256("raw-result");
        store
            .begin_action(BeginActionMutation {
                run_id: "run-1".into(),
                fencing_token: 7,
                mutation_id: "begin-finalize".into(),
                action_key: "action-finalize".into(),
                request_hash: ContentHash::sha256("request"),
                retryable_read: true,
            })
            .unwrap();
        let observed = store
            .observe_action(ObserveActionMutation {
                run_id: "run-1".into(),
                fencing_token: 7,
                mutation_id: "observe-finalize".into(),
                action_key: "action-finalize".into(),
                result_hash: result_hash.clone(),
            })
            .unwrap();
        assert_eq!(observed.stage, ActionStage::Observed);
        result_hash
    }

    fn finalization(
        result_hash: ContentHash,
        disposition: ActionDisposition,
    ) -> FinalizeActionMutation {
        FinalizeActionMutation {
            run_id: "run-1".into(),
            fencing_token: 7,
            mutation_id: "finalize-action".into(),
            action_key: "action-finalize".into(),
            result_hash,
            disposition,
            validation_receipt_hash: ContentHash::sha256("validation-receipt"),
            policy_receipt_hash: ContentHash::sha256("policy-receipt"),
        }
    }

    #[test]
    fn observed_action_is_finalized_with_durable_receipts_and_idempotent_replay() {
        let store = store();
        let result_hash = observed_action(&store);
        let mutation = finalization(result_hash, ActionDisposition::Accepted);
        let first = store.finalize_action(mutation.clone()).unwrap();
        let replay = store.finalize_action(mutation).unwrap();

        assert_eq!(first, replay);
        assert_eq!(first.action.stage, ActionStage::Accepted);
        assert_eq!(
            store
                .read_action_finalization("run-1", "action-finalize")
                .unwrap(),
            Some(first)
        );
    }

    #[test]
    fn crash_after_observe_exposes_raw_receipt_without_implicit_acceptance() {
        let store = store();
        let result_hash = observed_action(&store);
        let recovered = store
            .begin_action(BeginActionMutation {
                run_id: "run-1".into(),
                fencing_token: 7,
                mutation_id: "recover-begin".into(),
                action_key: "action-finalize".into(),
                request_hash: ContentHash::sha256("request"),
                retryable_read: true,
            })
            .unwrap();

        assert_eq!(recovered.stage, ActionStage::Observed);
        assert_eq!(recovered.result_hash, Some(result_hash));
        assert_eq!(
            store
                .read_action_finalization("run-1", "action-finalize")
                .unwrap(),
            None
        );
    }

    #[test]
    fn rejection_is_deny_monotone_and_cannot_be_promoted() {
        let store = store();
        let result_hash = observed_action(&store);
        let rejected = finalization(result_hash.clone(), ActionDisposition::Rejected);
        let receipt = store.finalize_action(rejected).unwrap();
        assert_eq!(receipt.action.stage, ActionStage::Rejected);

        let mut promote = finalization(result_hash, ActionDisposition::Accepted);
        promote.mutation_id = "attempt-promotion".into();
        assert!(matches!(
            store.finalize_action(promote),
            Err(PersistenceError::ActionFinalizationConflict(_))
        ));
    }

    #[test]
    fn finalization_requires_the_exact_observed_result_and_receipts_are_immutable() {
        let store = store();
        let result_hash = observed_action(&store);
        let mut wrong_result = finalization(
            ContentHash::sha256("different-result"),
            ActionDisposition::Accepted,
        );
        wrong_result.mutation_id = "wrong-result".into();
        assert!(matches!(
            store.finalize_action(wrong_result),
            Err(PersistenceError::FinalizeWithoutMatchingObservation(_))
        ));

        let accepted = finalization(result_hash.clone(), ActionDisposition::Accepted);
        store.finalize_action(accepted).unwrap();
        let mut changed_receipt = finalization(result_hash, ActionDisposition::Accepted);
        changed_receipt.mutation_id = "changed-receipt".into();
        changed_receipt.policy_receipt_hash = ContentHash::sha256("different-policy");
        assert!(matches!(
            store.finalize_action(changed_receipt),
            Err(PersistenceError::ActionFinalizationConflict(_))
        ));
    }

    #[test]
    fn stale_fence_cannot_finalize_an_observed_result() {
        let store = store();
        let result_hash = observed_action(&store);
        let mut mutation = finalization(result_hash, ActionDisposition::Accepted);
        mutation.fencing_token = 6;
        assert_eq!(
            store.finalize_action(mutation),
            Err(PersistenceError::StaleFence {
                expected: 7,
                observed: 6,
            })
        );
        assert_eq!(
            store
                .read_action_finalization("run-1", "action-finalize")
                .unwrap(),
            None
        );
    }
}
