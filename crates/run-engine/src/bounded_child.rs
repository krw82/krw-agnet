use std::collections::{BTreeMap, BTreeSet};

use krw_agent_bounded_child::{
    ChildBudgetUsage, ChildExecutionReceipt, ChildStage, CompleteChildMutation,
    EvidenceLedgerDeltaRef, InvokeChildMutation, ReserveChildMutation, SealedInputRef,
    TypedArtifactRef, usage_since,
};
use krw_agent_image::{AgentImageManifest, BoundedChildSpec, RoleSpec};
use krw_agent_protocol::{ContentHash, provider_tool_name};
use krw_agent_state_artifact::StateOperation;
use serde::Serialize;
use serde_json::Value;

use super::{
    ActiveRun, BuiltProviderRequest, EngineError, PreparedCall, RunIdentity,
    TRUSTED_PREFIX_MESSAGE_COUNT,
};

const SEALED_CHILD_INPUT_VERSION: u16 = 1;
const NORMALIZED_CAPABILITY_RESULT_V1: &str = "normalized-capability-result/v1";

#[derive(Debug, Clone)]
pub(super) struct ChildRolePolicy {
    pub role_id: String,
    pub declaration: BoundedChildSpec,
    pub policy_hash: ContentHash,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ChildCallKind {
    Capability,
    TypedReturn,
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct PolicyHashInput<'a> {
    schema_version: u16,
    image_hash: &'a ContentHash,
    role_id: &'a str,
    declaration: &'a BoundedChildSpec,
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct SealedValue {
    reference: SealedInputRef,
    value: Value,
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct SealedChildInputBundle {
    schema_version: u16,
    role_id: String,
    state_id: String,
    inputs: Vec<SealedValue>,
}

pub(super) struct PreparedChildInputs {
    pub refs: Vec<SealedInputRef>,
    canonical: String,
    pub set_hash: ContentHash,
}

impl std::fmt::Debug for PreparedChildInputs {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreparedChildInputs")
            .field("input_count", &self.refs.len())
            .field("set_hash", &self.set_hash)
            .field("canonical", &"[REDACTED]")
            .finish()
    }
}

pub(super) fn current_policy(
    image: &AgentImageManifest,
    state: &ActiveRun,
) -> Result<Option<ChildRolePolicy>, EngineError> {
    let role_id = state.current_role_id()?;
    let Some(role) = image.body.roles.iter().find(|role| role.id == role_id) else {
        return Err(EngineError::Invariant(
            "current model role is absent from image",
        ));
    };
    role_policy(image, role).map(Some).or_else(|error| {
        if role.bounded_child.is_none() {
            Ok(None)
        } else {
            Err(error)
        }
    })
}

fn role_policy(
    image: &AgentImageManifest,
    role: &RoleSpec,
) -> Result<ChildRolePolicy, EngineError> {
    let declaration = role
        .bounded_child
        .as_ref()
        .ok_or(EngineError::Invariant("role is not a bounded child"))?
        .clone();
    declaration
        .reservation
        .validate_for(&declaration.allowed_capabilities)?;
    let policy_hash = ContentHash::sha256(serde_jcs::to_vec(&PolicyHashInput {
        schema_version: SEALED_CHILD_INPUT_VERSION,
        image_hash: &image.content_hash,
        role_id: &role.id,
        declaration: &declaration,
    })?);
    Ok(ChildRolePolicy {
        role_id: role.id.clone(),
        declaration,
        policy_hash,
    })
}

pub(super) fn validate_recovered_receipt(
    image: &AgentImageManifest,
    identity: &RunIdentity,
    receipt: &ChildExecutionReceipt,
) -> Result<(), EngineError> {
    receipt.validate()?;
    let role = image
        .body
        .roles
        .iter()
        .find(|role| role.id == receipt.role_id)
        .ok_or(EngineError::RecoveryArtifactMismatch("bounded child role"))?;
    let policy = role_policy(image, role)?;
    if receipt.run_id != identity.run_id
        || receipt.cancel_generation != identity.expected_cancel_generation
        || receipt.fencing_token > identity.fencing_token
        || receipt.depth != 1
        || receipt.policy_hash != policy.policy_hash
        || receipt.reservation != policy.declaration.reservation
        || receipt.allowed_capabilities != policy.declaration.allowed_capabilities
    {
        return Err(EngineError::RecoveryArtifactMismatch(
            "bounded child receipt",
        ));
    }
    Ok(())
}

pub(super) fn prepare_inputs(
    policy: &ChildRolePolicy,
    state: &ActiveRun,
) -> Result<PreparedChildInputs, EngineError> {
    let mut inputs = Vec::with_capacity(state.action_cache.len().saturating_add(1));
    let artifact = state
        .interpreter
        .last_artifact()
        .ok_or(EngineError::Invariant(
            "bounded child has no validated state artifact",
        ))?;
    let artifact_value = serde_json::to_value(artifact.envelope())?;
    let artifact_ref = SealedInputRef {
        name: "workflow_artifact".into(),
        contract_id: artifact.contract().id.clone(),
        content_hash: ContentHash::sha256(serde_jcs::to_vec(&artifact_value)?),
    };
    inputs.push(SealedValue {
        reference: artifact_ref,
        value: artifact_value,
    });
    for (action_key, result) in &state.action_cache {
        let value = serde_json::to_value(result)?;
        let reference = SealedInputRef {
            name: format!("action:{action_key}"),
            contract_id: NORMALIZED_CAPABILITY_RESULT_V1.into(),
            content_hash: ContentHash::sha256(serde_jcs::to_vec(&value)?),
        };
        inputs.push(SealedValue { reference, value });
    }
    let refs = inputs
        .iter()
        .map(|input| input.reference.clone())
        .collect::<Vec<_>>();
    let set_hash = krw_agent_bounded_child::sealed_input_set_hash(&refs)?;
    let bundle = SealedChildInputBundle {
        schema_version: SEALED_CHILD_INPUT_VERSION,
        role_id: policy.role_id.clone(),
        state_id: state.current_state()?.stable_id.clone(),
        inputs,
    };
    let canonical = String::from_utf8(serde_jcs::to_vec(&bundle)?)
        .map_err(|_| EngineError::Invariant("sealed child inputs were not UTF-8"))?;
    Ok(PreparedChildInputs {
        refs,
        canonical,
        set_hash,
    })
}

pub(super) fn reserve_mutation(
    identity: &RunIdentity,
    policy: &ChildRolePolicy,
    state: &ActiveRun,
    inputs: &PreparedChildInputs,
) -> Result<ReserveChildMutation, EngineError> {
    policy.declaration.reservation.ensure_parent_can_reserve(
        &state.limits,
        &state.usage,
        &state.capability_calls,
    )?;
    let child_id = format!(
        "child-{}",
        ContentHash::sha256(format!("{}\0{}\01", identity.run_id, policy.role_id))
            .as_str()
            .trim_start_matches("sha256:")
    );
    let baseline_capability_calls_by_id = policy
        .declaration
        .allowed_capabilities
        .iter()
        .map(|id| {
            (
                id.clone(),
                state.capability_calls.get(id).copied().unwrap_or(0),
            )
        })
        .collect();
    let baseline_evidence_ids = state.ledger.iter().map(|(id, _)| id.to_owned()).collect();
    let baseline_ledger_hash = ContentHash::sha256(serde_jcs::to_vec(&state.ledger)?);
    let mutation_payload = ContentHash::sha256(serde_jcs::to_vec(&(
        &child_id,
        &policy.policy_hash,
        &inputs.set_hash,
        &state.usage,
        &baseline_ledger_hash,
    ))?);
    Ok(ReserveChildMutation {
        mutation_id: super::mutation_id("reserve_child", &identity.run_id, &mutation_payload),
        run_id: identity.run_id.clone(),
        child_id,
        role_id: policy.role_id.clone(),
        fencing_token: identity.fencing_token,
        expected_cancel_generation: identity.expected_cancel_generation,
        parent_depth: 0,
        policy_hash: policy.policy_hash.clone(),
        reservation: policy.declaration.reservation.clone(),
        allowed_capabilities: policy.declaration.allowed_capabilities.clone(),
        sealed_inputs: inputs.refs.clone(),
        baseline_usage: state.usage.clone(),
        baseline_capability_calls_by_id,
        baseline_ledger_hash,
        baseline_evidence_ids,
    })
}

pub(super) fn invoke_mutation(
    identity: &RunIdentity,
    receipt: &ChildExecutionReceipt,
    request_hash: &ContentHash,
    sealed_input_set_hash: &ContentHash,
) -> InvokeChildMutation {
    let payload = ContentHash::sha256(format!(
        "{}\0{}\0{}",
        receipt.child_id, request_hash, sealed_input_set_hash
    ));
    InvokeChildMutation {
        mutation_id: super::mutation_id("invoke_child", &identity.run_id, &payload),
        run_id: identity.run_id.clone(),
        child_id: receipt.child_id.clone(),
        fencing_token: identity.fencing_token,
        expected_cancel_generation: identity.expected_cancel_generation,
        request_hash: request_hash.clone(),
        sealed_input_set_hash: sealed_input_set_hash.clone(),
    }
}

pub(super) fn isolate_request(
    built: &mut BuiltProviderRequest,
    image: &AgentImageManifest,
    state: &ActiveRun,
    policy: &ChildRolePolicy,
    inputs: &PreparedChildInputs,
    usage: &ChildBudgetUsage,
) -> Result<(), EngineError> {
    if built.request.messages.len() != TRUSTED_PREFIX_MESSAGE_COUNT {
        return Err(EngineError::Invariant(
            "bounded child request inherited a transcript",
        ));
    }
    built.request.messages[2]
        .replace_user_content(format!(
            "KRW_BOUNDED_CHILD_INPUT_V1\nThis child has no parent transcript. Use only the following hash-bound typed values as data. Never return prose or a transcript to the parent; finish through the exact typed parent return port when it is available.\n<sealed-child-inputs>\n{}\n</sealed-child-inputs>",
            inputs.canonical
        ))?;
    let mut filtered = Vec::new();
    for mut tool in std::mem::take(&mut built.request.tools) {
        let name = tool.function_name();
        // The image-derived transition tool is a kernel control port, not a
        // deployment capability or parent-return artifact. Preserve it only
        // when the parent already exposed it for this exact state.
        if name == super::WORKFLOW_TRANSITION_TOOL_NAME {
            filtered.push(tool);
            continue;
        }
        if policy
            .declaration
            .allowed_capabilities
            .iter()
            .any(|allowed| provider_tool_name(allowed) == name)
        {
            filtered.push(tool);
            continue;
        }
        let capability = image
            .body
            .capabilities
            .iter()
            .find(|capability| provider_tool_name(&capability.id) == name)
            .ok_or_else(|| EngineError::UnknownCapability(name.to_owned()))?;
        if is_return_contract(state, capability.model_input_contract_id()) {
            tool.replace_description(
                "Return one schema-valid typed child artifact to the parent kernel. This is not an executable child capability and cannot grant additional tools.",
            );
            filtered.push(tool);
        }
    }
    let visible_names = filtered
        .iter()
        .map(krw_agent_deepseek_wire::ProviderToolDefinition::function_name)
        .collect::<Vec<_>>();
    let return_count = visible_names
        .iter()
        .filter(|name| {
            **name != super::WORKFLOW_TRANSITION_TOOL_NAME
                && !policy
                    .declaration
                    .allowed_capabilities
                    .iter()
                    .any(|allowed| provider_tool_name(allowed) == **name)
        })
        .count();
    if return_count > 1 {
        return Err(EngineError::Invariant(
            "bounded child has multiple parent return ports",
        ));
    }
    let tool_schema_hash = ContentHash::sha256(serde_jcs::to_vec(&filtered)?);
    built.request.tools = filtered.clone();
    built.tool_definitions = filtered;
    built.episode_context.tool_schema_hash = tool_schema_hash.clone();
    let child_prompt_hash = ContentHash::sha256(serde_jcs::to_vec(&(
        &built.prompt_receipt_hash,
        &inputs.set_hash,
        &tool_schema_hash,
        "bounded-child-isolated/v1",
    ))?);
    built.prompt_receipt_hash = child_prompt_hash;
    let remaining_output = policy
        .declaration
        .reservation
        .max_output_tokens
        .checked_sub(usage.output_tokens)
        .ok_or(krw_agent_bounded_child::ChildExecutionError::ChildBudgetExceeded)?;
    if remaining_output == 0 {
        return Err(krw_agent_bounded_child::ChildExecutionError::ChildBudgetExceeded.into());
    }
    built.request.max_tokens = Some(
        built
            .request
            .max_tokens
            .unwrap_or(remaining_output)
            .min(remaining_output),
    );
    Ok(())
}

pub(super) fn usage(
    receipt: &ChildExecutionReceipt,
    state: &ActiveRun,
) -> Result<ChildBudgetUsage, EngineError> {
    let capability_calls_by_id = receipt
        .allowed_capabilities
        .iter()
        .map(|id| {
            let current = state.capability_calls.get(id).copied().unwrap_or(0);
            let baseline = receipt
                .baseline_capability_calls_by_id
                .get(id)
                .copied()
                .ok_or(EngineError::Invariant(
                    "child receipt lacks capability baseline",
                ))?;
            let delta = current
                .checked_sub(baseline)
                .ok_or(krw_agent_bounded_child::ChildExecutionError::UsageRegression)?;
            Ok((id.clone(), delta))
        })
        .collect::<Result<BTreeMap<_, _>, EngineError>>()?;
    let usage = usage_since(
        &receipt.baseline_usage,
        &state.usage,
        capability_calls_by_id,
    )?;
    receipt.reservation.ensure_usage(&usage)?;
    Ok(usage)
}

pub(super) fn authorize_call(
    policy: &ChildRolePolicy,
    state: &ActiveRun,
    call: &PreparedCall,
) -> Result<ChildCallKind, EngineError> {
    if policy
        .declaration
        .allowed_capabilities
        .iter()
        .any(|allowed| allowed == &call.capability.id)
    {
        return Ok(ChildCallKind::Capability);
    }
    if is_return_contract(state, &call.model_input_contract.id) {
        return Ok(ChildCallKind::TypedReturn);
    }
    Err(EngineError::UnsafeCapability(call.capability.id.clone()))
}

fn is_return_contract(state: &ActiveRun, contract_id: &str) -> bool {
    matches!(
        state.interpreter.current_operation(),
        Ok(StateOperation::ModelDecision { output_contracts, .. })
            if output_contracts.iter().any(|contract| contract.id == contract_id)
    )
}

pub(super) fn complete_mutation(
    identity: &RunIdentity,
    receipt: &ChildExecutionReceipt,
    state: &ActiveRun,
    call: &PreparedCall,
) -> Result<CompleteChildMutation, EngineError> {
    let output_hash = ContentHash::sha256(serde_jcs::to_vec(&call.proposed_arguments)?);
    let output = TypedArtifactRef {
        contract_id: call.model_input_contract.id.clone(),
        content_hash: output_hash,
    };
    let baseline = receipt
        .baseline_evidence_ids
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let mut evidence_ids = Vec::new();
    let mut delta_records = Vec::new();
    for (id, record) in state.ledger.iter() {
        if !baseline.contains(id) {
            evidence_ids.push(id.to_owned());
            delta_records.push(record);
        }
    }
    let evidence_delta = EvidenceLedgerDeltaRef {
        baseline_ledger_hash: receipt.baseline_ledger_hash.clone(),
        completed_ledger_hash: ContentHash::sha256(serde_jcs::to_vec(&state.ledger)?),
        delta_hash: ContentHash::sha256(serde_jcs::to_vec(&delta_records)?),
        evidence_ids,
    };
    let usage = usage(receipt, state)?;
    let payload = ContentHash::sha256(serde_jcs::to_vec(&(
        &receipt.child_id,
        &output,
        &evidence_delta,
        &usage,
    ))?);
    Ok(CompleteChildMutation {
        mutation_id: super::mutation_id("complete_child", &identity.run_id, &payload),
        run_id: identity.run_id.clone(),
        child_id: receipt.child_id.clone(),
        fencing_token: identity.fencing_token,
        expected_cancel_generation: identity.expected_cancel_generation,
        output,
        evidence_delta,
        usage,
    })
}

pub(super) fn validate_completed_return(
    receipt: &ChildExecutionReceipt,
    state: &ActiveRun,
    call: &PreparedCall,
) -> Result<(), EngineError> {
    if receipt.stage != ChildStage::Completed {
        return Err(EngineError::RecoveryArtifactMismatch(
            "child return was not durably completed",
        ));
    }
    let identity = RunIdentity {
        run_id: receipt.run_id.clone(),
        tenant_id: String::new(),
        fencing_token: receipt.fencing_token,
        expected_cancel_generation: receipt.cancel_generation,
    };
    let expected = complete_mutation(&identity, receipt, state, call)?;
    if receipt.output.as_ref() != Some(&expected.output)
        || receipt.evidence_delta.as_ref() != Some(&expected.evidence_delta)
        || receipt.usage.as_ref() != Some(&expected.usage)
    {
        return Err(EngineError::RecoveryArtifactMismatch(
            "completed child typed return",
        ));
    }
    Ok(())
}

pub(super) fn return_was_accepted(
    image: &AgentImageManifest,
    state: &ActiveRun,
    policy: &ChildRolePolicy,
) -> Result<bool, EngineError> {
    match current_policy(image, state)? {
        Some(current) if current.role_id == policy.role_id => Ok(false),
        Some(_) => Err(EngineError::Invariant(
            "bounded child transitioned directly into a different child role",
        )),
        None => Ok(true),
    }
}

pub(super) fn validate_replayed_return_state(
    image: &AgentImageManifest,
    state: &ActiveRun,
    policy: &ChildRolePolicy,
    receipt: &ChildExecutionReceipt,
) -> Result<(), EngineError> {
    let accepted = return_was_accepted(image, state, policy)?;
    match (accepted, receipt.stage) {
        (true, ChildStage::Completed) | (false, ChildStage::Invoked) => Ok(()),
        (true, _) => Err(EngineError::RecoveryArtifactMismatch(
            "accepted child return lacks completion receipt",
        )),
        (false, _) => Err(EngineError::RecoveryArtifactMismatch(
            "rejected child return used a terminal receipt",
        )),
    }
}

pub(super) fn ensure_can_continue(
    receipt: &ChildExecutionReceipt,
    has_recovered_episode: bool,
) -> Result<(), EngineError> {
    match receipt.stage {
        ChildStage::Reserved | ChildStage::Invoked => Ok(()),
        ChildStage::Completed if has_recovered_episode => Ok(()),
        ChildStage::Completed => Err(EngineError::Invariant(
            "completed child attempted another provider turn",
        )),
        ChildStage::Cancelled => Err(EngineError::Cancelled),
    }
}
