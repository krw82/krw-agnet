//! Generic durable execution and action-authorization state machines.

use krw_agent_persistence::{ActionReceipt, ActionStage};
use krw_agent_protocol::ContentHash;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionState {
    Queued,
    Claimed,
    Admitted,
    Running,
    Verifying,
    Committing,
    Succeeded,
    RetryWait,
    Cancelling,
    Failed,
}

impl ExecutionState {
    pub fn transition(self, event: ExecutionEvent) -> Result<Self, KernelError> {
        let next = match (self, event) {
            (Self::Queued, ExecutionEvent::Claim) => Self::Claimed,
            (Self::Claimed, ExecutionEvent::Admit) => Self::Admitted,
            (Self::Admitted, ExecutionEvent::Start) | (Self::RetryWait, ExecutionEvent::Resume) => {
                Self::Running
            }
            (Self::Running, ExecutionEvent::BeginVerification) => Self::Verifying,
            (Self::Verifying, ExecutionEvent::BeginCommit) => Self::Committing,
            (Self::Committing, ExecutionEvent::CommitSucceeded) => Self::Succeeded,
            (
                Self::Queued | Self::Claimed | Self::Admitted | Self::Running | Self::Verifying,
                ExecutionEvent::Retry,
            ) => Self::RetryWait,
            (
                Self::Queued
                | Self::Claimed
                | Self::Admitted
                | Self::Running
                | Self::Verifying
                | Self::RetryWait,
                ExecutionEvent::Cancel,
            ) => Self::Cancelling,
            (Self::Cancelling, ExecutionEvent::CancelCommitted)
            | (
                Self::Queued
                | Self::Claimed
                | Self::Admitted
                | Self::Running
                | Self::Verifying
                | Self::Committing
                | Self::RetryWait
                | Self::Cancelling,
                ExecutionEvent::Fail,
            ) => Self::Failed,
            _ => return Err(KernelError::InvalidExecutionTransition { state: self, event }),
        };
        Ok(next)
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionEvent {
    Claim,
    Admit,
    Start,
    BeginVerification,
    BeginCommit,
    CommitSucceeded,
    Retry,
    Resume,
    Cancel,
    CancelCommitted,
    Fail,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchStage {
    Proposed,
    EpisodeCommitted,
    ActionBegun,
    Dispatched,
    Observed,
    Accepted,
    Ambiguous,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizedAction {
    pub action_key: String,
    pub request_hash: ContentHash,
    pub episode_hash: ContentHash,
    pub stage: DispatchStage,
    pub retryable_read: bool,
    pub result_hash: Option<ContentHash>,
}

impl AuthorizedAction {
    pub fn proposed(
        action_key: String,
        request_hash: ContentHash,
        episode_hash: ContentHash,
    ) -> Self {
        Self {
            action_key,
            request_hash,
            episode_hash,
            stage: DispatchStage::Proposed,
            retryable_read: false,
            result_hash: None,
        }
    }

    pub fn episode_committed(&mut self) -> Result<(), KernelError> {
        self.require_stage(DispatchStage::Proposed)?;
        self.stage = DispatchStage::EpisodeCommitted;
        Ok(())
    }

    pub fn bind_receipt(&mut self, receipt: &ActionReceipt) -> Result<(), KernelError> {
        self.require_stage(DispatchStage::EpisodeCommitted)?;
        if receipt.action_key != self.action_key || receipt.request_hash != self.request_hash {
            return Err(KernelError::ActionReceiptMismatch);
        }
        if receipt.stage != ActionStage::Begun {
            return Err(KernelError::ActionReceiptMismatch);
        }
        self.retryable_read = receipt.retryable_read;
        self.stage = DispatchStage::ActionBegun;
        Ok(())
    }

    pub fn mark_dispatched(&mut self) -> Result<(), KernelError> {
        self.require_stage(DispatchStage::ActionBegun)?;
        self.stage = DispatchStage::Dispatched;
        Ok(())
    }

    pub fn observe(&mut self, result_hash: ContentHash) -> Result<(), KernelError> {
        self.require_stage(DispatchStage::Dispatched)?;
        self.result_hash = Some(result_hash);
        self.stage = DispatchStage::Observed;
        Ok(())
    }

    pub fn accept(&mut self, result_hash: &ContentHash) -> Result<(), KernelError> {
        self.require_stage(DispatchStage::Observed)?;
        if self.result_hash.as_ref() != Some(result_hash) {
            return Err(KernelError::ActionResultMismatch);
        }
        self.stage = DispatchStage::Accepted;
        Ok(())
    }

    pub fn mark_ambiguous(&mut self) -> Result<(), KernelError> {
        if self.stage != DispatchStage::Dispatched {
            return Err(KernelError::InvalidActionTransition {
                from: self.stage,
                to: DispatchStage::Ambiguous,
            });
        }
        self.stage = DispatchStage::Ambiguous;
        Ok(())
    }

    pub fn may_retry_after_ambiguity(&self) -> bool {
        self.stage == DispatchStage::Ambiguous && self.retryable_read
    }

    fn require_stage(&self, expected: DispatchStage) -> Result<(), KernelError> {
        if self.stage == expected {
            Ok(())
        } else {
            Err(KernelError::InvalidActionTransition {
                from: self.stage,
                to: expected,
            })
        }
    }
}

#[derive(Debug, Error)]
pub enum KernelError {
    #[error("invalid execution transition from {state:?} on {event:?}")]
    InvalidExecutionTransition {
        state: ExecutionState,
        event: ExecutionEvent,
    },
    #[error("invalid action transition from {from:?} to {to:?}")]
    InvalidActionTransition {
        from: DispatchStage,
        to: DispatchStage,
    },
    #[error("durable action receipt does not match proposed action")]
    ActionReceiptMismatch,
    #[error("observed action result does not match commit")]
    ActionResultMismatch,
    #[error(transparent)]
    Contract(#[from] krw_agent_protocol::ContractError),
}

#[cfg(test)]
mod tests {
    use krw_agent_persistence::{ActionReceipt, ActionStage};

    use super::*;

    #[test]
    fn execution_commit_requires_verification() {
        let running = ExecutionState::Queued
            .transition(ExecutionEvent::Claim)
            .unwrap()
            .transition(ExecutionEvent::Admit)
            .unwrap()
            .transition(ExecutionEvent::Start)
            .unwrap();
        assert!(running.transition(ExecutionEvent::BeginCommit).is_err());
        let succeeded = running
            .transition(ExecutionEvent::BeginVerification)
            .unwrap()
            .transition(ExecutionEvent::BeginCommit)
            .unwrap()
            .transition(ExecutionEvent::CommitSucceeded)
            .unwrap();
        assert_eq!(succeeded, ExecutionState::Succeeded);
    }

    #[test]
    fn dispatch_requires_episode_checkpoint_and_action_receipt() {
        let request = ContentHash::sha256("request");
        let mut action = AuthorizedAction::proposed(
            "a1".into(),
            request.clone(),
            ContentHash::sha256("episode"),
        );
        assert!(action.mark_dispatched().is_err());
        action.episode_committed().unwrap();
        assert!(action.mark_dispatched().is_err());
        action
            .bind_receipt(&ActionReceipt {
                action_key: "a1".into(),
                mutation_id: "m1".into(),
                request_hash: request,
                result_hash: None,
                stage: ActionStage::Begun,
                retryable_read: true,
            })
            .unwrap();
        action.mark_dispatched().unwrap();
        action.mark_ambiguous().unwrap();
        assert!(action.may_retry_after_ambiguity());
    }
}
