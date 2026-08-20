//! Capability dispatch boundary: prepared-call assembly from a committed
//! provider episode, canonical action keys and durable action receipts,
//! argument derivation from image-pinned input contracts, and the AfterAction
//! validation/policy gate that admits an observed capability result.

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResearchDispatchDecision {
    Execute { selected_index: usize },
    NoPositiveValue(ResearchStopReason),
    ProposalRejected(NoPositiveReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResearchStopReason {
    NoFrontier,
    ReplanBudgetExhausted,
}

pub(crate) const fn rejection_reason_code(reason: NoPositiveReason) -> &'static str {
    match reason {
        NoPositiveReason::NoFrontier => "no_frontier",
        NoPositiveReason::DuplicateCompleted => "duplicate_completed",
        NoPositiveReason::ProposalUnmapped => "proposal_unmapped",
        NoPositiveReason::NonPositiveScore => "non_positive_score",
    }
}

pub(crate) fn research_fingerprint(call: &PreparedCall) -> ContentHash {
    ContentHash::sha256(call.action_key.as_bytes())
}

/// Convert an image-pinned action policy into the planner's runtime scoring
/// input. There is deliberately no capability-id or evidence-mapping switch
/// here: adding a new research tool means declaring its semantic kind, cost,
/// and conflict domain in the `AgentImage`.
pub(crate) fn research_candidate(
    call: &PreparedCall,
    policy: &ResearchActionPolicy,
) -> CandidateProposal {
    CandidateProposal {
        proposal_id: call.tool_call_id.clone(),
        capability_id: call.capability.id.clone(),
        kind: match policy.kind {
            ImageResearchActionKind::Context => ResearchActionKind::QueryContext,
            ImageResearchActionKind::Targeted => ResearchActionKind::TargetedQuery,
            ImageResearchActionKind::Trace => ResearchActionKind::Trace,
        },
        fingerprint: research_fingerprint(call),
        arguments: call.arguments.clone(),
        estimate: CandidateEstimate {
            historical_success_lower_ppm: policy.estimate.historical_success_lower_ppm,
            expected_duplicate_ppm: policy.estimate.expected_duplicate_ppm,
            failure_risk_upper_ppm: policy.estimate.failure_risk_upper_ppm,
            expected_latency_ms: policy.estimate.expected_latency_ms,
            expected_tokens: policy.estimate.expected_tokens,
            expected_tool_cost_micros: policy.estimate.expected_tool_cost_micros,
            expected_result_bytes: policy.estimate.expected_result_bytes,
        },
        effect: ActionEffect::ReadOnly,
        concurrency: ActionConcurrency::Serial,
        auth_isolation: AuthIsolation::Isolated,
        conflict_keys: vec![policy.conflict_domain.clone()],
    }
}

pub(crate) fn is_input_correction(result: &CapabilityResult) -> bool {
    result
        .provider_content
        .get("status")
        .and_then(Value::as_str)
        == Some("input_correction_required")
        || result
            .provider_content
            .get("violations")
            .and_then(Value::as_array)
            .is_some_and(|violations| !violations.is_empty())
}

pub(crate) fn capability_result_completes_prerequisite(result: &CapabilityResult) -> bool {
    !is_input_correction(result)
}

pub(crate) fn capability_result_cacheable(
    kind: Option<ImageResearchActionKind>,
    result: &CapabilityResult,
) -> bool {
    let Some(kind) = kind else {
        return true;
    };
    match kind {
        ImageResearchActionKind::Context => true,
        ImageResearchActionKind::Targeted => {
            supplemental_status_for_targeted_payload(&result.provider_content)
                .map(|status| {
                    matches!(
                        status.kind,
                        SupplementalReadKind::Retrieved
                            | SupplementalReadKind::Empty
                            | SupplementalReadKind::NotFound
                    )
                })
                .unwrap_or(false)
        }
        ImageResearchActionKind::Trace => {
            supplemental_status_for_trace_payload(&result.provider_content)
                .map(|status| {
                    matches!(
                        status.kind,
                        SupplementalReadKind::Retrieved
                            | SupplementalReadKind::Empty
                            | SupplementalReadKind::NotFound
                    )
                })
                .unwrap_or(false)
        }
    }
}

#[derive(Debug)]
pub(crate) struct AfterActionEvaluation {
    pub(crate) accepted: bool,
    validation_receipt_hash: ContentHash,
    policy_receipt_hash: ContentHash,
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct AfterActionValidationReceipt<'a> {
    schema_version: u16,
    action_key: &'a str,
    capability_id: &'a str,
    request_hash: &'a ContentHash,
    result_hash: &'a ContentHash,
    output_contract_set_hash: &'a ContentHash,
    data_release_hash: &'a ContentHash,
    issues: &'a BTreeSet<String>,
}

pub(crate) struct ActionExecutionContext<'a> {
    pub(crate) identity: &'a RunIdentity,
    pub(crate) episode_hash: &'a ContentHash,
    pub(crate) image: &'a LoadedImage,
    pub(crate) state: &'a ActiveRun,
    pub(crate) deadline: Instant,
    pub(crate) max_result_bytes: usize,
}

#[derive(Clone)]
pub(crate) struct PreparedCall {
    pub(crate) tool_call_id: String,
    pub(crate) capability: CapabilitySpec,
    pub(crate) binding: CapabilityBinding,
    pub(crate) contracts: ResolvedCapabilityContracts,
    pub(crate) model_input_contract: ContractPin,
    pub(crate) normalized_output_contract_hash: ContentHash,
    pub(crate) proposed_arguments: Value,
    pub(crate) arguments: Value,
    /// Semantic receipt emitted only by the closed `ResearchIntent →
    /// SearchPlan` compiler. It is not provider input and does not cross the
    /// MCP transport boundary.
    pub(crate) research_intent_receipt: Option<ResearchIntentReceipt>,
    pub(crate) canonical_arguments: Vec<u8>,
    pub(crate) request_hash: ContentHash,
    pub(crate) action_key: String,
    pub(crate) state_id: String,
}

impl fmt::Debug for PreparedCall {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedCall")
            .field("tool_call_id", &self.tool_call_id)
            .field("capability_id", &self.capability.id)
            .field("request_hash", &self.request_hash)
            .field("action_key", &self.action_key)
            .field(
                "model_input_schema_hash",
                &self.model_input_contract.content_hash,
            )
            .field("input_schema_hash", &self.contracts.input.content_hash)
            .field(
                "output_schema_hash",
                &self.contracts.output_contract_set_hash,
            )
            .field("state_id", &self.state_id)
            .field("proposed_arguments", &"[REDACTED]")
            .field("arguments", &"[REDACTED]")
            .field(
                "research_intent_receipt",
                &self
                    .research_intent_receipt
                    .as_ref()
                    .map(|receipt| receipt.compiled_plan_hash.clone()),
            )
            .field("canonical_arguments_len", &self.canonical_arguments.len())
            .finish_non_exhaustive()
    }
}

impl Drop for PreparedCall {
    fn drop(&mut self) {
        scrub_json(&mut self.proposed_arguments);
        scrub_json(&mut self.arguments);
        self.canonical_arguments.zeroize();
    }
}

/// Provider-visible capability results preserve the exact typed server result
/// while adding kernel-owned goal aliases after a research proposal has been
/// lowered. The model never constructs these aliases; it may only cite them
/// in a later `AnswerIR`. This closes the identity gap created when opaque
/// graph and clause IDs were correctly removed from the proposal ABI.
pub(crate) fn model_visible_capability_result(
    call: &PreparedCall,
    result: &CapabilityResult,
) -> Value {
    let Some(receipt) = &call.research_intent_receipt else {
        return result.provider_content.clone();
    };
    if is_input_correction(result) {
        return result.provider_content.clone();
    }
    let bindings = receipt
        .clause_goal_ids
        .iter()
        .flat_map(|(clause_id, goal_ids)| {
            goal_ids.iter().map(move |goal_id| {
                serde_json::json!({
                    "goal_id": goal_id,
                    "clause_id": clause_id,
                })
            })
        })
        .collect::<Vec<_>>();
    let mut visible = serde_json::json!({
        "result": result.provider_content,
        "kernel_research_goals": {
            "schema_version": 1,
            "bindings": bindings,
        }
    });
    // Preserve the exact typed server result above, but add a small
    // kernel-owned orientation note after a context response.  A full
    // ResearchState can contain many thousands of tokens of observations;
    // this makes the server-reported *required* gaps visible at the decision
    // point without changing evidence or forcing a tool choice.
    if call.capability.id == "ontology.query_context" {
        if let Some(hint) = model_research_gap_hint(&result.provider_content) {
            visible
                .as_object_mut()
                .expect("JSON object literal")
                .insert("kernel_research_gap_hint".into(), hint);
        }
    }
    visible
}

/// Project only a bounded list of canonical retrieval gaps from a typed
/// `ResearchState`. The raw capability result remains verbatim under
/// `result`; this is a kernel note telling the model which known gaps deserve
/// a decision before it declares evidence sufficient. Each candidate retains
/// the exact canonical `retrieval_query`, so the research planner can map a
/// model-selected targeted query back to the corresponding goal. Query strings
/// are intentionally labeled untrusted because they ultimately originate from
/// the user question and remote ontology result.
pub(crate) fn model_research_gap_hint(result: &Value) -> Option<Value> {
    // A proposal is bounded to twelve clauses.  Hiding its last required
    // gaps from the analyst made a user-named item depend on arbitrary plan
    // ordering (for example a cash-flow clause after several geography
    // clauses), even though the exact query was already available.  Carry
    // every bounded required candidate; this is context only, not another
    // model turn or a completion gate.
    const MAX_TOPICS: usize = 12;
    const MAX_TOPIC_CHARS: usize = 512;
    const MAX_FILTER_ITEMS: usize = 16;
    const MAX_FILTER_CHARS: usize = 128;

    let root = result.as_object()?;
    let plan = root.get("plan")?.as_object()?;
    let missing_clause_ids = root
        .get("missing_parts")?
        .as_array()?
        .iter()
        .filter_map(|part| part.get("clause_id")?.as_str())
        .collect::<BTreeSet<_>>();
    if missing_clause_ids.is_empty() {
        return None;
    }

    let fallback_ticker = plan
        .get("tickers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .find(|ticker| ticker.len() <= 32 && is_canonical_ticker(ticker))
        .map(ToOwned::to_owned);
    let document_types = bounded_gap_filter_values(
        plan.get("document_types"),
        MAX_FILTER_ITEMS,
        MAX_FILTER_CHARS,
    );
    let periods =
        bounded_gap_filter_values(plan.get("periods"), MAX_FILTER_ITEMS, MAX_FILTER_CHARS);
    let mut topics = Vec::new();
    let mut exact_candidate_queries = Vec::new();
    for clause in plan
        .get("clauses")?
        .as_array()?
        .iter()
        .filter(|clause| {
            clause
                .get("required")
                .and_then(Value::as_bool)
                .unwrap_or(false)
                && clause
                    .get("clause_id")
                    .and_then(Value::as_str)
                    .is_some_and(|id| missing_clause_ids.contains(id))
        })
        .take(MAX_TOPICS)
    {
        let Some(topic) = clause.get("retrieval_query").and_then(Value::as_str) else {
            continue;
        };
        // Do not truncate or concatenate a canonical query. `ResearchPlanner`
        // intentionally compares it exactly when deciding whether one precise
        // read advances a missing goal.
        if topic.is_empty() || topic.chars().count() > MAX_TOPIC_CHARS {
            continue;
        }
        let ticker = clause
            .get("tickers")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .find(|ticker| ticker.len() <= 32 && is_canonical_ticker(ticker))
            .map(ToOwned::to_owned)
            .or_else(|| fallback_ticker.clone());
        let Some(ticker) = ticker else {
            continue;
        };
        let object_types = bounded_gap_filter_values(
            clause.get("object_types"),
            MAX_FILTER_ITEMS,
            MAX_FILTER_CHARS,
        );
        let mut candidate = serde_json::Map::new();
        candidate.insert("ticker".into(), Value::String(ticker));
        candidate.insert("topic".into(), Value::String(topic.to_owned()));
        // Compact is the safe default carried into the candidate. The analyst
        // may choose full in this same bounded call when the question needs an
        // exact quote, numeric basis, period/scope detail, or lineage.
        candidate.insert("response_detail".into(), Value::String("compact".into()));
        // This is a kernel-owned exact-gap read, not a model-selected broad
        // discovery query. The runtime uses the existing public field to keep
        // the complete phrase strict rather than filling a page with partial
        // matches that would make an available disclosure look absent.
        candidate.insert("answer_candidate_only".into(), Value::Bool(true));
        candidate.insert("limit".into(), Value::from(20_u64));
        if !document_types.is_empty() {
            candidate.insert(
                "document_types".into(),
                Value::Array(document_types.iter().cloned().map(Value::String).collect()),
            );
        }
        if !periods.is_empty() {
            candidate.insert(
                "periods".into(),
                Value::Array(periods.iter().cloned().map(Value::String).collect()),
            );
        }
        if !object_types.is_empty() {
            candidate.insert(
                "object_types".into(),
                Value::Array(object_types.into_iter().map(Value::String).collect()),
            );
        }
        topics.push(topic.to_owned());
        exact_candidate_queries.push(Value::Object(candidate));
    }
    if exact_candidate_queries.is_empty() {
        return None;
    }
    let has_more = root
        .get("continuation")
        .and_then(Value::as_object)
        .and_then(|continuation| continuation.get("has_more"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let omitted_evidence_count = root
        .get("continuation")
        .and_then(Value::as_object)
        .and_then(|continuation| continuation.get("omitted_evidence_count"))
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(1_000_000);
    let retrieval_incomplete = has_more || omitted_evidence_count > 0;
    let kernel_guidance = if retrieval_incomplete {
        "The current evidence page is incomplete: matching evidence was omitted. This cannot establish that a company did not disclose a named fact. Before selecting evidence_sufficient, select one advertised exact candidate when it can resolve a user-named material gap. Select it exactly without joining or rewriting its topic. After the exact result, continue with the best supported answer even if the fact remains unavailable."
    } else {
        "The exact result above reports required gaps. Before selecting evidence_sufficient, prefer at most one advertised precise query when it can resolve a named gap. Select one exact candidate below without joining or rewriting its topic. If no candidate can materially help, choose the ordinary bounded stop path and still produce the best supported answer."
    };

    Some(serde_json::json!({
        "schema_version": 1,
        "kind": "required_retrieval_gap_hint",
        "kernel_guidance": kernel_guidance,
        "retrieval_status": {
            "has_more": has_more,
            "omitted_evidence_count": omitted_evidence_count,
            "incomplete": retrieval_incomplete,
        },
        "missing_required_clause_count": missing_clause_ids.len(),
        "untrusted_exact_candidate_topics": topics,
        "exact_precise_query_candidates": exact_candidate_queries,
    }))
}

/// The hint is derived from a remote ResearchState. Keep optional filters
/// bounded and string-only before copying them into a model-visible candidate.
/// Bad optional filters are omitted (broadened), never turned into a new
/// rejection path for the exact read.
fn bounded_gap_filter_values(
    value: Option<&Value>,
    max_items: usize,
    max_chars: usize,
) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|value| !value.is_empty() && value.chars().count() <= max_chars)
        .map(ToOwned::to_owned)
        .take(max_items)
        .collect()
}

pub(crate) fn capability_invocation(run_id: &str, call: &PreparedCall) -> CapabilityInvocation {
    CapabilityInvocation {
        run_id: run_id.to_owned(),
        action_key: call.action_key.clone(),
        capability_id: call.capability.id.clone(),
        request_hash: call.request_hash.clone(),
        input_schema_hash: call.contracts.input.content_hash.clone(),
        output_schema_hash: call.contracts.output_contract_set_hash.clone(),
        normalized_output_contract_hash: call.normalized_output_contract_hash.clone(),
        arguments: call.arguments.clone(),
        binding: call.binding.clone(),
    }
}

/// Thin wrapper preserving the engine-facing signature: the canonical,
/// JSON-order-independent action key now lives in
/// `krw-agent-execution-contracts`; a fingerprint failure (practically
/// unreachable) is surfaced as a non-retryable engine dependency error.
pub fn deterministic_action_key(
    run_id: &str,
    agent_image_hash: &ContentHash,
    capability: &CapabilitySpec,
    contracts: &ResolvedCapabilityContracts,
    binding: &CapabilityBinding,
    arguments: &Value,
) -> Result<String, EngineError> {
    krw_agent_execution_contracts::deterministic_action_key(
        run_id,
        agent_image_hash,
        capability,
        contracts,
        binding,
        arguments,
    )
    .map_err(|failure| EngineError::Dependency {
        component: "capability.action_key",
        failure,
    })
}

fn accepted_action_result<'a>(
    state: &'a ActiveRun,
    capability_id: &str,
) -> Result<(&'a AcceptedActionRef, &'a CapabilityResult), EngineError> {
    for action in state
        .accepted_actions
        .iter()
        .rev()
        .filter(|action| action.capability_id == capability_id)
    {
        let result = state
            .action_cache
            .get(&action.action_key)
            .ok_or(EngineError::Invariant(
                "accepted action reference lacks its committed result",
            ))?;
        if capability_result_completes_prerequisite(result) {
            return Ok((action, result));
        }
    }
    Err(EngineError::CapabilityInputDerivation(
        capability_id.to_owned(),
    ))
}

fn committed_retained_capability_input<'a>(
    state: &'a ActiveRun,
    source_capability: &str,
    owner_capability: &str,
) -> Result<(Value, &'a CapabilityResult), EngineError> {
    let (action, result) = accepted_action_result(state, source_capability)?;
    let bytes = action.sealed_input.as_deref().ok_or_else(|| {
        EngineError::CapabilityInputDerivation(format!(
            "{owner_capability} requires retained input from {source_capability}"
        ))
    })?;
    if ContentHash::sha256(bytes) != action.input_hash {
        return Err(EngineError::Invariant(
            "retained capability input differs from its accepted action hash",
        ));
    }
    let input = serde_json::from_slice(bytes)?;
    Ok((input, result))
}

fn assemble_guru_company_brief(
    state: &ActiveRun,
    draft: &Value,
    query_context_capability: &str,
    owner_capability: &str,
) -> Result<Value, EngineError> {
    let (query_input, query_result) =
        committed_retained_capability_input(state, query_context_capability, owner_capability)?;
    let query_input =
        enrich_guru_query_input_with_result_context(&query_input, &query_result.provider_content)
            .map_err(|error| {
            tracing::warn!(
                capability = owner_capability,
                stage = "enrich_query_context",
                error_code = guru_contract_error_code(&error),
                "Guru company-brief input derivation rejected committed query context"
            );
            EngineError::CapabilityInputDerivation(owner_capability.into())
        })?;
    let research_pack = query_result
        .provider_content
        .get("research_pack")
        .ok_or_else(|| {
            tracing::warn!(
                capability = owner_capability,
                stage = "research_pack",
                "Guru company-brief input derivation found no committed research pack"
            );
            EngineError::CapabilityInputDerivation(owner_capability.into())
        })?;
    match build_company_brief_input(&query_input, research_pack, draft) {
        Ok(arguments) => attach_guru_query_result_context(
            arguments,
            &query_result.provider_content,
            owner_capability,
        ),
        Err(error)
            if matches!(
                &error,
                krw_agent_contracts::ContractValueError::Semantic(contract)
                    if *contract == KRW_GURU_INVESTIGATION_QUESTION_DRAFT_V1
            ) =>
        {
            let context = query_input
                .get("company_context")
                .ok_or_else(|| EngineError::CapabilityInputDerivation(owner_capability.into()))?;
            let Some(repaired) = repair_guru_draft_linkage(draft, research_pack, context) else {
                tracing::warn!(
                    capability = owner_capability,
                    stage = "build_company_brief_input",
                    error_code = guru_contract_error_code(&error),
                    "Guru company-brief draft could not be linked to committed IDs"
                );
                return Err(EngineError::ModelProposalRejected(
                    ModelProposalRejection::generic("guru_company_brief_draft_unlinked"),
                ));
            };
            tracing::warn!(
                capability = owner_capability,
                stage = "repair_guru_draft_linkage",
                "Guru draft IDs were narrowed to committed principle and context IDs"
            );
            build_company_brief_input(&query_input, research_pack, &repaired)
                .map_err(|repair_error| {
                    tracing::warn!(
                        capability = owner_capability,
                        stage = "build_company_brief_input_after_repair",
                        error_code = guru_contract_error_code(&repair_error),
                        "Guru company-brief draft remained invalid after deterministic ID repair"
                    );
                    EngineError::CapabilityInputDerivation(owner_capability.into())
                })
                .and_then(|arguments| {
                    attach_guru_query_result_context(
                        arguments,
                        &query_result.provider_content,
                        owner_capability,
                    )
                })
        }
        Err(error) => {
            tracing::warn!(
                capability = owner_capability,
                stage = "build_company_brief_input",
                error_code = guru_contract_error_code(&error),
                "Guru company-brief input derivation rejected trusted or malformed input"
            );
            Err(EngineError::CapabilityInputDerivation(
                owner_capability.into(),
            ))
        }
    }
}

/// The canonical brief input accepts either the compact ResearchPack or the
/// complete query-context result.  The Guru MCP's company-brief adapter needs
/// the complete envelope to carry `research_status` and
/// `selected_author_keys` into its response, so preserve the committed result
/// while keeping the model-authored draft and all other fields unchanged.
fn attach_guru_query_result_context(
    arguments: Value,
    query_result: &Value,
    capability: &str,
) -> Result<Value, EngineError> {
    let mut object = arguments
        .as_object()
        .cloned()
        .ok_or_else(|| EngineError::CapabilityInputDerivation(capability.to_owned()))?;
    object.insert("guru_query_context".into(), query_result.clone());
    let enriched = Value::Object(object);
    validate_canonical_value("krw-guru-company-brief-input/v1", &enriched).map_err(|error| {
        tracing::warn!(
            capability,
            stage = "attach_query_result_context",
            error = ?error,
            "Guru company-brief input rejected the committed query-context envelope"
        );
        EngineError::CapabilityInputDerivation(capability.to_owned())
    })?;
    Ok(enriched)
}

/// Keep model-authored reasoning intact while preventing a provider from
/// inventing the opaque IDs that link it to the committed Guru pack/context.
/// Only exact IDs already present in trusted artifacts survive; if the model
/// supplied no valid ID, the first committed ID is a deterministic fallback.
fn repair_guru_draft_linkage(draft: &Value, pack: &Value, context: &Value) -> Option<Value> {
    let mut object = draft.as_object()?.clone();
    let selected_principles = pack
        .get("selected_lenses")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|lens| lens.get("reviewed_id").and_then(Value::as_str))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let trusted_anchors = context
        .get("context_anchors")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|anchor| anchor.get("anchor_id").and_then(Value::as_str))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if selected_principles.is_empty() || trusted_anchors.is_empty() {
        return None;
    }
    let mut changed = false;
    for (field, allowed) in [
        ("guru_principle_ids", selected_principles.as_slice()),
        ("company_context_anchor_ids", trusted_anchors.as_slice()),
    ] {
        let supplied = object
            .get(field)
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .filter(|value| allowed.iter().any(|candidate| candidate == value))
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let repaired = if supplied.is_empty() {
            vec![allowed[0].clone()]
        } else {
            supplied
        };
        let current = object.get(field).and_then(Value::as_array);
        let differs = current.is_none_or(|values| {
            values
                .iter()
                .map(Value::as_str)
                .collect::<Option<Vec<_>>>()
                .is_none_or(|values| {
                    values != repaired.iter().map(String::as_str).collect::<Vec<_>>()
                })
        });
        if differs {
            object.insert(
                field.to_string(),
                Value::Array(repaired.into_iter().map(Value::String).collect()),
            );
            changed = true;
        }
    }
    if let Some(values) = object
        .get_mut("evidence_needed")
        .and_then(Value::as_array_mut)
        && values.len() > 3
    {
        values.truncate(3);
        changed = true;
    }
    changed.then_some(Value::Object(object))
}

fn guru_contract_error_code(error: &krw_agent_contracts::ContractValueError) -> &'static str {
    match error {
        krw_agent_contracts::ContractValueError::UnknownContract(_) => "unknown_contract",
        krw_agent_contracts::ContractValueError::Shape(_) => "shape_invalid",
        krw_agent_contracts::ContractValueError::Semantic(_) => "semantic_invalid",
        krw_agent_contracts::ContractValueError::Limit(_) => "limit_exceeded",
        krw_agent_contracts::ContractValueError::Json(_) => "serialization_invalid",
    }
}

/// Lower the empty provider-visible Guru trigger to the physical retrieval
/// request. The model cannot select a Guru, ticker, company context, or
/// retrieval knobs: the selected entrypoint and authenticated run scope own
/// all of them.
fn assemble_guru_query_context(
    proposed: &Value,
    request: &RunRequest,
    entrypoint: &EntrypointSpec,
) -> Result<Value, EngineError> {
    validate_canonical_value(GURU_QUERY_REQUEST_V1, proposed).map_err(|_| {
        EngineError::ModelProposalRejected(ModelProposalRejection::generic(
            "guru_query_request_invalid",
        ))
    })?;
    let author =
        entrypoint
            .constants
            .fixed_guru_author
            .ok_or(EngineError::CapabilityInputDerivation(
                "guru.query_context fixed author".into(),
            ))?;
    let ticker = match request.context.trusted_tickers() {
        [ticker] if is_canonical_ticker(ticker) => ticker,
        _ => {
            return Err(EngineError::RunScopeViolation(
                "Guru query context requires exactly one canonical trusted ticker",
            ));
        }
    };
    Ok(serde_json::json!({
        "question": request.question,
        "author_keys": [author.as_str()],
        "ticker": ticker,
    }))
}

/// Lower the small provider-authored orientation request to the physical MCP
/// schema. A company ontology map is deliberately broad: it introduces the
/// planner to the company's current vocabulary and document landscape, not a
/// model-guessed historical slice. Exact period or filing requests belong to
/// the later ResearchProposal/SearchPlan path, where they are interpreted
/// against the user's question. Internal router controls are kernel-owned: they
/// are never part of the model contract and the raw response is sanitized by
/// capability-runtime before it is retained in the transcript.
pub(crate) fn assemble_company_context_request(proposed: &Value) -> Result<Value, EngineError> {
    let request = proposed.as_object().ok_or_else(|| {
        EngineError::ModelProposalRejected(ModelProposalRejection::generic(
            "company_context_request_invalid",
        ))
    })?;
    let ticker = request
        .get("ticker")
        .and_then(Value::as_str)
        .filter(|ticker| !ticker.is_empty() && ticker.len() <= 32)
        .ok_or_else(|| {
            EngineError::ModelProposalRejected(ModelProposalRejection::generic(
                "company_context_request_invalid",
            ))
        })?;
    let mut physical = serde_json::Map::new();
    physical.insert("ticker".into(), Value::String(ticker.to_owned()));
    // The orientation turn used to accept model-authored period/document
    // filters. A planner could therefore start from an arbitrary (and often
    // non-canonical) historical slice before it had seen the company map. Pin
    // the bounded map size and leave scope open here; the executable research
    // plan remains the single authority for exact scope later in the flow.
    physical.insert("limit_topics".into(), Value::from(8));
    // The source service defaults this field to true. Always pin it false so
    // routing/internal IDs cannot enter raw capability retention or logs.
    physical.insert("include_internal_ids".into(), Value::Bool(false));
    Ok(Value::Object(physical))
}

fn guru_evidence_review_sources(
    state: &ActiveRun,
    query_context_capability: &str,
    company_brief_capability: &str,
    evidence_capabilities: &[String],
    owner_capability: &str,
) -> Result<(Value, Value, Value), EngineError> {
    let (query_input, query_result) =
        committed_retained_capability_input(state, query_context_capability, owner_capability)?;
    let query_input =
        enrich_guru_query_input_with_result_context(&query_input, &query_result.provider_content)
            .map_err(|_| EngineError::CapabilityInputDerivation(owner_capability.into()))?;
    let (brief_action, brief_result) = accepted_action_result(state, company_brief_capability)?;
    let brief = brief_result
        .provider_content
        .get("investigation_brief")
        .filter(|value| !value.is_null())
        .ok_or_else(|| EngineError::CapabilityInputDerivation(owner_capability.into()))?;
    let brief_position = state
        .accepted_actions
        .iter()
        .position(|action| action.action_key == brief_action.action_key)
        .ok_or(EngineError::Invariant(
            "accepted Guru brief disappeared from action order",
        ))?;
    let mut evidence_results = Vec::new();
    for action in state
        .accepted_actions
        .iter()
        .skip(brief_position.saturating_add(1))
        .filter(|action| {
            evidence_capabilities
                .iter()
                .any(|id| id == &action.capability_id)
        })
    {
        let result = state
            .action_cache
            .get(&action.action_key)
            .ok_or(EngineError::Invariant(
                "accepted evidence action lacks its committed result",
            ))?;
        if capability_result_completes_prerequisite(result) {
            evidence_results.push(result.provider_content.clone());
        }
    }
    if evidence_results.is_empty() {
        return Err(EngineError::CapabilityInputDerivation(
            owner_capability.into(),
        ));
    }
    let research_context = build_company_research_context(brief, &evidence_results)
        .map_err(|_| EngineError::CapabilityInputDerivation(owner_capability.into()))?;
    Ok((query_input, brief.clone(), research_context))
}

fn assemble_guru_evidence_review(
    state: &ActiveRun,
    analysis: &Value,
    query_context_capability: &str,
    company_brief_capability: &str,
    evidence_capabilities: &[String],
    owner_capability: &str,
) -> Result<Value, EngineError> {
    let (query_input, brief, research_context) = guru_evidence_review_sources(
        state,
        query_context_capability,
        company_brief_capability,
        evidence_capabilities,
        owner_capability,
    )?;
    let analysis = normalize_guru_agent_evidence_analysis(&brief, &research_context, analysis)
        .map_err(|_| EngineError::CapabilityInputDerivation(owner_capability.into()))?;
    build_evidence_review_input(&query_input, &brief, &research_context, &analysis)
        .map_err(|_| EngineError::CapabilityInputDerivation(owner_capability.into()))
}

fn resolve_research_proposal_question(
    capability: &CapabilitySpec,
    state: &ActiveRun,
    request: &RunRequest,
) -> Result<String, EngineError> {
    match capability.research_proposal_anchor.as_ref() {
        Some(ResearchProposalAnchor::RunQuestion) => Ok(request.question.clone()),
        Some(ResearchProposalAnchor::SealedGuruInvestigationBriefV1 { source_capability }) => {
            let (_, result) = accepted_action_result(state, source_capability)?;
            validate_canonical_value(KRW_GURU_COMPANY_BRIEF_RESULT_V1, &result.provider_content)
                .map_err(|_| {
                    EngineError::CapabilityInputDerivation(format!(
                        "{} sealed source contract",
                        capability.id
                    ))
                })?;
            let brief_result: GuruCompanyBriefResult =
                serde_json::from_value(result.provider_content.clone()).map_err(|_| {
                    EngineError::CapabilityInputDerivation(format!(
                        "{} sealed source decode",
                        capability.id
                    ))
                })?;
            let brief = brief_result.investigation_brief.0.as_ref().ok_or_else(|| {
                EngineError::CapabilityInputDerivation(format!(
                    "{} sealed investigation brief missing",
                    capability.id
                ))
            })?;
            let brief_value = serde_json::to_value(brief).map_err(|_| {
                EngineError::CapabilityInputDerivation(format!(
                    "{} sealed investigation frame serialization",
                    capability.id
                ))
            })?;
            let frame = compile_guru_research_frame(&brief_value).map_err(|_| {
                EngineError::CapabilityInputDerivation(format!(
                    "{} sealed investigation frame",
                    capability.id
                ))
            })?;
            let question = frame.question.trim();
            if question.is_empty() {
                return Err(EngineError::CapabilityInputDerivation(format!(
                    "{} sealed investigation question empty",
                    capability.id
                )));
            }
            Ok(question.to_owned())
        }
        None => Err(EngineError::CapabilityInputDerivation(format!(
            "{} missing research proposal anchor",
            capability.id
        ))),
    }
}

fn assemble_capability_arguments(
    capability: &CapabilitySpec,
    state: &ActiveRun,
    proposed: &Value,
    request: &RunRequest,
    entrypoint: &EntrypointSpec,
) -> Result<AssembledCapabilityArguments, EngineError> {
    match &capability.input_derivation {
        InputDerivation::Identity => Ok(AssembledCapabilityArguments {
            arguments: proposed.clone(),
            research_intent_receipt: None,
        }),
        InputDerivation::CompanyContextRequestV1 => {
            assemble_company_context_request(proposed).map(|arguments| {
                AssembledCapabilityArguments {
                    arguments,
                    research_intent_receipt: None,
                }
            })
        }
        InputDerivation::SealedGuruQueryContextV1 => {
            assemble_guru_query_context(proposed, request, entrypoint).map(|arguments| {
                AssembledCapabilityArguments {
                    arguments,
                    research_intent_receipt: None,
                }
            })
        }
        InputDerivation::ResearchProposalToSearchPlanV4 => {
            let question = resolve_research_proposal_question(capability, state, request)?;
            compile_research_proposal(
                proposed,
                InitialPlanScope {
                    question: &question,
                    context: &request.context,
                    derived_tickers: state
                        .derived_ticker_scope
                        .as_ref()
                        .map(|scope| scope.tickers.as_slice()),
                    max_discovery_tickers: entrypoint.scope.cardinality.value(),
                    prior_plan: state.research_planner.confirmed_context_plan(),
                    requester: ResearchPlanRequester::from_capability_id(&capability.id),
                },
            )
            .map(|compiled| AssembledCapabilityArguments {
                arguments: compiled.search_plan,
                research_intent_receipt: Some(compiled.receipt),
            })
            // The compiler rejects a model-owned ResearchProposal before any
            // capability action exists. Model-controlled failures cross the
            // bounded declared repair edge with a closed, non-content code;
            // trusted scope/receipt failures remain terminal invariants.
            .map_err(|e| research_proposal_compilation_error(&e))
        }
        InputDerivation::SealedGuruCompanyBriefV1 {
            query_context_capability,
        } => assemble_guru_company_brief(state, proposed, query_context_capability, &capability.id)
            .map(|arguments| AssembledCapabilityArguments {
                arguments,
                research_intent_receipt: None,
            }),
        InputDerivation::SealedGuruEvidenceReviewV1 {
            query_context_capability,
            company_brief_capability,
            evidence_capabilities,
        } => assemble_guru_evidence_review(
            state,
            proposed,
            query_context_capability,
            company_brief_capability,
            evidence_capabilities,
            &capability.id,
        )
        .map(|arguments| AssembledCapabilityArguments {
            arguments,
            research_intent_receipt: None,
        }),
    }
}

struct AssembledCapabilityArguments {
    arguments: Value,
    research_intent_receipt: Option<ResearchIntentReceipt>,
}

/// Maps the deterministic `ResearchProposal` compiler boundary to the same
/// closed repair channel used for provider-schema failures. The proposal has
/// no model-authored IDs or graph edges, so only proposal shape and bounded
/// plan-size failures are repairable; all remaining errors are trusted
/// compiler or scope invariants.
fn research_proposal_compilation_error(error: &InitialPlanError) -> EngineError {
    let model_code = match error {
        InitialPlanError::Contract => Some("model_research_proposal_contract_invalid"),
        InitialPlanError::Decode => Some("model_research_proposal_decode_invalid"),
        InitialPlanError::PlanTooLarge => Some("model_research_proposal_plan_too_large"),
        InitialPlanError::AppendClauseLimit => Some("proposal_append_plan_capacity_reached"),
        // These compiler-internal errors were previously terminal Invariants,
        // but they often stem from the model producing a proposal that is
        // schema-valid yet semantically inconsistent (e.g. duplicate search
        // clauses, goals the planner cannot cover). Routing them through the
        // repair channel gives the model a bounded retry instead of an
        // immediate run failure.
        InitialPlanError::DuplicateClause => Some("proposal_duplicate_search_clause"),
        InitialPlanError::UnlinkedUserGoal => Some("proposal_unlinked_user_goal"),
        InitialPlanError::UnlinkedDependency => Some("proposal_unlinked_dependency"),
        InitialPlanError::GoalGraph => Some("proposal_goal_graph_invalid"),
        InitialPlanError::CandidateCoverage => Some("proposal_candidate_coverage_gap"),
        InitialPlanError::UncoverableGoal => Some("proposal_uncoverable_goal"),
        InitialPlanError::SearchPlanContract => Some("proposal_search_plan_contract_invalid"),
        InitialPlanError::UnsupportedScope => Some("proposal_unsupported_scope"),
        InitialPlanError::PriorPlan => Some("proposal_prior_plan_conflict"),
        InitialPlanError::Receipt => Some("proposal_receipt_invalid"),
        InitialPlanError::Canonicalization => Some("proposal_canonicalization_failed"),
    };
    model_code.map_or_else(
        || EngineError::Invariant("trusted research-proposal compilation boundary failed"),
        |reason_code| {
            EngineError::ModelProposalRejected(ModelProposalRejection::generic(reason_code))
        },
    )
}

pub(crate) fn is_append_context_plan_capacity_rejection(error: &EngineError) -> bool {
    matches!(
        error,
        EngineError::ModelProposalRejected(rejection)
            if rejection.code() == "proposal_append_plan_capacity_reached"
    )
}

pub(crate) fn prepare_calls(
    episode: &ProviderEpisodeV1,
    input: &RunInput<'_>,
    state: &ActiveRun,
    max_calls: usize,
    contract_guard: &dyn ContractGuard,
) -> Result<Vec<PreparedCall>, EngineError> {
    let current_context = state
        .context_planner
        .for_request(input.request, state.interpreter.current_state())?;
    let state_capability_frontier = current_context
        .capability_schemas
        .iter()
        .map(|schema| schema.capability_id.as_str())
        .collect::<BTreeSet<_>>();
    let calls = &episode.assistant.tool_calls;
    if calls.len() > max_calls {
        return Err(EngineError::TooManyToolCalls {
            observed: calls.len(),
            limit: max_calls,
        });
    }
    if episode.finish_reason != "tool_calls" {
        return Err(EngineError::InvalidProviderEpisode(
            "tool calls require tool_calls finish_reason",
        ));
    }
    let mut call_ids = BTreeSet::new();
    let mut prepared = Vec::with_capacity(calls.len());
    for call in calls {
        if call.id.is_empty() || !call_ids.insert(call.id.as_str()) {
            return Err(EngineError::InvalidToolCallId);
        }
        if call.kind != ToolCallKind::Function {
            return Err(EngineError::InvalidProviderEpisode(
                "unsupported tool call type",
            ));
        }
        let capability = input
            .image
            .body
            .capabilities
            .iter()
            .find(|capability| provider_tool_name(&capability.id) == call.function.name.as_str())
            .ok_or_else(|| EngineError::UnknownCapability(call.function.name.as_str().into()))?;
        if capability.permission != Permission::Read
            || capability.idempotency != IdempotencyPolicy::CanonicalArgs
        {
            return Err(EngineError::UnsafeCapability(capability.id.clone()));
        }
        if !capability
            .prerequisites
            .iter()
            .all(|required| state.completed_capabilities.contains(required))
        {
            return Err(EngineError::CapabilityPrerequisiteMissing(
                capability.id.clone(),
            ));
        }
        if !state
            .tool_definitions
            .iter()
            .any(|definition| definition.name() == call.function.name.as_str())
        {
            return Err(EngineError::InvalidProviderEpisode(
                "provider invoked a capability absent from the advertised dynamic frontier",
            ));
        }
        if !state_capability_frontier.contains(capability.id.as_str()) {
            return Err(EngineError::WorkflowResolution {
                outcome: "state-scoped capability frontier",
            });
        }
        let binding_key = capability
            .remote_binding_key()
            .ok_or(EngineError::Invariant(
                "local capability reached external dispatch preparation",
            ))?;
        let binding = input
            .deployment
            .capabilities
            .iter()
            .find(|binding| binding.binding_key == binding_key)
            .ok_or_else(|| EngineError::MissingCapabilityBinding(capability.id.clone()))?;
        let pinned_release = input
            .snapshot
            .capability_release_hashes
            .get(&capability.id)
            .ok_or_else(|| EngineError::MissingPinnedRelease(capability.id.clone()))?;
        if pinned_release != &binding.data_release_hash {
            return Err(EngineError::CapabilityReleaseMismatch(
                capability.id.clone(),
            ));
        }
        let raw_arguments: Value =
            serde_json::from_str(&call.function.arguments).map_err(|_| {
                EngineError::ModelProposalRejected(ModelProposalRejection::generic(
                    "tool_arguments_json_invalid",
                ))
            })?;
        let mut proposed_arguments = capability
            .provider_input_codec
            .decode(raw_arguments)
            .map_err(|_| {
                EngineError::ModelProposalRejected(ModelProposalRejection::generic(
                    "provider_input_envelope_invalid",
                ))
            })?;
        if !proposed_arguments.is_object() {
            return Err(EngineError::ToolArgumentsMustBeObject(
                capability.id.clone(),
            ));
        }
        let entrypoint = selected_entrypoint(input.image, input.request)?;
        validate_fixed_guru_author_payload(entrypoint, &proposed_arguments)?;
        let model_input = input
            .image
            .resolve_capability_model_input_contract(capability)?;
        normalize_provider_model_input(&model_input.id, &mut proposed_arguments);
        // Question/object identifiers in a Guru evidence analysis are opaque
        // links to kernel-owned artifacts.  Normalize them against the
        // committed brief/context before the model-input guard runs so a
        // harmless ID or verdict mismatch does not open a recovery loop inside
        // the bounded child.  The reasoning prose remains model-authored.
        if capability.id == "guru.review_company_evidence"
            && let InputDerivation::SealedGuruEvidenceReviewV1 {
                query_context_capability,
                company_brief_capability,
                evidence_capabilities,
            } = &capability.input_derivation
        {
            let (_, brief, research_context) = guru_evidence_review_sources(
                state,
                query_context_capability,
                company_brief_capability,
                evidence_capabilities,
                &capability.id,
            )?;
            let before = proposed_arguments.clone();
            proposed_arguments = normalize_guru_agent_evidence_analysis(
                &brief,
                &research_context,
                &proposed_arguments,
            )
            .map_err(|_| EngineError::CapabilityInputDerivation(capability.id.clone()))?;
            if proposed_arguments != before {
                tracing::warn!(
                    capability = %capability.id,
                    "normalized Guru evidence analysis linkage before review"
                );
            }
        }
        let frontier_schema = current_context
            .capability_schemas
            .iter()
            .find(|schema| schema.capability_id == capability.id)
            .ok_or(EngineError::WorkflowResolution {
                outcome: "capability model input schema",
            })?;
        if frontier_schema.input_contract_id != model_input.id
            || frontier_schema.input_schema_hash != model_input.content_hash
        {
            return Err(EngineError::WorkflowResolution {
                outcome: "capability model input schema pin",
            });
        }
        if let Some(rejection) = model_input_repair_directive(&model_input.id, &proposed_arguments)
        {
            return Err(EngineError::ModelProposalRejected(rejection));
        }
        let mut model_capability = capability.clone();
        model_capability.input_contract.clone_from(&model_input.id);
        model_capability.model_input_contract = None;
        model_capability.provider_input_codec = krw_agent_image::ProviderInputCodec::default();
        model_capability.input_derivation = InputDerivation::Identity;
        // GLM authors the investigation-question draft with the linkage ID
        // arrays empty. The assemble-side deterministic repair exists for
        // exactly that shape, but the strict model-input guard below rejects
        // the draft BEFORE the repair can run, and the generic rejection code
        // leaves the model nothing actionable — the next episode answers in
        // prose and the run dies terminally (production 2026-08-19: 4 of 5
        // guru lenses failed this way). Pre-link the draft the same way the
        // assemble path would, so empty linkage never reaches the guard.
        if capability.id == "guru.company_brief"
            && let InputDerivation::SealedGuruCompanyBriefV1 {
                query_context_capability,
            } = &capability.input_derivation
        {
            if let Ok((query_input, query_result)) =
                committed_retained_capability_input(state, query_context_capability, &capability.id)
                && let Ok(enriched) = enrich_guru_query_input_with_result_context(
                    &query_input,
                    &query_result.provider_content,
                )
                && let (Some(research_pack), Some(context)) = (
                    query_result.provider_content.get("research_pack"),
                    enriched.get("company_context"),
                )
                && let Some(repaired) =
                    repair_guru_draft_linkage(&proposed_arguments, research_pack, context)
                && repaired != proposed_arguments
            {
                tracing::warn!(
                    capability = %capability.id,
                    "pre-linked empty Guru draft IDs before the model-input guard"
                );
                proposed_arguments = repaired;
            }
        }
        if let Err(failure) =
            contract_guard.validate_arguments(&model_capability, binding, &proposed_arguments)
        {
            // Keep the diagnostic bounded and structural: model/user/evidence
            // values never enter ordinary logs, while field names make a
            // provider-shape mismatch repairable without exposing content.
            let keys = proposed_arguments
                .as_object()
                .map(|object| object.keys().take(32).cloned().collect::<Vec<_>>())
                .unwrap_or_default();
            tracing::warn!(
                capability = %capability.id,
                model_input_contract = %model_input.id,
                failure_code = %failure.code,
                key_count = keys.len(),
                keys = ?keys,
                "model capability input rejected"
            );
            let rejection = if capability.id == "guru.company_brief"
                && proposed_arguments
                    .as_object()
                    .is_some_and(|object| object.is_empty())
            {
                Some(ModelProposalRejection::generic(
                    "guru_company_brief_input_empty",
                ))
            } else {
                model_input_rejection_code(&failure)
            };
            return Err(rejection.map_or_else(
                || EngineError::Dependency {
                    component: "contract_guard.model_input",
                    failure,
                },
                EngineError::ModelProposalRejected,
            ));
        }
        let assembled = assemble_capability_arguments(
            capability,
            state,
            &proposed_arguments,
            input.request,
            entrypoint,
        )?;
        let mut arguments = assembled.arguments;
        // Counted at canonicalization time — before scope validation and the
        // interpreter's admission decision — so the copy-verbatim signal
        // records the model's behavior even when this dispatch is rejected
        // downstream.
        if let Some(attribution) = canonicalize_required_gap_targeted_query(
            &capability.id,
            state.research_planner.projection(),
            &mut arguments,
        ) {
            krw_agent_persistence::metrics::record_targeted_query_attribution(attribution.as_str());
        }
        normalize_physical_capability_arguments(&capability.id, &mut arguments);
        if !arguments.is_object() {
            return Err(EngineError::ToolArgumentsMustBeObject(
                capability.id.clone(),
            ));
        }
        validate_fixed_guru_author_payload(entrypoint, &arguments)?;
        validate_capability_run_scope(
            entrypoint,
            &input.request.context,
            state.derived_ticker_scope.as_ref(),
            capability,
            &arguments,
        )?;
        contract_guard
            .validate_arguments(capability, binding, &arguments)
            .map_err(|failure| EngineError::Dependency {
                component: "contract_guard.input",
                failure,
            })?;
        let contracts = input.image.resolve_capability_contracts(capability)?;
        let normalized_output_contract_hash = contracts
            .outputs
            .iter()
            .find(|contract| contract.id == NORMALIZED_CAPABILITY_RESULT_V1)
            .ok_or_else(|| EngineError::MissingNormalizedOutputContract(capability.id.clone()))?
            .content_hash
            .clone();
        let canonical_arguments = serde_jcs::to_vec(&arguments)?;
        let request_hash = ContentHash::sha256(&canonical_arguments);
        if assembled
            .research_intent_receipt
            .as_ref()
            .is_some_and(|receipt| receipt.compiled_plan_hash != request_hash)
        {
            return Err(EngineError::Invariant(
                "research intent receipt does not bind the dispatched canonical plan",
            ));
        }
        let action_key = deterministic_action_key(
            &input.request.run_id,
            &input.image.content_hash,
            capability,
            &contracts,
            binding,
            &arguments,
        )?;
        let state_id = state
            .program
            .capability_state(&capability.id)?
            .stable_id
            .clone();
        prepared.push(PreparedCall {
            tool_call_id: call.id.clone(),
            capability: capability.clone(),
            binding: binding.clone(),
            contracts,
            model_input_contract: ContractPin {
                id: model_input.id,
                content_hash: model_input.content_hash,
            },
            normalized_output_contract_hash,
            proposed_arguments,
            arguments,
            research_intent_receipt: assembled.research_intent_receipt,
            canonical_arguments,
            request_hash,
            action_key,
            state_id,
        });
    }
    Ok(prepared)
}

/// A bounded correction instruction. Generic canonical schemas use `replace`;
/// proposal contracts may additionally carry the declared `narrow` or `split`
/// mode and a diagnostic detail (JSON pointer, offending value, valid
/// alternatives). The detail is safe to cross the provider boundary because
/// it contains only structural identifiers already advertised in the system
/// prompt, never user text, evidence, or internal state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelProposalRejection {
    Generic { reason_code: &'static str },
    ResearchProposalV4(ResearchProposalRepairDirective),
}

impl ModelProposalRejection {
    pub(crate) fn generic(reason_code: &'static str) -> Self {
        Self::Generic { reason_code }
    }

    pub(crate) fn code(&self) -> &'static str {
        match self {
            Self::Generic { reason_code } => reason_code,
            Self::ResearchProposalV4(directive) => directive.code(),
        }
    }

    pub(crate) fn repair_mode(&self) -> &'static str {
        match self {
            Self::Generic { .. } => "replace",
            Self::ResearchProposalV4(directive) => match directive.repair_mode {
                krw_agent_contracts::ResearchProposalRepairMode::Replace => "replace",
                krw_agent_contracts::ResearchProposalRepairMode::Narrow => "narrow",
                krw_agent_contracts::ResearchProposalRepairMode::Split => "split",
            },
        }
    }

    /// Returns the research-proposal violation if this rejection originated
    /// from a `ResearchProposal` v4 contract check. The caller uses the payload
    /// (offending metric, JSON pointer, valid alternatives) to build a
    /// diagnostic detail for the model recovery envelope.
    pub(crate) fn violation(&self) -> Option<&ResearchProposalViolation> {
        match self {
            Self::ResearchProposalV4(directive) => Some(&directive.violation),
            Self::Generic { .. } => None,
        }
    }
}

impl fmt::Display for ModelProposalRejection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.code())
    }
}

/// Contract-owned model input diagnostics are computed before the generic
/// guard intentionally erases semantic detail into a privacy-safe category.
/// This dispatches by immutable contract identity, never capability or user
/// question, so a new proposal family can add one declaration without a new
/// kernel behavior branch.
fn model_input_repair_directive(
    contract_id: &str,
    value: &Value,
) -> Option<ModelProposalRejection> {
    (contract_id == RESEARCH_PROPOSAL_V4)
        .then(|| research_proposal_v4_repair_directive(value))
        .flatten()
        .map(ModelProposalRejection::ResearchProposalV4)
}

/// Apply deterministic compatibility repairs for common GLM omissions.
///
/// The provider schema advertises the tagged `kind` field, but GLM can emit an
/// otherwise exact goal without that discriminator.  A qualitative
/// `{concepts, predicates}` pair is unambiguous.  Likewise, the exact metric
/// field sets identify an observation or a change goal.  We only add a tag
/// when the complete field set identifies one variant; unknown or mixed fields
/// remain a normal repair so this cannot broaden the model's scope or invent a
/// calculation.
pub(crate) fn normalize_provider_model_input(contract_id: &str, value: &mut Value) {
    if contract_id != RESEARCH_PROPOSAL_V4 {
        return;
    }
    // The provider schema exposes one `proposal` envelope, while the
    // canonical contract is the proposal itself. A few Anthropic-compatible
    // models repeat that named envelope inside the valid outer function
    // argument. It is unambiguous here because `proposal` is not a canonical
    // ResearchProposal field; unwrap exactly one duplicate layer rather than
    // wasting a repair turn on an otherwise complete research request.
    if let Some(duplicate) = value
        .as_object()
        .filter(|object| object.len() == 1)
        .and_then(|object| object.get("proposal"))
        .cloned()
    {
        *value = duplicate;
    }
    let Some(objectives) = value
        .as_object_mut()
        .and_then(|proposal| proposal.get_mut("objectives"))
        .and_then(Value::as_array_mut)
    else {
        return;
    };
    for objective in objectives.iter_mut() {
        let Some(goal) = objective.get_mut("goal").and_then(Value::as_object_mut) else {
            continue;
        };
        if goal.contains_key("kind") {
            continue;
        }
        let keys = goal
            .keys()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>();
        let inferred_kind = if keys == ["concepts", "predicates"].into_iter().collect() {
            Some("qualitative_evidence")
        } else if keys == ["metric", "metric_dimensions"].into_iter().collect() {
            Some("metric_observation")
        } else if keys
            == ["change", "metric", "metric_dimensions", "window"]
                .into_iter()
                .collect()
        {
            Some("metric_change")
        } else {
            None
        };
        if let Some(kind) = inferred_kind {
            goal.insert("kind".to_string(), Value::String(kind.to_string()));
        }
    }
    // A qualitative objective that lists several concepts without a linking
    // predicate is the most common GLM proposal rejection
    // (`QualitativePredicateMissing`, repair mode Split). The Split repair it
    // asks for is purely mechanical — one objective per concept — so apply it
    // deterministically instead of spending a scarce repair turn that
    // production showed the model failing again (scenario run died at
    // 0.4min after exhausting its single repair). Each split objective keeps
    // the original priority, alternatives, and filters; a single-concept
    // goal needs no predicate. Stay inside the 12-objective contract bound
    // or leave the proposal to the ordinary repair path.
    let qualitative_without_predicate = |objective: &Value| {
        let Some(goal) = objective.get("goal").and_then(Value::as_object) else {
            return None;
        };
        if goal.get("kind").and_then(Value::as_str) != Some("qualitative_evidence") {
            return None;
        }
        let predicates_empty = goal
            .get("predicates")
            .and_then(Value::as_array)
            .is_none_or(|predicates| predicates.is_empty());
        let concepts = goal.get("concepts")?.as_array()?;
        (predicates_empty && concepts.len() > 1).then(|| concepts.clone())
    };
    let projected = objectives
        .iter()
        .map(|objective| {
            qualitative_without_predicate(objective).map_or(1, |concepts| concepts.len())
        })
        .sum::<usize>();
    if projected <= 12 {
        let mut rebuilt = Vec::with_capacity(projected);
        let mut split_any = false;
        for objective in objectives.iter() {
            if let Some(concepts) = qualitative_without_predicate(objective) {
                split_any = true;
                for concept in concepts {
                    let mut split_objective = objective.clone();
                    if let Some(goal) = split_objective
                        .get_mut("goal")
                        .and_then(Value::as_object_mut)
                        .and_then(|goal| goal.get_mut("concepts"))
                    {
                        *goal = Value::Array(vec![concept]);
                    }
                    rebuilt.push(split_objective);
                }
            } else {
                rebuilt.push(objective.clone());
            }
        }
        if split_any {
            tracing::warn!(
                "split multi-concept qualitative objectives without predicates \
                 (deterministic QualitativePredicateMissing repair)"
            );
            *objectives = rebuilt;
        }
    }
}

/// Keep the physical response JSON-shaped while preserving the canonical
/// targeted-query detail level. `response_detail=full` is not presentation
/// fluff: it carries the quote, lineage, and observation-basis fields needed
/// for a precise follow-up to close a real research gap. The deployed MCP
/// contract accepts it, so stripping it here silently downgraded every exact
/// query to compact and made known evidence look unavailable.
pub(crate) fn normalize_physical_capability_arguments(capability_id: &str, value: &mut Value) {
    if !matches!(
        capability_id,
        "ontology.query" | "ontology.trace" | "ontology.chain"
    ) {
        return;
    }
    let Some(object) = value.as_object_mut() else {
        return;
    };
    object.remove("response_format");
}

/// Copy-verbatim attribution for one model-authored targeted query, judged
/// at the unique point where the model's original topic and the advertised
/// exact candidate coexist: kernel canonicalization inside
/// [`prepare_calls`]. The outcome is judged on the model's topic versus the
/// candidate's advertised topic, never on the final emitted arguments (the
/// dispatched topic is always the clause's canonical binding).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TargetedQueryAttribution {
    /// The model's topic equals the advertised candidate topic exactly.
    Verbatim,
    /// The model's topic was a token-subset rewrite of exactly one
    /// advertised candidate.
    Canonicalized,
    /// Candidates were advertised for the queried ticker's currently missing
    /// clauses, but the call matched none of them (including the cases where
    /// canonicalization declines: zero or ambiguous topic matches).
    Unmatched,
}

impl TargetedQueryAttribution {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Verbatim => krw_agent_persistence::metrics::ATTRIBUTION_VERBATIM,
            Self::Canonicalized => krw_agent_persistence::metrics::ATTRIBUTION_CANONICALIZED,
            Self::Unmatched => krw_agent_persistence::metrics::ATTRIBUTION_UNMATCHED,
        }
    }
}

/// Resolve a model-selected targeted query back to the exact query input that
/// the committed `ResearchState` supplied for a missing required clause, and
/// report how the model's own call attributed to the advertised candidates.
///
/// The analyst is still choosing *whether* to issue `ontology.query`; this
/// only avoids treating a harmless wording change as a different research
/// objective. The replacement is deliberately narrow:
///
/// * it applies only to the ordinary ticker-bound targeted-query capability;
/// * the model ticker must equal the candidate ticker;
/// * every model topic token must occur in the candidate topic; and
/// * exactly one current missing-clause candidate may match.
///
/// Once selected, the physical request is the ontology's own bounded
/// candidate (including period/document/object filters), not the model's
/// paraphrase. The model controls only the compact/full evidence depth. That
/// keeps goal mapping, action idempotency, restart replay, and evidence scope
/// on one canonical input without adding a model turn.
///
/// The returned [`TargetedQueryAttribution`] is the copy-verbatim
/// measurement signal (`krw_targeted_query_attribution_total`): `None` means
/// the call is not an attribution event — either it is not an ordinary
/// targeted query, it carries no topic/ticker to attribute, or no candidate
/// was advertised for the queried ticker's missing clauses. Attribution is
/// computed here, before any downstream admission or rejection, so the
/// counter records the model's behavior even when the dispatch is later
/// declined.
pub(crate) fn canonicalize_required_gap_targeted_query(
    capability_id: &str,
    projection: Option<&ResearchPlanningProjection>,
    arguments: &mut Value,
) -> Option<TargetedQueryAttribution> {
    if capability_id != "ontology.query" {
        return None;
    }
    let Some(object) = arguments.as_object() else {
        return None;
    };
    let selected_response_detail = selected_targeted_response_detail(object);
    let Some(ticker) = object.get("ticker").and_then(Value::as_str) else {
        return None;
    };
    let Some(topic) = object.get("topic").and_then(Value::as_str) else {
        return None;
    };
    let Some(projection) = projection else {
        return None;
    };
    let requested_tokens = normalized_retrieval_topic_tokens(topic);
    let ticker_token = ticker.to_lowercase();
    let requested_non_ticker = requested_tokens
        .iter()
        .filter(|token| token.as_str() != ticker_token.as_str())
        .cloned()
        .collect::<BTreeSet<_>>();

    // Eligibility is the match filter minus the topic check: candidates
    // advertised for this ticker whose clause is still missing. With none
    // present the model was never offered a candidate for this call's scope,
    // so the call is not an attribution event.
    let eligible = projection
        .exact_precise_query_candidates
        .iter()
        .filter(|candidate| {
            candidate.ticker == ticker
                && projection.missing_parts.iter().any(|missing| {
                    missing.clause_id.as_deref() == Some(candidate.clause_id.as_str())
                })
        })
        .collect::<Vec<_>>();
    if eligible.is_empty() {
        return None;
    }

    // An empty non-ticker token set (ticker-only or over-long topic) would
    // be a subset of every candidate, so it selects nothing: the call is
    // counted as unmatched rather than silently rewriten to an arbitrary
    // candidate.
    let matches = eligible
        .iter()
        .copied()
        .filter(|candidate| {
            !requested_non_ticker.is_empty()
                && requested_non_ticker
                    .is_subset(&normalized_retrieval_topic_tokens(&candidate.topic))
        })
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [candidate] => {
            let verbatim = topic == candidate.topic;
            // The advertised candidate may carry a focused filing phrase as its
            // display topic (adapter `focused_targeted_query_topic`), but
            // `map_goals` binds a targeted query to a clause by exact string
            // equality against the clause's `retrieval_query`. Dispatch the
            // canonical binding target, never the display phrase, so a verbatim
            // copy of an advertised candidate reaches the server and the planner
            // as the exact gap read the kernel itself derived.
            let canonical_topic = projection
                .clauses
                .iter()
                .find(|clause| clause.clause_id == candidate.clause_id)
                .map(|clause| clause.retrieval_query.as_str())
                .unwrap_or(candidate.topic.as_str());
            *arguments =
                exact_required_gap_arguments(candidate, canonical_topic, selected_response_detail);
            Some(if verbatim {
                TargetedQueryAttribution::Verbatim
            } else {
                TargetedQueryAttribution::Canonicalized
            })
        }
        // Zero matches, or an ambiguous multi-match the canonicalizer must
        // decline: candidates existed, but this call attributed to none.
        _ => Some(TargetedQueryAttribution::Unmatched),
    }
}

pub(crate) fn selected_targeted_response_detail(
    object: &serde_json::Map<String, Value>,
) -> &'static str {
    match object
        .get("response_detail")
        .and_then(Value::as_str)
        .unwrap_or("compact")
    {
        "full" => "full",
        _ => "compact",
    }
}

fn normalized_retrieval_topic_tokens(value: &str) -> BTreeSet<String> {
    const MAX_TOPIC_CHARS: usize = 1_024;
    const MAX_TOPIC_TOKENS: usize = 32;
    const MAX_TOKEN_CHARS: usize = 96;

    if value.chars().count() > MAX_TOPIC_CHARS {
        return BTreeSet::new();
    }
    let mut tokens = BTreeSet::new();
    let mut current = String::new();
    let flush = |current: &mut String, tokens: &mut BTreeSet<String>| {
        if !current.is_empty() && current.chars().count() <= MAX_TOKEN_CHARS {
            tokens.insert(std::mem::take(current));
        } else {
            current.clear();
        }
    };
    for character in value.chars() {
        if character.is_alphanumeric() {
            current.extend(character.to_lowercase());
        } else {
            flush(&mut current, &mut tokens);
            if tokens.len() > MAX_TOPIC_TOKENS {
                return BTreeSet::new();
            }
        }
    }
    flush(&mut current, &mut tokens);
    (tokens.len() <= MAX_TOPIC_TOKENS)
        .then_some(tokens)
        .unwrap_or_default()
}

pub(crate) fn exact_required_gap_arguments(
    candidate: &ExactTargetedQueryCandidate,
    canonical_topic: &str,
    selected_response_detail: &str,
) -> Value {
    let mut arguments = serde_json::Map::from_iter([
        ("ticker".into(), Value::String(candidate.ticker.clone())),
        ("topic".into(), Value::String(canonical_topic.to_owned())),
        (
            "response_detail".into(),
            Value::String(
                if selected_response_detail == "full" {
                    "full"
                } else {
                    "compact"
                }
                .to_owned(),
            ),
        ),
        (
            "answer_candidate_only".into(),
            Value::Bool(candidate.answer_candidate_only),
        ),
        (
            "limit".into(),
            Value::from(u64::from(candidate.limit.min(20))),
        ),
    ]);
    if !candidate.document_types.is_empty() {
        arguments.insert(
            "document_types".into(),
            Value::Array(
                candidate
                    .document_types
                    .iter()
                    .cloned()
                    .map(Value::String)
                    .collect(),
            ),
        );
    }
    if !candidate.periods.is_empty() {
        arguments.insert(
            "periods".into(),
            Value::Array(
                candidate
                    .periods
                    .iter()
                    .cloned()
                    .map(Value::String)
                    .collect(),
            ),
        );
    }
    if !candidate.object_types.is_empty() {
        arguments.insert(
            "object_types".into(),
            Value::Array(
                candidate
                    .object_types
                    .iter()
                    .cloned()
                    .map(Value::String)
                    .collect(),
            ),
        );
    }
    Value::Object(arguments)
}

/// Maps only model-controlled canonical input failures to a repairable,
/// stable reason. Registry pins, unknown contracts, bindings, permissions,
/// and scope failures intentionally remain terminal: retrying a model must
/// never disguise an image/deployment/security fault as a prompt-quality
/// problem.
fn model_input_rejection_code(failure: &DependencyFailure) -> Option<ModelProposalRejection> {
    match failure.code.as_str() {
        "canonical_input_shape_invalid" => {
            Some(ModelProposalRejection::generic("model_input_shape_invalid"))
        }
        "canonical_input_semantic_invalid" => Some(ModelProposalRejection::generic(
            "model_input_semantic_invalid",
        )),
        "canonical_input_limit_exceeded" => Some(ModelProposalRejection::generic(
            "model_input_limit_exceeded",
        )),
        "canonical_input_serialization_invalid" => Some(ModelProposalRejection::generic(
            "model_input_serialization_invalid",
        )),
        _ => None,
    }
}

/// Extract a bounded diagnostic from a research-proposal violation. Only
/// `MetricIdentityInvalid` carries enough context to be actionable; other
/// violations return `None` (the `reason_code` alone suffices).
pub(crate) fn violation_to_detail(violation: &ResearchProposalViolation) -> RecoveryDetailV1 {
    match violation {
        ResearchProposalViolation::MetricIdentityInvalid {
            offending,
            valid,
            pointer,
        } => RecoveryDetailV1 {
            schema_version: 1,
            field: pointer.clone(),
            offending_value: offending.clone(),
            valid_alternatives: valid.clone(),
            hint: "Replace the offending metric identifier with one of the \
                   valid_alternatives. Use canonical identifiers from the ontology \
                   catalog, not aliases."
                .to_string(),
        },
        ResearchProposalViolation::ShapeInvalid {
            pointer,
            offending,
            allowed,
        } => RecoveryDetailV1 {
            schema_version: 1,
            field: pointer.clone(),
            offending_value: offending.clone().unwrap_or_default(),
            valid_alternatives: allowed.clone(),
            hint: if offending.is_some() && !allowed.is_empty() {
                "The field identified by detail.field has an invalid value. \
                 Use one of detail.valid_alternatives instead. If the field is \
                 an unknown key, remove it — the proposal must not be \
                 double-wrapped or have extra top-level keys."
                    .to_string()
            } else {
                "The proposal has a structural error. Check that top-level keys \
                 are exactly: answer_scope, document_types, intent, objectives, \
                 periods, uncertainty. Do not wrap the proposal in an extra \
                 object."
                    .to_string()
            },
        },
        ResearchProposalViolation::RequiredObjectiveMissing => RecoveryDetailV1 {
            schema_version: 1,
            field: "/objectives".to_string(),
            offending_value: String::new(),
            valid_alternatives: vec!["required".into(), "deferred".into()],
            hint: "At least one objective must have priority \"required\".".to_string(),
        },
        ResearchProposalViolation::QualitativeConceptsMissing { pointer } => RecoveryDetailV1 {
            schema_version: 1,
            field: pointer.clone(),
            offending_value: String::new(),
            valid_alternatives: vec![],
            hint: "The concepts array must be non-empty (1–16 strings).".to_string(),
        },
        ResearchProposalViolation::QualitativePredicateMissing { pointer } => RecoveryDetailV1 {
            schema_version: 1,
            field: pointer.clone(),
            offending_value: String::new(),
            valid_alternatives: vec![],
            hint: "When the concepts array has 2+ entries, predicates must \
                   be a non-empty array (1–16 strings)."
                .to_string(),
        },
        ResearchProposalViolation::LimitExceeded => RecoveryDetailV1 {
            schema_version: 1,
            field: "/".to_string(),
            offending_value: String::new(),
            valid_alternatives: vec![],
            hint: "The proposal exceeds a size or count limit. Reduce the number \
                   of objectives, alternatives, or terms."
                .to_string(),
        },
        ResearchProposalViolation::SerializationInvalid => RecoveryDetailV1 {
            schema_version: 1,
            field: "/".to_string(),
            offending_value: String::new(),
            valid_alternatives: vec![],
            hint: "The proposal could not be canonicalized. Reduce nesting or \
                   remove non-finite values."
                .to_string(),
        },
    }
}

impl<P, C, S> RunEngine<P, C, S>
where
    P: Provider,
    C: CapabilityRuntime,
    S: Persistence,
{
    pub(crate) async fn execute_action(
        &self,
        context: &ActionExecutionContext<'_>,
        call: &PreparedCall,
    ) -> Result<CapabilityResult, EngineError> {
        let identity = context.identity;
        let episode_hash = context.episode_hash;
        let deadline = context.deadline;
        let max_result_bytes = context.max_result_bytes;
        let mut action = AuthorizedAction::proposed(
            call.action_key.clone(),
            call.request_hash.clone(),
            episode_hash.clone(),
        );
        action.episode_committed()?;
        let intent = ActionIntent {
            mutation: BeginActionMutation {
                run_id: identity.run_id.clone(),
                fencing_token: identity.fencing_token,
                mutation_id: mutation_id(
                    "begin_action",
                    &identity.run_id,
                    &ContentHash::sha256(&call.action_key),
                ),
                action_key: call.action_key.clone(),
                request_hash: call.request_hash.clone(),
                retryable_read: true,
            },
            episode_hash: episode_hash.clone(),
            tool_call_id: call.tool_call_id.clone(),
            capability_id: call.capability.id.clone(),
            input_contract: call.capability.input_contract.clone(),
            output_contracts: call.capability.output_contracts.clone(),
            input_schema_hash: call.contracts.input.content_hash.clone(),
            output_schema_hash: call.contracts.output_contract_set_hash.clone(),
            normalized_output_contract_hash: call.normalized_output_contract_hash.clone(),
            server_schema_bundle_hash: call.binding.server_schema_bundle_hash.clone(),
            data_release_hash: call.binding.data_release_hash.clone(),
            canonical_arguments: call.canonical_arguments.clone(),
        };
        let receipt = dependency_call(
            deadline,
            "persistence.begin_action",
            self.persistence.begin_action(&intent),
        )
        .await?;
        validate_action_receipt(&receipt, call)?;

        match receipt.stage {
            ActionStage::Begun => {
                action.bind_receipt(&receipt)?;
                self.guard_control(identity, deadline).await?;
                action.mark_dispatched()?;
                // Local skill-body lookup (progressive disclosure). The body is
                // resolved from the immutable image blob store — no MCP round
                // trip. Falls through to the normal MCP path for any other
                // capability id.
                let result = if call.capability.id == "skill.load" {
                    let arguments: Value = serde_json::from_slice(&call.canonical_arguments)
                        .map_err(|_| EngineError::Invariant("skill.load canonical arguments"))?;
                    invoke_skill_load(context.image, &arguments)?
                } else {
                    let invocation = capability_invocation(&identity.run_id, call);
                    let cap_span = tracing::info_span!("capability", id = %identity.run_id);
                    cap_span.in_scope(|| {
                        tracing::debug!(capability = %call.capability.id, "dispatching");
                    });
                    let dispatch_t0 = Instant::now();
                    let dispatch_outcome =
                        await_until(deadline, self.capabilities.invoke(&invocation)).await;
                    let dispatch_ms = elapsed_millis(dispatch_t0);
                    let _ = &cap_span;
                    match dispatch_outcome {
                        Ok(Ok(result)) => {
                            krw_agent_persistence::metrics::record_capability_call(
                                &call.capability.id,
                                "success",
                            );
                            krw_agent_persistence::metrics::record_capability_duration_seconds(
                                &call.capability.id,
                                "success",
                                dispatch_ms,
                            );
                            result
                        }
                        Ok(Err(failure)) => {
                            krw_agent_persistence::metrics::record_capability_call(
                                &call.capability.id,
                                "error",
                            );
                            krw_agent_persistence::metrics::record_capability_duration_seconds(
                                &call.capability.id,
                                "error",
                                dispatch_ms,
                            );
                            if failure.delivery == DeliveryCertainty::MayHaveDispatched {
                                self.record_ambiguous(
                                    identity,
                                    call,
                                    "capability_failure",
                                    deadline,
                                )
                                .await?;
                            }
                            return Err(EngineError::Dependency {
                                component: "capability",
                                failure,
                            });
                        }
                        Err(()) => {
                            krw_agent_persistence::metrics::record_capability_call(
                                &call.capability.id,
                                "error",
                            );
                            krw_agent_persistence::metrics::record_capability_duration_seconds(
                                &call.capability.id,
                                "error",
                                dispatch_ms,
                            );
                            self.record_ambiguous(identity, call, "capability_timeout", deadline)
                                .await?;
                            return Err(EngineError::DeadlineExceeded("capability"));
                        }
                    }
                };
                let result_bytes = serde_jcs::to_vec(&result)?;
                if let Err(error) =
                    ensure_size(result_bytes.len(), max_result_bytes, "capability_result")
                {
                    self.best_effort_ambiguous(identity, call, "result_too_large")
                        .await;
                    return Err(error);
                }
                let result_hash = ContentHash::sha256(&result_bytes);
                action.observe(result_hash.clone())?;
                if let Err(error) = self.guard_control(identity, deadline).await {
                    self.best_effort_ambiguous(identity, call, "control_changed_after_dispatch")
                        .await;
                    return Err(error);
                }
                let observation = DurableActionObservation {
                    mutation: ObserveActionMutation {
                        run_id: identity.run_id.clone(),
                        fencing_token: identity.fencing_token,
                        mutation_id: action_mutation_id(
                            "observe_action",
                            &identity.run_id,
                            &call.action_key,
                            &result_hash,
                        ),
                        action_key: call.action_key.clone(),
                        result_hash: result_hash.clone(),
                    },
                    result_bytes,
                };
                let observed = match await_until(
                    deadline,
                    self.persistence.observe_action(&observation),
                )
                .await
                {
                    Ok(Ok(receipt)) => receipt,
                    Ok(Err(failure)) => {
                        self.best_effort_ambiguous(identity, call, "observation_failed")
                            .await;
                        return Err(EngineError::Dependency {
                            component: "persistence.observe_action",
                            failure,
                        });
                    }
                    Err(()) => {
                        self.best_effort_ambiguous(identity, call, "observation_timeout")
                            .await;
                        return Err(EngineError::DeadlineExceeded("persistence.observe_action"));
                    }
                };
                validate_observed_receipt(&observed, call, &result_hash)?;
                let accepted = self
                    .finalize_observed_action(context, call, result, result_hash.clone())
                    .await?;
                action.accept(&result_hash)?;
                Ok(accepted)
            }
            ActionStage::Observed | ActionStage::Accepted | ActionStage::Rejected => {
                let result_hash =
                    receipt
                        .result_hash
                        .as_ref()
                        .ok_or(EngineError::InvalidActionReceipt(
                            "replayed action is missing result_hash",
                        ))?;
                let bytes = dependency_call(
                    deadline,
                    "persistence.load_action_result",
                    self.persistence
                        .load_action_result(identity, &call.action_key, result_hash),
                )
                .await?
                .ok_or(EngineError::MissingDurableActionResult)?;
                ensure_size(bytes.len(), max_result_bytes, "capability_result")?;
                if ContentHash::sha256(&bytes) != *result_hash {
                    return Err(EngineError::DurableResultHashMismatch);
                }
                let result: CapabilityResult = serde_json::from_slice(&bytes)?;
                self.finalize_observed_action(context, call, result, result_hash.clone())
                    .await
            }
            ActionStage::Ambiguous => Err(EngineError::AmbiguousAction(call.action_key.clone())),
        }
    }

    async fn finalize_observed_action(
        &self,
        context: &ActionExecutionContext<'_>,
        call: &PreparedCall,
        result: CapabilityResult,
        result_hash: ContentHash,
    ) -> Result<CapabilityResult, EngineError> {
        let identity = context.identity;
        let deadline = context.deadline;
        let evaluation =
            self.evaluate_after_action(context.image, context.state, call, &result, &result_hash)?;
        let disposition = if evaluation.accepted {
            ActionDisposition::Accepted
        } else {
            ActionDisposition::Rejected
        };
        let mutation_fingerprint = ContentHash::sha256(serde_jcs::to_vec(&serde_json::json!({
            "disposition": disposition,
            "policy_receipt_hash": evaluation.policy_receipt_hash,
            "result_hash": result_hash,
            "validation_receipt_hash": evaluation.validation_receipt_hash,
        }))?);
        let finalized = dependency_call(
            deadline,
            "persistence.finalize_action",
            self.persistence.finalize_action(FinalizeActionMutation {
                run_id: identity.run_id.clone(),
                fencing_token: identity.fencing_token,
                mutation_id: action_mutation_id(
                    "finalize_action",
                    &identity.run_id,
                    &call.action_key,
                    &mutation_fingerprint,
                ),
                action_key: call.action_key.clone(),
                result_hash: result_hash.clone(),
                disposition,
                validation_receipt_hash: evaluation.validation_receipt_hash.clone(),
                policy_receipt_hash: evaluation.policy_receipt_hash.clone(),
            }),
        )
        .await?;
        validate_finalized_receipt(
            &finalized,
            call,
            &result_hash,
            disposition,
            &evaluation.validation_receipt_hash,
            &evaluation.policy_receipt_hash,
        )?;
        if disposition == ActionDisposition::Rejected {
            return Err(EngineError::ActionRejected(call.action_key.clone()));
        }
        Ok(result)
    }

    pub(crate) fn evaluate_after_action(
        &self,
        image: &AgentImageManifest,
        state: &ActiveRun,
        call: &PreparedCall,
        result: &CapabilityResult,
        result_hash: &ContentHash,
    ) -> Result<AfterActionEvaluation, EngineError> {
        let mut validation_issues = BTreeSet::new();
        if self
            .config
            .contract_guard
            .validate_result(&call.capability, &call.binding, result)
            .is_err()
        {
            validation_issues.insert("canonical_output_invalid".to_owned());
        }
        if validate_capability_result(call, result).is_err() {
            validation_issues.insert("provenance_invalid".to_owned());
        }
        let validation_receipt_hash =
            ContentHash::sha256(serde_jcs::to_vec(&AfterActionValidationReceipt {
                schema_version: 1,
                action_key: &call.action_key,
                capability_id: &call.capability.id,
                request_hash: &call.request_hash,
                result_hash,
                output_contract_set_hash: &call.contracts.output_contract_set_hash,
                data_release_hash: &call.binding.data_release_hash,
                issues: &validation_issues,
            })?);

        let mut policy = PolicyAccumulator::new(PolicyCeiling {
            capabilities: BTreeSet::from([call.capability.id.clone()]),
            context_refs: BTreeSet::new(),
            budget: state.limits.clone(),
            verifier_tier: VerifierTier::Structural,
            claim_strengths: BTreeMap::new(),
        })?;
        if !validation_issues.is_empty() {
            policy.apply(PolicyDecision {
                policy_id: "kernel_after_action_validation".into(),
                phase: PolicyPhase::AfterAction,
                authority: PolicyAuthority::KernelInvariant,
                effect: PolicyEffect::Deny {
                    reason_code: "action_validation_failed".into(),
                },
            })?;
        } else if state.validate_post_action(image, call, result).is_err() {
            policy.apply(PolicyDecision {
                policy_id: "agent_after_action_policy".into(),
                phase: PolicyPhase::AfterAction,
                authority: PolicyAuthority::AgentImage,
                effect: PolicyEffect::Deny {
                    reason_code: "action_policy_rejected".into(),
                },
            })?;
        }
        let policy = policy.finish();
        let accepted = validation_issues.is_empty() && !policy.denied;
        Ok(AfterActionEvaluation {
            accepted,
            validation_receipt_hash,
            policy_receipt_hash: policy.content_hash()?,
        })
    }

    async fn record_ambiguous(
        &self,
        identity: &RunIdentity,
        call: &PreparedCall,
        reason: &str,
        _deadline: Instant,
    ) -> Result<(), EngineError> {
        let safety_deadline = Instant::now()
            .checked_add(self.config.safety_write_timeout)
            .ok_or(EngineError::InvalidInput("safety deadline overflow"))?;
        dependency_call(
            safety_deadline,
            "persistence.mark_action_ambiguous",
            self.persistence.mark_action_ambiguous(MarkActionAmbiguous {
                run_id: identity.run_id.clone(),
                fencing_token: identity.fencing_token,
                mutation_id: mutation_id(
                    "mark_action_ambiguous",
                    &identity.run_id,
                    &ContentHash::sha256(&call.action_key),
                ),
                action_key: call.action_key.clone(),
                reason_code: reason.into(),
            }),
        )
        .await
    }

    async fn best_effort_ambiguous(
        &self,
        identity: &RunIdentity,
        call: &PreparedCall,
        reason: &str,
    ) {
        drop(
            self.record_ambiguous(identity, call, reason, Instant::now())
                .await,
        );
    }
}
