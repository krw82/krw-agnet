//! Closed, non-recursive child-execution state machine.
//!
//! This crate deliberately contains no provider client, scheduler callback, or
//! transcript field. A child receives only hash-bound typed inputs, owns an
//! explicit reservation carved from its parent, can use an exact read-only
//! capability intersection, and can return only one typed artifact plus an
//! `EvidenceLedger` delta receipt.

use std::collections::{BTreeMap, BTreeSet};

use krw_agent_protocol::{BudgetLimits, BudgetUsage, ContentHash};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const CHILD_EXECUTION_SCHEMA_VERSION: u16 = 1;
pub const MAX_CHILD_CAPABILITIES: usize = 8;
pub const MAX_SEALED_INPUTS: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChildBudgetLimits {
    pub max_provider_turns: u16,
    pub max_capability_calls: u16,
    pub max_input_tokens: u32,
    pub max_output_tokens: u32,
    pub max_evidence_bytes: u64,
    pub capability_call_limits: BTreeMap<String, u16>,
}

impl ChildBudgetLimits {
    pub fn validate_for(&self, allowed_capabilities: &[String]) -> Result<(), ChildExecutionError> {
        if self.max_provider_turns == 0
            || self.max_capability_calls == 0
            || self.max_input_tokens == 0
            || self.max_output_tokens == 0
            || self.max_evidence_bytes == 0
        {
            return Err(ChildExecutionError::InvalidBudget);
        }
        let allowed = allowed_capabilities.iter().collect::<BTreeSet<_>>();
        if self.capability_call_limits.len() != allowed.len()
            || self
                .capability_call_limits
                .iter()
                .any(|(id, limit)| *limit == 0 || !allowed.contains(id))
            || self
                .capability_call_limits
                .values()
                .try_fold(0_u16, |total, value| total.checked_add(*value))
                .is_none_or(|total| total > self.max_capability_calls)
        {
            return Err(ChildExecutionError::InvalidCapabilityIntersection);
        }
        Ok(())
    }

    pub fn ensure_parent_can_reserve(
        &self,
        parent: &BudgetLimits,
        already_used: &BudgetUsage,
        capability_calls_by_id: &BTreeMap<String, u16>,
    ) -> Result<(), ChildExecutionError> {
        let remaining_provider = parent
            .max_provider_turns
            .checked_sub(already_used.provider_turns)
            .ok_or(ChildExecutionError::ParentBudgetInsufficient)?;
        let remaining_calls = parent
            .max_capability_calls
            .checked_sub(already_used.capability_calls)
            .ok_or(ChildExecutionError::ParentBudgetInsufficient)?;
        let remaining_input = parent
            .max_input_tokens
            .checked_sub(already_used.input_tokens)
            .ok_or(ChildExecutionError::ParentBudgetInsufficient)?;
        let remaining_output = parent
            .max_output_tokens
            .checked_sub(already_used.output_tokens)
            .ok_or(ChildExecutionError::ParentBudgetInsufficient)?;
        let remaining_evidence = parent
            .max_evidence_bytes
            .checked_sub(already_used.evidence_bytes)
            .ok_or(ChildExecutionError::ParentBudgetInsufficient)?;
        if self.max_provider_turns > remaining_provider
            || self.max_capability_calls > remaining_calls
            || self.max_input_tokens > remaining_input
            || self.max_output_tokens > remaining_output
            || self.max_evidence_bytes > remaining_evidence
        {
            return Err(ChildExecutionError::ParentBudgetInsufficient);
        }
        for (capability, reserved) in &self.capability_call_limits {
            let parent_limit = parent
                .capability_call_limits
                .get(capability)
                .copied()
                .ok_or(ChildExecutionError::ParentBudgetInsufficient)?;
            let already_used = capability_calls_by_id.get(capability).copied().unwrap_or(0);
            let remaining = parent_limit
                .checked_sub(already_used)
                .ok_or(ChildExecutionError::ParentBudgetInsufficient)?;
            if *reserved > remaining {
                return Err(ChildExecutionError::ParentBudgetInsufficient);
            }
        }
        Ok(())
    }

    pub fn ensure_usage(&self, usage: &ChildBudgetUsage) -> Result<(), ChildExecutionError> {
        let allowed = self.capability_call_limits.keys().collect::<BTreeSet<_>>();
        let observed = usage.capability_calls_by_id.keys().collect::<BTreeSet<_>>();
        if observed != allowed
            || usage.provider_turns > self.max_provider_turns
            || usage.capability_calls > self.max_capability_calls
            || usage.input_tokens > self.max_input_tokens
            || usage.output_tokens > self.max_output_tokens
            || usage.evidence_bytes > self.max_evidence_bytes
            || usage.capability_calls_by_id.iter().any(|(id, used)| {
                self.capability_call_limits
                    .get(id)
                    .is_none_or(|limit| used > limit)
            })
            || usage
                .capability_calls_by_id
                .values()
                .try_fold(0_u16, |total, value| total.checked_add(*value))
                != Some(usage.capability_calls)
        {
            return Err(ChildExecutionError::ChildBudgetExceeded);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChildBudgetUsage {
    pub provider_turns: u16,
    pub capability_calls: u16,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub evidence_bytes: u64,
    pub capability_calls_by_id: BTreeMap<String, u16>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SealedInputRef {
    pub name: String,
    pub contract_id: String,
    pub content_hash: ContentHash,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TypedArtifactRef {
    pub contract_id: String,
    pub content_hash: ContentHash,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceLedgerDeltaRef {
    pub baseline_ledger_hash: ContentHash,
    pub completed_ledger_hash: ContentHash,
    pub delta_hash: ContentHash,
    pub evidence_ids: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChildStage {
    Reserved,
    Invoked,
    Completed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChildExecutionReceipt {
    pub schema_version: u16,
    pub run_id: String,
    pub child_id: String,
    pub role_id: String,
    pub fencing_token: u64,
    pub cancel_generation: u64,
    pub depth: u8,
    pub stage: ChildStage,
    pub policy_hash: ContentHash,
    pub reservation: ChildBudgetLimits,
    pub allowed_capabilities: Vec<String>,
    pub sealed_input_set_hash: ContentHash,
    pub baseline_usage: BudgetUsage,
    pub baseline_capability_calls_by_id: BTreeMap<String, u16>,
    pub baseline_ledger_hash: ContentHash,
    pub baseline_evidence_ids: Vec<String>,
    pub invocation_request_hash: Option<ContentHash>,
    pub output: Option<TypedArtifactRef>,
    pub evidence_delta: Option<EvidenceLedgerDeltaRef>,
    pub usage: Option<ChildBudgetUsage>,
    pub cancellation_reason_hash: Option<ContentHash>,
}

impl ChildExecutionReceipt {
    pub fn validate(&self) -> Result<(), ChildExecutionError> {
        if self.schema_version != CHILD_EXECUTION_SCHEMA_VERSION
            || self.run_id.is_empty()
            || self.child_id.is_empty()
            || self.role_id.is_empty()
            || self.depth != 1
            || self.allowed_capabilities.is_empty()
            || self.allowed_capabilities.len() > MAX_CHILD_CAPABILITIES
            || !is_unique(&self.allowed_capabilities)
            || self.baseline_evidence_ids.len() > 256
            || !is_unique(&self.baseline_evidence_ids)
        {
            return Err(ChildExecutionError::InvalidReceipt);
        }
        let allowed = self.allowed_capabilities.iter().collect::<BTreeSet<_>>();
        if self.baseline_capability_calls_by_id.len() != allowed.len()
            || self
                .baseline_capability_calls_by_id
                .keys()
                .any(|id| !allowed.contains(id))
        {
            return Err(ChildExecutionError::InvalidReceipt);
        }
        self.reservation.validate_for(&self.allowed_capabilities)?;
        match self.stage {
            ChildStage::Reserved => {
                if self.invocation_request_hash.is_some()
                    || self.output.is_some()
                    || self.evidence_delta.is_some()
                    || self.usage.is_some()
                    || self.cancellation_reason_hash.is_some()
                {
                    return Err(ChildExecutionError::InvalidReceipt);
                }
            }
            ChildStage::Invoked => {
                if self.invocation_request_hash.is_none()
                    || self.output.is_some()
                    || self.evidence_delta.is_some()
                    || self.usage.is_some()
                    || self.cancellation_reason_hash.is_some()
                {
                    return Err(ChildExecutionError::InvalidReceipt);
                }
            }
            ChildStage::Completed => {
                let usage = self
                    .usage
                    .as_ref()
                    .ok_or(ChildExecutionError::InvalidReceipt)?;
                if self.invocation_request_hash.is_none()
                    || self.output.is_none()
                    || self.evidence_delta.is_none()
                    || self.cancellation_reason_hash.is_some()
                {
                    return Err(ChildExecutionError::InvalidReceipt);
                }
                let delta = self
                    .evidence_delta
                    .as_ref()
                    .ok_or(ChildExecutionError::InvalidReceipt)?;
                let baseline = self.baseline_evidence_ids.iter().collect::<BTreeSet<_>>();
                if delta.evidence_ids.len() > 256
                    || !is_unique_nonempty(&delta.evidence_ids)
                    || delta.evidence_ids.iter().any(|id| baseline.contains(id))
                {
                    return Err(ChildExecutionError::InvalidReceipt);
                }
                self.reservation.ensure_usage(usage)?;
            }
            ChildStage::Cancelled => {
                if self.output.is_some()
                    || self.evidence_delta.is_some()
                    || self.usage.is_some()
                    || self.cancellation_reason_hash.is_none()
                {
                    return Err(ChildExecutionError::InvalidReceipt);
                }
            }
        }
        Ok(())
    }

    pub fn receipt_hash(&self) -> Result<ContentHash, ChildExecutionError> {
        self.validate()?;
        Ok(ContentHash::sha256(serde_jcs::to_vec(self)?))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReserveChildMutation {
    pub mutation_id: String,
    pub run_id: String,
    pub child_id: String,
    pub role_id: String,
    pub fencing_token: u64,
    pub expected_cancel_generation: u64,
    pub parent_depth: u8,
    pub policy_hash: ContentHash,
    pub reservation: ChildBudgetLimits,
    pub allowed_capabilities: Vec<String>,
    pub sealed_inputs: Vec<SealedInputRef>,
    pub baseline_usage: BudgetUsage,
    pub baseline_capability_calls_by_id: BTreeMap<String, u16>,
    pub baseline_ledger_hash: ContentHash,
    pub baseline_evidence_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InvokeChildMutation {
    pub mutation_id: String,
    pub run_id: String,
    pub child_id: String,
    pub fencing_token: u64,
    pub expected_cancel_generation: u64,
    pub request_hash: ContentHash,
    pub sealed_input_set_hash: ContentHash,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CompleteChildMutation {
    pub mutation_id: String,
    pub run_id: String,
    pub child_id: String,
    pub fencing_token: u64,
    pub expected_cancel_generation: u64,
    pub output: TypedArtifactRef,
    pub evidence_delta: EvidenceLedgerDeltaRef,
    pub usage: ChildBudgetUsage,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CancelChildMutation {
    pub mutation_id: String,
    pub run_id: String,
    pub child_id: String,
    pub fencing_token: u64,
    pub expected_cancel_generation: u64,
    pub reason_hash: ContentHash,
}

pub fn reserve(
    mutation: &ReserveChildMutation,
) -> Result<ChildExecutionReceipt, ChildExecutionError> {
    if mutation.parent_depth != 0 {
        return Err(ChildExecutionError::NestedChildForbidden);
    }
    if mutation.sealed_inputs.is_empty()
        || mutation.sealed_inputs.len() > MAX_SEALED_INPUTS
        || !unique_input_names(&mutation.sealed_inputs)
        || mutation.allowed_capabilities.is_empty()
        || mutation.allowed_capabilities.len() > MAX_CHILD_CAPABILITIES
        || !is_unique(&mutation.allowed_capabilities)
    {
        return Err(ChildExecutionError::InvalidReservation);
    }
    let allowed = mutation
        .allowed_capabilities
        .iter()
        .collect::<BTreeSet<_>>();
    if mutation.baseline_capability_calls_by_id.len() != allowed.len()
        || mutation
            .baseline_capability_calls_by_id
            .keys()
            .any(|id| !allowed.contains(id))
        || mutation.baseline_evidence_ids.len() > 256
        || !is_unique(&mutation.baseline_evidence_ids)
    {
        return Err(ChildExecutionError::InvalidReservation);
    }
    mutation
        .reservation
        .validate_for(&mutation.allowed_capabilities)?;
    let sealed_input_set_hash = sealed_input_set_hash(&mutation.sealed_inputs)?;
    let receipt = ChildExecutionReceipt {
        schema_version: CHILD_EXECUTION_SCHEMA_VERSION,
        run_id: mutation.run_id.clone(),
        child_id: mutation.child_id.clone(),
        role_id: mutation.role_id.clone(),
        fencing_token: mutation.fencing_token,
        cancel_generation: mutation.expected_cancel_generation,
        depth: 1,
        stage: ChildStage::Reserved,
        policy_hash: mutation.policy_hash.clone(),
        reservation: mutation.reservation.clone(),
        allowed_capabilities: mutation.allowed_capabilities.clone(),
        sealed_input_set_hash,
        baseline_usage: mutation.baseline_usage.clone(),
        baseline_capability_calls_by_id: mutation.baseline_capability_calls_by_id.clone(),
        baseline_ledger_hash: mutation.baseline_ledger_hash.clone(),
        baseline_evidence_ids: mutation.baseline_evidence_ids.clone(),
        invocation_request_hash: None,
        output: None,
        evidence_delta: None,
        usage: None,
        cancellation_reason_hash: None,
    };
    receipt.validate()?;
    Ok(receipt)
}

pub fn invoke(
    receipt: &ChildExecutionReceipt,
    mutation: &InvokeChildMutation,
) -> Result<ChildExecutionReceipt, ChildExecutionError> {
    validate_identity(
        receipt,
        &mutation.run_id,
        &mutation.child_id,
        mutation.fencing_token,
        mutation.expected_cancel_generation,
    )?;
    if receipt.stage == ChildStage::Invoked {
        if receipt.invocation_request_hash.as_ref() == Some(&mutation.request_hash)
            && receipt.sealed_input_set_hash == mutation.sealed_input_set_hash
        {
            return Ok(receipt.clone());
        }
        return Err(ChildExecutionError::TransitionConflict);
    }
    if receipt.stage != ChildStage::Reserved
        || receipt.sealed_input_set_hash != mutation.sealed_input_set_hash
    {
        return Err(ChildExecutionError::InvalidTransition);
    }
    let mut next = receipt.clone();
    next.fencing_token = mutation.fencing_token;
    next.stage = ChildStage::Invoked;
    next.invocation_request_hash = Some(mutation.request_hash.clone());
    next.validate()?;
    Ok(next)
}

pub fn complete(
    receipt: &ChildExecutionReceipt,
    mutation: &CompleteChildMutation,
) -> Result<ChildExecutionReceipt, ChildExecutionError> {
    validate_identity(
        receipt,
        &mutation.run_id,
        &mutation.child_id,
        mutation.fencing_token,
        mutation.expected_cancel_generation,
    )?;
    if receipt.stage == ChildStage::Completed {
        if receipt.output.as_ref() == Some(&mutation.output)
            && receipt.evidence_delta.as_ref() == Some(&mutation.evidence_delta)
            && receipt.usage.as_ref() == Some(&mutation.usage)
        {
            return Ok(receipt.clone());
        }
        return Err(ChildExecutionError::TransitionConflict);
    }
    if receipt.stage != ChildStage::Invoked
        || mutation.evidence_delta.baseline_ledger_hash != receipt.baseline_ledger_hash
    {
        return Err(ChildExecutionError::InvalidTransition);
    }
    receipt.reservation.ensure_usage(&mutation.usage)?;
    let mut next = receipt.clone();
    next.fencing_token = mutation.fencing_token;
    next.stage = ChildStage::Completed;
    next.output = Some(mutation.output.clone());
    next.evidence_delta = Some(mutation.evidence_delta.clone());
    next.usage = Some(mutation.usage.clone());
    next.validate()?;
    Ok(next)
}

pub fn cancel(
    receipt: &ChildExecutionReceipt,
    mutation: &CancelChildMutation,
) -> Result<ChildExecutionReceipt, ChildExecutionError> {
    validate_identity(
        receipt,
        &mutation.run_id,
        &mutation.child_id,
        mutation.fencing_token,
        mutation.expected_cancel_generation,
    )?;
    if receipt.stage == ChildStage::Completed {
        return Err(ChildExecutionError::TerminalChild);
    }
    if receipt.stage == ChildStage::Cancelled {
        if receipt.cancellation_reason_hash.as_ref() == Some(&mutation.reason_hash) {
            return Ok(receipt.clone());
        }
        return Err(ChildExecutionError::TransitionConflict);
    }
    let mut next = receipt.clone();
    next.fencing_token = mutation.fencing_token;
    next.stage = ChildStage::Cancelled;
    next.output = None;
    next.evidence_delta = None;
    next.usage = None;
    next.cancellation_reason_hash = Some(mutation.reason_hash.clone());
    next.validate()?;
    Ok(next)
}

pub fn sealed_input_set_hash(
    inputs: &[SealedInputRef],
) -> Result<ContentHash, ChildExecutionError> {
    if inputs.is_empty() || inputs.len() > MAX_SEALED_INPUTS || !unique_input_names(inputs) {
        return Err(ChildExecutionError::InvalidReservation);
    }
    Ok(ContentHash::sha256(serde_jcs::to_vec(inputs)?))
}

pub fn usage_since(
    baseline: &BudgetUsage,
    current: &BudgetUsage,
    capability_calls_by_id: BTreeMap<String, u16>,
) -> Result<ChildBudgetUsage, ChildExecutionError> {
    if current.capability_calls < baseline.capability_calls {
        return Err(ChildExecutionError::UsageRegression);
    }
    let capability_calls = capability_calls_by_id
        .values()
        .try_fold(0_u16, |total, value| total.checked_add(*value))
        .ok_or(ChildExecutionError::ChildBudgetExceeded)?;
    let usage = ChildBudgetUsage {
        provider_turns: current
            .provider_turns
            .checked_sub(baseline.provider_turns)
            .ok_or(ChildExecutionError::UsageRegression)?,
        // Parent-only boundary capabilities may execute while the child is
        // invoked. Only the exact allowed-capability intersection belongs to
        // the child reservation; the parent still accounts every call in its
        // aggregate run budget.
        capability_calls,
        input_tokens: current
            .input_tokens
            .checked_sub(baseline.input_tokens)
            .ok_or(ChildExecutionError::UsageRegression)?,
        output_tokens: current
            .output_tokens
            .checked_sub(baseline.output_tokens)
            .ok_or(ChildExecutionError::UsageRegression)?,
        evidence_bytes: current
            .evidence_bytes
            .checked_sub(baseline.evidence_bytes)
            .ok_or(ChildExecutionError::UsageRegression)?,
        capability_calls_by_id,
    };
    Ok(usage)
}

fn validate_identity(
    receipt: &ChildExecutionReceipt,
    run_id: &str,
    child_id: &str,
    fencing_token: u64,
    cancel_generation: u64,
) -> Result<(), ChildExecutionError> {
    receipt.validate()?;
    if receipt.run_id != run_id || receipt.child_id != child_id {
        return Err(ChildExecutionError::IdentityMismatch);
    }
    if receipt.fencing_token > fencing_token {
        return Err(ChildExecutionError::StaleFence);
    }
    if receipt.cancel_generation != cancel_generation {
        return Err(ChildExecutionError::CancelGenerationMismatch);
    }
    Ok(())
}

fn unique_input_names(inputs: &[SealedInputRef]) -> bool {
    inputs
        .iter()
        .all(|input| !input.name.is_empty() && !input.contract_id.is_empty())
        && inputs
            .iter()
            .map(|input| input.name.as_str())
            .collect::<BTreeSet<_>>()
            .len()
            == inputs.len()
}

fn is_unique(values: &[String]) -> bool {
    values.iter().collect::<BTreeSet<_>>().len() == values.len()
}

fn is_unique_nonempty(values: &[String]) -> bool {
    values.iter().all(|value| !value.is_empty()) && is_unique(values)
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ChildExecutionError {
    #[error("nested child invocation is forbidden")]
    NestedChildForbidden,
    #[error("child reservation is invalid")]
    InvalidReservation,
    #[error("child budget is invalid")]
    InvalidBudget,
    #[error("child capability intersection is invalid")]
    InvalidCapabilityIntersection,
    #[error("parent has insufficient unreserved budget")]
    ParentBudgetInsufficient,
    #[error("child budget was exceeded")]
    ChildBudgetExceeded,
    #[error("child receipt is invalid")]
    InvalidReceipt,
    #[error("child transition is invalid")]
    InvalidTransition,
    #[error("child transition conflicts with a durable receipt")]
    TransitionConflict,
    #[error("child identity does not match")]
    IdentityMismatch,
    #[error("child receipt uses a stale fence")]
    StaleFence,
    #[error("child cancel generation changed")]
    CancelGenerationMismatch,
    #[error("child is already terminal")]
    TerminalChild,
    #[error("child usage counters regressed")]
    UsageRegression,
    #[error("could not encode canonical child receipt")]
    CanonicalEncoding,
}

impl From<serde_json::Error> for ChildExecutionError {
    fn from(_: serde_json::Error) -> Self {
        Self::CanonicalEncoding
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash(label: &str) -> ContentHash {
        ContentHash::sha256(label)
    }

    fn budget() -> ChildBudgetLimits {
        ChildBudgetLimits {
            max_provider_turns: 4,
            max_capability_calls: 3,
            max_input_tokens: 10_000,
            max_output_tokens: 2_000,
            max_evidence_bytes: 1_048_576,
            capability_call_limits: BTreeMap::from([
                ("ontology.query_context".into(), 1),
                ("ontology.query".into(), 1),
                ("ontology.trace".into(), 1),
            ]),
        }
    }

    fn reservation(depth: u8) -> ReserveChildMutation {
        ReserveChildMutation {
            mutation_id: "reserve-child".into(),
            run_id: "run-1".into(),
            child_id: "run-1:company-evidence:1".into(),
            role_id: "company_evidence_researcher".into(),
            fencing_token: 7,
            expected_cancel_generation: 2,
            parent_depth: depth,
            policy_hash: hash("policy"),
            reservation: budget(),
            allowed_capabilities: vec![
                "ontology.query_context".into(),
                "ontology.query".into(),
                "ontology.trace".into(),
            ],
            sealed_inputs: vec![SealedInputRef {
                name: "sealed_brief".into(),
                contract_id: "krw-guru-investigation-brief/v1".into(),
                content_hash: hash("brief"),
            }],
            baseline_usage: BudgetUsage::default(),
            baseline_capability_calls_by_id: BTreeMap::from([
                ("ontology.query_context".into(), 0),
                ("ontology.query".into(), 0),
                ("ontology.trace".into(), 0),
            ]),
            baseline_ledger_hash: hash("ledger-before"),
            baseline_evidence_ids: Vec::new(),
        }
    }

    #[test]
    fn happy_path_has_no_transcript_carrier_and_is_hash_stable() {
        let reserved = reserve(&reservation(0)).unwrap();
        let invoked = invoke(
            &reserved,
            &InvokeChildMutation {
                mutation_id: "invoke-child".into(),
                run_id: "run-1".into(),
                child_id: reserved.child_id.clone(),
                fencing_token: 7,
                expected_cancel_generation: 2,
                request_hash: hash("request"),
                sealed_input_set_hash: reserved.sealed_input_set_hash.clone(),
            },
        )
        .unwrap();
        let completed = complete(
            &invoked,
            &CompleteChildMutation {
                mutation_id: "complete-child".into(),
                run_id: "run-1".into(),
                child_id: invoked.child_id.clone(),
                fencing_token: 7,
                expected_cancel_generation: 2,
                output: TypedArtifactRef {
                    contract_id: "krw-guru-agent-evidence-analysis/v1".into(),
                    content_hash: hash("typed-output"),
                },
                evidence_delta: EvidenceLedgerDeltaRef {
                    baseline_ledger_hash: hash("ledger-before"),
                    completed_ledger_hash: hash("ledger-after"),
                    delta_hash: hash("delta"),
                    evidence_ids: vec!["evidence-1".into()],
                },
                usage: ChildBudgetUsage {
                    provider_turns: 2,
                    capability_calls: 1,
                    input_tokens: 500,
                    output_tokens: 200,
                    evidence_bytes: 100,
                    capability_calls_by_id: BTreeMap::from([
                        ("ontology.query_context".into(), 1),
                        ("ontology.query".into(), 0),
                        ("ontology.trace".into(), 0),
                    ]),
                },
            },
        )
        .unwrap();
        assert_eq!(completed.stage, ChildStage::Completed);
        assert_eq!(
            completed.receipt_hash().unwrap(),
            completed.receipt_hash().unwrap()
        );
        let encoded = serde_jcs::to_vec(&completed).unwrap();
        assert!(!String::from_utf8(encoded).unwrap().contains("transcript"));
    }

    #[test]
    fn child_of_child_is_fail_closed() {
        assert_eq!(
            reserve(&reservation(1)),
            Err(ChildExecutionError::NestedChildForbidden)
        );
    }

    #[test]
    fn exact_capability_intersection_and_per_cap_budget_are_enforced() {
        let mut invalid = reservation(0);
        invalid
            .reservation
            .capability_call_limits
            .insert("guru.review_company_evidence".into(), 1);
        assert_eq!(
            reserve(&invalid),
            Err(ChildExecutionError::InvalidCapabilityIntersection)
        );
    }

    #[test]
    fn stale_fence_cancel_race_and_completion_conflicts_fail_closed() {
        let reserved = reserve(&reservation(0)).unwrap();
        let stale = InvokeChildMutation {
            mutation_id: "stale-invoke".into(),
            run_id: "run-1".into(),
            child_id: reserved.child_id.clone(),
            fencing_token: 6,
            expected_cancel_generation: 2,
            request_hash: hash("request"),
            sealed_input_set_hash: reserved.sealed_input_set_hash.clone(),
        };
        assert_eq!(
            invoke(&reserved, &stale),
            Err(ChildExecutionError::StaleFence)
        );

        let recovered = invoke(
            &reserved,
            &InvokeChildMutation {
                mutation_id: "recovered-invoke".into(),
                run_id: "run-1".into(),
                child_id: reserved.child_id.clone(),
                fencing_token: 8,
                expected_cancel_generation: 2,
                request_hash: hash("recovered-request"),
                sealed_input_set_hash: reserved.sealed_input_set_hash.clone(),
            },
        )
        .unwrap();
        assert_eq!(recovered.fencing_token, 8);

        let cancelled = cancel(
            &reserved,
            &CancelChildMutation {
                mutation_id: "cancel-child".into(),
                run_id: "run-1".into(),
                child_id: reserved.child_id.clone(),
                fencing_token: 7,
                expected_cancel_generation: 2,
                reason_hash: hash("cancelled"),
            },
        )
        .unwrap();
        assert_eq!(
            invoke(
                &cancelled,
                &InvokeChildMutation {
                    mutation_id: "invoke-after-cancel".into(),
                    run_id: "run-1".into(),
                    child_id: cancelled.child_id.clone(),
                    fencing_token: 7,
                    expected_cancel_generation: 2,
                    request_hash: hash("request"),
                    sealed_input_set_hash: cancelled.sealed_input_set_hash.clone(),
                }
            ),
            Err(ChildExecutionError::InvalidTransition)
        );
    }

    #[test]
    fn reservation_is_rejected_when_parent_cannot_cover_it() {
        let limits = BudgetLimits {
            max_provider_turns: 4,
            max_capability_calls: 3,
            max_replans: 0,
            max_repairs: 0,
            max_input_tokens: 10_000,
            max_output_tokens: 2_000,
            max_evidence_bytes: 1_048_576,
            deadline_ms: 1_000,
            capability_call_limits: budget().capability_call_limits,
        };
        let used = BudgetUsage {
            provider_turns: 1,
            ..BudgetUsage::default()
        };
        assert_eq!(
            budget().ensure_parent_can_reserve(&limits, &used, &BTreeMap::new()),
            Err(ChildExecutionError::ParentBudgetInsufficient)
        );
    }

    #[test]
    fn reservation_checks_remaining_parent_per_capability_budget() {
        let limits = BudgetLimits {
            max_provider_turns: 8,
            max_capability_calls: 10,
            max_replans: 0,
            max_repairs: 0,
            max_input_tokens: 20_000,
            max_output_tokens: 4_000,
            max_evidence_bytes: 2_097_152,
            deadline_ms: 1_000,
            capability_call_limits: budget().capability_call_limits,
        };
        let used_by_id = BTreeMap::from([("ontology.query_context".into(), 1)]);
        assert_eq!(
            budget().ensure_parent_can_reserve(&limits, &BudgetUsage::default(), &used_by_id,),
            Err(ChildExecutionError::ParentBudgetInsufficient)
        );
    }

    #[test]
    fn parent_boundary_calls_do_not_consume_child_capability_reservation() {
        let baseline = BudgetUsage {
            capability_calls: 2,
            ..BudgetUsage::default()
        };
        let current = BudgetUsage {
            provider_turns: 1,
            capability_calls: 5,
            input_tokens: 100,
            output_tokens: 50,
            evidence_bytes: 80,
            ..BudgetUsage::default()
        };
        let usage = usage_since(
            &baseline,
            &current,
            BTreeMap::from([
                ("ontology.query_context".into(), 1),
                ("ontology.query".into(), 0),
                ("ontology.trace".into(), 0),
            ]),
        )
        .unwrap();
        assert_eq!(usage.capability_calls, 1);
        budget().ensure_usage(&usage).unwrap();
    }
}
