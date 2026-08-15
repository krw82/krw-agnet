//! Final answer policy: parse, integrity-validate, sanitize, render, and
//! atomically commit the core answer.  Presentation packs are compiled here
//! but are optional and never fail the text answer.

use super::*;

/// A private pack is only useful when it came from the same immutable ontology
/// release as the model-visible ResearchState that accompanied the call.  A
/// mismatch is an optional presentation defect, never a research failure.
pub(crate) fn presentation_pack_matches_result(pack: &Value, provider_content: &Value) -> bool {
    let pack_release = pack
        .get("release_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let result_release = provider_content
        .get("release_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    match (pack_release, result_release) {
        (Some(pack_release), Some(result_release)) => pack_release == result_release,
        (None, None) => true,
        // A production ResearchState always carries a release identity.  Do
        // not render a pack when only one side advertises provenance.
        _ => false,
    }
}

/// Return the only source references a committed visualization may use.  The
/// chart sidecar identifies ontology objects, while the ledger identifies
/// normalized evidence records, so both namespaces are intentionally allowed
/// after the current-run relation has been observed.
fn current_presentation_evidence_refs(ledger: &EvidenceLedger) -> BTreeSet<String> {
    let mut refs = BTreeSet::new();
    for (evidence_id, _) in ledger.iter() {
        let Some(active) = ledger.active(evidence_id) else {
            continue;
        };
        if active.evidence_id != evidence_id {
            continue;
        }
        refs.insert(evidence_id.to_owned());
        refs.extend(active.source_object_ids.iter().cloned());
    }
    refs
}

/// Keep an artifact only when every source/derived evidence reference belongs
/// to the current run.  A partial chart is more misleading than no chart, so
/// the unit is dropped as a whole and the text answer remains untouched.
fn filter_grounded_visualizations(
    artifacts: Vec<Value>,
    allowed_refs: &BTreeSet<String>,
    limit: usize,
) -> Vec<Value> {
    artifacts
        .into_iter()
        .filter(|artifact| {
            let mut refs = BTreeSet::new();
            collect_visualization_evidence_refs(artifact, &mut refs);
            !refs.is_empty()
                && refs
                    .iter()
                    .all(|reference| allowed_refs.contains(reference))
        })
        .take(limit)
        .collect()
}

fn collect_visualization_evidence_refs(value: &Value, refs: &mut BTreeSet<String>) {
    match value {
        Value::Object(object) => {
            for (key, child) in object {
                match key.as_str() {
                    "evidence_ref" => {
                        if let Some(reference) = child.as_str().map(str::trim)
                            && !reference.is_empty()
                        {
                            refs.insert(reference.to_owned());
                        }
                    }
                    "evidence_refs" | "derived_from_evidence_refs" => {
                        if let Some(values) = child.as_array() {
                            refs.extend(
                                values
                                    .iter()
                                    .filter_map(Value::as_str)
                                    .map(str::trim)
                                    .filter(|reference| !reference.is_empty())
                                    .map(str::to_owned),
                            );
                        }
                    }
                    _ => {}
                }
                collect_visualization_evidence_refs(child, refs);
            }
        }
        Value::Array(values) => {
            for child in values {
                collect_visualization_evidence_refs(child, refs);
            }
        }
        _ => {}
    }
}

/// Whether an already-admitted evidence result should use the workflow's
/// declared composition fallback because the next research decision has no
/// remaining state visit.  This is deliberately narrower than a generic
/// "finish early" rule: it applies only while sitting in the kernel-owned
/// evidence-ingest builtin, only after an `ingested` model successor has been
/// exhausted, and only when the image explicitly provides an answer-producing
/// `output_budget_reserved` edge.
pub(crate) fn finalize_after_exhausted_ingest_successor(
    workflow: &CompiledWorkflow,
    current_state: u16,
    state_available: impl Fn(&CompiledState) -> Result<bool, EngineError>,
    answer_contract: &ContractPin,
) -> Result<bool, EngineError> {
    let current = workflow
        .states
        .iter()
        .find(|state| state.numeric_id == current_state)
        .ok_or(EngineError::InvalidStateProgram)?;
    if !matches!(
        current.operation,
        StateOperation::Builtin {
            handler: BuiltinHandler::IngestEvidence,
            ..
        }
    ) {
        return Ok(false);
    }

    let mut has_next_research_decision = false;
    let mut next_research_decision_available = false;
    for transition in workflow
        .transitions
        .iter()
        .filter(|transition| transition.from == current_state && transition.event == "ingested")
    {
        let target = workflow
            .states
            .iter()
            .find(|state| state.numeric_id == transition.to)
            .ok_or(EngineError::InvalidStateProgram)?;
        if matches!(target.operation, StateOperation::ModelDecision { .. }) {
            has_next_research_decision = true;
            next_research_decision_available |= state_available(target)?;
        }
    }
    if !has_next_research_decision || next_research_decision_available {
        return Ok(false);
    }

    let mut answer_fallbacks = 0_u8;
    for transition in workflow.transitions.iter().filter(|transition| {
        transition.from == current_state && transition.event == "output_budget_reserved"
    }) {
        let target = workflow
            .states
            .iter()
            .find(|state| state.numeric_id == transition.to)
            .ok_or(EngineError::InvalidStateProgram)?;
        if matches!(
            &target.operation,
            StateOperation::ModelDecision {
                output_mode: ModelOutputMode::TypedJson | ModelOutputMode::Markdown,
                output_contracts,
                ..
            } if output_contracts.contains(answer_contract)
        ) && state_available(target)?
        {
            answer_fallbacks = answer_fallbacks
                .checked_add(1)
                .ok_or(EngineError::CounterOverflow("ingest composition fallbacks"))?;
        }
    }
    Ok(answer_fallbacks == 1)
}

pub(crate) fn derive_result_scope_projection(
    capability: &CapabilitySpec,
    capability_id: &str,
    provider_content: &Value,
) -> Result<Option<DerivedTickerScope>, EngineError> {
    let Some(spec) = &capability.scope_projection else {
        return Ok(None);
    };
    if !capability
        .output_contracts
        .iter()
        .any(|contract| contract == &spec.output_contract)
    {
        return Err(EngineError::Invariant(
            "scope projection output is absent from the capability contract set",
        ));
    }
    validate_canonical_value(&spec.output_contract, provider_content).map_err(|error| {
        EngineError::CanonicalRegistry(format!("scope projection output: {error:?}"))
    })?;
    let tickers = match spec.kind {
        ScopeProjectionKind::TickerSet => provider_content
            .pointer(&spec.source_pointer)
            .and_then(Value::as_array)
            .ok_or(EngineError::RunScopeViolation(
                "scope projection omitted its declared ticker set",
            ))?
            .iter()
            .map(Value::as_str)
            .collect::<Option<Vec<_>>>()
            .ok_or(EngineError::RunScopeViolation(
                "scope projection ticker set contains a non-string value",
            ))?
            .into_iter()
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>(),
    };
    if tickers.len() > usize::from(spec.max_items)
        || (spec.require_nonempty && tickers.is_empty())
        || tickers.iter().any(|ticker| !is_canonical_ticker(ticker))
    {
        return Err(EngineError::RunScopeViolation(
            "scope projection ticker set is outside its canonical bound",
        ));
    }
    let supplied_len = tickers.len();
    let mut tickers = tickers;
    tickers.sort();
    tickers.dedup();
    if tickers.len() != supplied_len
        || tickers.len() > usize::from(spec.max_items)
        || (spec.require_nonempty && tickers.is_empty())
    {
        return Err(EngineError::RunScopeViolation(
            "scope projection ticker set is duplicated or empty",
        ));
    }
    Ok(Some(DerivedTickerScope {
        producer_capability_id: capability_id.to_owned(),
        output_contract: spec.output_contract.clone(),
        source_hash: ContentHash::sha256(serde_jcs::to_vec(provider_content)?),
        tickers,
    }))
}

fn answer_error_code(error: &EngineError) -> &'static str {
    match error {
        EngineError::Json(_) => "answer_ir_json_invalid",
        EngineError::UncommittedCalculation(_) | EngineError::CalculationConflict(_) => {
            "calculation_lineage_invalid"
        }
        EngineError::AnswerValidation(_) => "evidence_validation_failed",
        EngineError::RuleViolations(_) | EngineError::PhaseRuleViolations { .. } => {
            "answer_policy_failed"
        }
        _ => "answer_verification_failed",
    }
}

/// GLM's JSON-object mode guarantees an object-oriented response, but the
/// Anthropic-compatible endpoint can still wrap that object in a Markdown
/// fence or a short explanatory prefix. Keep the canonical schema validation
/// strict while accepting only a recoverable JSON object from that wrapper.
/// No prose is interpreted as an answer: the extracted value still goes
/// through the pinned contract and evidence-linkage validators below.
pub(crate) fn parse_typed_json_content(content: &str) -> Result<Value, serde_json::Error> {
    let trimmed = content.trim();
    if let Ok(value) = serde_json::from_str(trimmed) {
        return Ok(value);
    }

    let unfenced = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```JSON"))
        .or_else(|| trimmed.strip_prefix("```"))
        .and_then(|body| body.strip_suffix("```"))
        .map(str::trim)
        .unwrap_or(trimmed);
    if let Ok(value) = serde_json::from_str(unfenced) {
        return Ok(value);
    }

    if let (Some(start), Some(end)) = (unfenced.find('{'), unfenced.rfind('}')) {
        if start <= end {
            if let Ok(value) = serde_json::from_str(&unfenced[start..=end]) {
                return Ok(value);
            }
        }
    }

    // Preserve the original serde error for the normal repair/error taxonomy.
    serde_json::from_str(trimmed)
}

fn validate_typed_output(
    input: &RunInput<'_>,
    state: &ActiveRun,
    contract: &ContractPin,
    output: &Value,
) -> Result<Option<AnswerIr>, EngineError> {
    verify_pin(&contract.id, &contract.content_hash)
        .map_err(|error| EngineError::CanonicalRegistry(format!("{error:?}")))?;
    validate_fixed_guru_author_payload(selected_entrypoint(input.image, input.request)?, output)?;

    let answer_ir = if contract.id == ANSWER_IR_V1 {
        let mut answer_ir: AnswerIr = serde_json::from_value(output.clone())?;
        bind_kernel_goal_ids(&mut answer_ir, state);
        normalize_answer_calculation_lineage(&mut answer_ir, state);
        normalize_answer_section_headings(&mut answer_ir, &answer_policy(input.image));
        validate_calculations(&answer_ir, &state.calculations)?;
        validate_answer(&answer_ir, &state.ledger, &answer_policy(input.image))
            .map_err(|issues| EngineError::AnswerValidation(issue_codes(&issues)))?;
        validate_kernel_goal_bindings(&answer_ir, state)?;
        Some(answer_ir)
    } else {
        None
    };
    for program in input
        .image
        .body
        .validators
        .iter()
        .filter(|program| program.phase == RulePhase::Answer)
    {
        let evaluation = evaluate_rule_program(program, output)?;
        if !evaluation.violations.is_empty() {
            return Err(EngineError::RuleViolations(
                evaluation
                    .violations
                    .into_iter()
                    .map(|violation| violation.code)
                    .collect(),
            ));
        }
    }
    Ok(answer_ir)
}

/// Research-goal aliases are created by the kernel after proposal lowering,
/// surfaced only in the tool result, and must be reused verbatim by grounded
/// answer claims. This prevents a final model turn from inventing a new
/// semantic target after evidence collection has finished.
fn validate_kernel_goal_bindings(answer: &AnswerIr, state: &ActiveRun) -> Result<(), EngineError> {
    let Some(projection) = state.research_planner.intent_projection() else {
        return Ok(());
    };
    let known = projection
        .graph
        .goals()
        .map(|goal| goal.goal_id.as_str())
        .collect::<BTreeSet<_>>();
    let mut codes = BTreeSet::new();
    for claim in &answer.claims {
        if claim.kind == krw_agent_evidence::ClaimKind::Uncertainty {
            continue;
        }
        if claim.goal_ids.is_empty() {
            codes.insert("claim_missing_kernel_goal_binding".to_owned());
        }
        if claim
            .goal_ids
            .iter()
            .any(|goal_id| !known.contains(goal_id.as_str()))
        {
            codes.insert("claim_unknown_kernel_goal_binding".to_owned());
        }
    }
    if codes.is_empty() {
        Ok(())
    } else {
        Err(EngineError::AnswerValidation(codes.into_iter().collect()))
    }
}

/// Goal aliases are kernel-owned linkage, not model-authored content. The
/// composer may leave `goal_ids` empty (or repeat stale aliases from an older
/// tool result); bind every grounded claim to the immutable goal set after
/// deserialization and before validation. This keeps final-answer generation
/// from failing on opaque planner identifiers while preserving the invariant
/// that no unknown goal can be committed.
fn bind_kernel_goal_ids(answer: &mut AnswerIr, state: &ActiveRun) {
    let Some(projection) = state.research_planner.intent_projection() else {
        return;
    };
    let goal_ids = projection
        .graph
        .goals()
        .map(|goal| goal.goal_id.clone())
        .collect::<Vec<_>>();
    if goal_ids.is_empty() {
        return;
    }
    for claim in &mut answer.claims {
        if claim.kind != krw_agent_evidence::ClaimKind::Uncertainty {
            claim.goal_ids = goal_ids.clone();
        }
    }
}

/// A composer can mention a plausible calculation identifier that was never
/// emitted by the evidence capabilities. Never let that model-owned lineage
/// become a final-answer failure or a trusted calculation: retain only exact
/// calculations committed by the kernel, clear unknown references, and
/// downgrade a now-unlinked numeric claim to a grounded fact. The evidence
/// validator still requires real evidence for that fact.
fn normalize_answer_calculation_lineage(answer: &mut AnswerIr, state: &ActiveRun) {
    answer.calculations.retain(|calculation| {
        state.calculations.get(&calculation.calculation_id) == Some(calculation)
    });
    for claim in &mut answer.claims {
        claim
            .calculation_ids
            .retain(|calculation_id| state.calculations.contains_key(calculation_id));
        claim
            .counter_evidence_ids
            .retain(|evidence_id| state.ledger.active(evidence_id).is_some());
        if claim.kind == krw_agent_evidence::ClaimKind::Number && claim.calculation_ids.is_empty() {
            claim.kind = krw_agent_evidence::ClaimKind::Fact;
        }
        if claim.kind == krw_agent_evidence::ClaimKind::Interpretation
            && claim.counter_evidence_ids.is_empty()
        {
            claim.kind = krw_agent_evidence::ClaimKind::Fact;
        }
    }
}

/// Keep model-authored section labels user-facing and single-line. A heading
/// is presentation metadata, so a malformed label should not discard an
/// otherwise grounded answer; fall back to a small safe label only when the
/// model supplied an empty, oversized, or internal-only value.
fn normalize_answer_section_headings(answer: &mut AnswerIr, policy: &AnswerPolicy) {
    const FALLBACKS: [&str; 4] = ["결론", "근거", "반대 신호", "확인 조건"];
    for (index, section) in answer.sections.iter_mut().enumerate() {
        let normalized = section
            .heading
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let contains_internal = policy
            .forbidden_terms
            .iter()
            .any(|term| normalized.to_lowercase().contains(&term.to_lowercase()));
        if normalized.is_empty() || normalized.len() > 80 || contains_internal {
            section.heading = FALLBACKS.get(index).map_or_else(
                || format!("핵심 판단 {}", index + 1),
                |value| (*value).to_owned(),
            );
        } else {
            section.heading = normalized;
        }
    }
}

fn render_typed_output(
    contract: &ContractPin,
    output: &Value,
    answer_ir: Option<&AnswerIr>,
    ledger: &EvidenceLedger,
) -> Result<String, EngineError> {
    if contract.id == ANSWER_IR_V1 {
        return render_markdown(
            answer_ir.ok_or(EngineError::Invariant(
                "AnswerIR contract was validated without typed AnswerIR",
            ))?,
            ledger,
        )
        .map_err(EngineError::from);
    }
    if contract.id == NOTEBOOK_TRANSFORM_V2 {
        let transform: NotebookTransformV2 = serde_json::from_value(output.clone())?;
        return match transform {
            NotebookTransformV2::NotebookMarkdown { markdown, .. } => Ok(markdown),
            NotebookTransformV2::OpenQuestions { questions, .. } => Ok(questions.join("\n")),
        };
    }
    match output {
        Value::String(value) => Ok(value.clone()),
        Value::Array(values) if values.iter().all(Value::is_string) => Ok(values
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join("\n")),
        _ => String::from_utf8(serde_jcs::to_vec(output)?)
            .map_err(|_| EngineError::Invariant("canonical output was not UTF-8")),
    }
}

pub(crate) fn validate_product_output_linkage(
    request: &RunRequest,
    contract: &ContractPin,
    output: &Value,
) -> Result<(), EngineError> {
    match contract.id.as_str() {
        ROUTING_DECISION_V2 => {
            let routing = routing_input(request)?;
            let decision: RoutingDecisionV2 = serde_json::from_value(output.clone())?;
            validate_routing_linkage(&routing, &decision)
                .map_err(|error| EngineError::ProductContract(format!("{error:?}")))?;
        }
        NOTEBOOK_TRANSFORM_V2 => {
            let notebook = notebook_input(request)?;
            let transform: NotebookTransformV2 = serde_json::from_value(output.clone())?;
            validate_notebook_linkage(&notebook, &transform)
                .map_err(|error| EngineError::ProductContract(format!("{error:?}")))?;
        }
        DISPLAY_PLAN_V2 => {
            let source = committed_display_source(request)?;
            let plan: DisplayPlanV2 = serde_json::from_value(output.clone())?;
            validate_display_plan_linkage(&source, &plan)
                .map_err(|error| EngineError::ProductContract(format!("{error:?}")))?;
        }
        _ => {}
    }
    Ok(())
}

impl<P, C, S> RunEngine<P, C, S>
where
    P: Provider,
    C: CapabilityRuntime,
    S: Persistence,
{
    pub(crate) async fn finish(
        &self,
        input: &RunInput<'_>,
        identity: &RunIdentity,
        episode: &ProviderEpisodeV1,
        state: &mut ActiveRun,
        execution: ExecutionState,
        constraint_mode: ProviderConstraintMode,
        deadline: Instant,
    ) -> Result<Option<RunOutcome>, EngineError> {
        if episode.finish_reason != "stop" {
            if constraint_mode == ProviderConstraintMode::JsonSchema {
                return Err(EngineError::ProviderConstrainedOutputIncomplete(
                    "finish reason",
                ));
            }
            if episode.finish_reason == "length"
                && state.current_operation_emits_answer(input.image)?
                && !state.direct_answer_retry_requested()
            {
                // A truncated final answer is neither evidence nor a useful
                // replay turn. Keep it out of the next context and switch the
                // next bounded attempt to direct visible-output mode. This is
                // deliberately not a generic repair reservation: a planner
                // repair must not consume the only safe final-answer fallback.
                state.request_direct_answer_retry();
                state.append_direct_answer_retry_feedback("answer_output_truncated");
                state.check_conversation_limit(self.config.max_conversation_bytes)?;
                return Ok(None);
            }
            return Err(EngineError::InvalidProviderEpisode(
                "final episode must finish with stop",
            ));
        }
        if !state.current_operation_emits_answer(input.image)? {
            return Err(EngineError::WorkflowResolution {
                outcome: "answer composition",
            });
        }
        let content = match episode.assistant.content.as_deref() {
            Some(content) if !content.trim().is_empty() => content,
            _ if constraint_mode == ProviderConstraintMode::JsonSchema => {
                return Err(EngineError::ProviderConstrainedOutputIncomplete(
                    "final content",
                ));
            }
            _ if !state.direct_answer_retry_requested() => {
                state.request_direct_answer_retry();
                state.append_assistant(episode);
                state.append_direct_answer_retry_feedback("final_output_missing");
                state.check_conversation_limit(self.config.max_conversation_bytes)?;
                return Ok(None);
            }
            _ => {
                return Err(EngineError::InvalidProviderEpisode(
                    "final episode has no content",
                ));
            }
        };
        ensure_size(
            content.len(),
            self.config.max_model_output_bytes,
            "final_output",
        )?;
        let output_contract =
            ContractPin::canonical(&input.image.body.answer_policy.internal_format)?;
        let final_output_mode = state.current_model_output_mode()?;
        let (output, answer_ir, rendered_content) = match final_output_mode {
            ModelOutputMode::Markdown => {
                let output = Value::String(content.to_owned());
                validate_canonical_value(&output_contract.id, &output)
                    .map_err(|error| EngineError::CanonicalRegistry(format!("{error:?}")))?;
                (output, None, content.to_owned())
            }
            ModelOutputMode::TypedJson => {
                let output: Value = match parse_typed_json_content(content) {
                    Ok(output) => output,
                    Err(error)
                        if constraint_mode != ProviderConstraintMode::JsonSchema
                            && state.reserve_repair()? =>
                    {
                        state.append_assistant(episode);
                        state.append_repair_feedback(
                            &output_contract.id,
                            answer_error_code(&EngineError::Json(error)),
                        );
                        state.check_conversation_limit(self.config.max_conversation_bytes)?;
                        return Ok(None);
                    }
                    Err(_error) if constraint_mode == ProviderConstraintMode::JsonSchema => {
                        return Err(EngineError::ProviderConstrainedOutputViolation(
                            "invalid JSON",
                        ));
                    }
                    Err(error) => return Err(EngineError::Json(error)),
                };
                if let Err(error) = validate_canonical_value(&output_contract.id, &output) {
                    if constraint_mode == ProviderConstraintMode::JsonSchema {
                        return Err(EngineError::ProviderConstrainedOutputViolation(
                            "canonical schema",
                        ));
                    }
                    if state.reserve_repair()? {
                        state.append_assistant(episode);
                        state.append_repair_feedback(
                            &output_contract.id,
                            answer_error_code(&EngineError::CanonicalRegistry(format!(
                                "{error:?}"
                            ))),
                        );
                        state.check_conversation_limit(self.config.max_conversation_bytes)?;
                        return Ok(None);
                    }
                    return Err(EngineError::CanonicalRegistry(format!("{error:?}")));
                }
                if let Err(error) =
                    validate_product_output_linkage(input.request, &output_contract, &output)
                {
                    if state.reserve_repair()? {
                        state.append_assistant(episode);
                        state
                            .append_repair_feedback(&output_contract.id, answer_error_code(&error));
                        state.check_conversation_limit(self.config.max_conversation_bytes)?;
                        return Ok(None);
                    }
                    return Err(error);
                }

                let candidate = validate_typed_output(input, state, &output_contract, &output);
                let answer_ir = match candidate {
                    Ok(answer_ir) => answer_ir,
                    Err(error) if state.apply_answer_repair(answer_error_code(&error))? => {
                        state.append_assistant(episode);
                        state
                            .append_repair_feedback(&output_contract.id, answer_error_code(&error));
                        state.check_conversation_limit(self.config.max_conversation_bytes)?;
                        return Ok(None);
                    }
                    Err(error) => return Err(error),
                };
                let rendered_content = render_typed_output(
                    &output_contract,
                    &output,
                    answer_ir.as_ref(),
                    &state.ledger,
                )?;
                // `bind_kernel_goal_ids` may normalize kernel-owned linkage
                // after the model response is parsed. Persist that normalized
                // AnswerIR as the canonical output too, otherwise the final
                // answer hash and the session-memory hash would disagree at
                // the durable commit boundary.
                let normalized_output = match &answer_ir {
                    Some(answer_ir) => serde_json::to_value(answer_ir)?,
                    // Product runs such as routing, notebook, and display
                    // planning intentionally do not produce AnswerIR. Keep
                    // their already-validated typed payload as the state
                    // artifact instead of serializing `None` to `null` and
                    // failing the product contract at the commit boundary.
                    None => output.clone(),
                };
                (normalized_output, answer_ir, rendered_content)
            }
            _ => {
                return Err(EngineError::WorkflowResolution {
                    outcome: "final output mode",
                });
            }
        };

        let verification_event = state.program.unique_transition_event(
            state.interpreter.current_state(),
            &output,
            |candidate| {
                matches!(
                    candidate.operation,
                    StateOperation::Builtin {
                        handler: BuiltinHandler::ValidateArtifact | BuiltinHandler::VerifyOutput,
                        ..
                    }
                )
            },
            "typed output verification",
        )?;
        state.apply_model_artifact(
            &verification_event,
            output_contract.clone(),
            &output,
            ContentHash::sha256(serde_jcs::to_vec(episode)?),
        )?;

        let verification_handler = match state.interpreter.current_operation()? {
            StateOperation::Builtin { handler, .. }
                if matches!(
                    handler,
                    BuiltinHandler::ValidateArtifact | BuiltinHandler::VerifyOutput
                ) =>
            {
                *handler
            }
            _ => {
                return Err(EngineError::WorkflowResolution {
                    outcome: "typed output verifier",
                });
            }
        };
        let render_event = state.program.unique_transition_event(
            state.interpreter.current_state(),
            &output,
            |candidate| {
                matches!(
                    candidate.operation,
                    StateOperation::Builtin {
                        handler: BuiltinHandler::RenderOutput,
                        ..
                    }
                )
            },
            "verified output",
        )?;
        state.apply_builtin_artifact(
            verification_handler,
            &render_event,
            output_contract.clone(),
            &output,
        )?;
        let commit_event = state.program.unique_transition_event(
            state.interpreter.current_state(),
            &output,
            |candidate| {
                matches!(
                    candidate.operation,
                    StateOperation::Builtin {
                        handler: BuiltinHandler::CommitOutput,
                        ..
                    }
                )
            },
            "rendered output",
        )?;
        state.apply_builtin_artifact(
            BuiltinHandler::RenderOutput,
            &commit_event,
            output_contract.clone(),
            &output,
        )?;
        self.guard_control(identity, deadline).await?;

        let execution = execution.transition(ExecutionEvent::BeginCommit)?;
        // Fold non-authoritative per-run timing counters into the final
        // usage projection exactly once. They never affect admission, budget,
        // or replay decisions.
        state.merge_runtime_timings();
        let evidence_ledger_hash = ContentHash::sha256(serde_jcs::to_vec(&state.ledger)?);
        let evidence_ids = state
            .ledger
            .iter()
            .map(|(evidence_id, _)| evidence_id.to_owned())
            .collect::<Vec<_>>();
        let visualizations = self.compile_visualizations(&state.presentation_packs, &state.ledger);
        let answer_bundle = AnswerBundle {
            schema_version: 4,
            output_contract: output_contract.clone(),
            output: output.clone(),
            evidence_ledger_hash,
            evidence_ids,
            answer_ir,
            rendered_content: rendered_content.clone(),
            rendered_markdown: rendered_content,
            visualizations,
            usage: state.usage.clone(),
            agent_image_hash: input.image.content_hash.clone(),
        };
        let bundle_value = serde_json::to_value(&answer_bundle)?;
        let bundle_bytes = serde_jcs::to_vec(&bundle_value)?;
        let answer_bundle_hash = ContentHash::sha256(&bundle_bytes);
        let final_output_hash = ContentHash::sha256(serde_jcs::to_vec(&answer_bundle.output)?);
        let rendered_message_hash = ContentHash::sha256(&answer_bundle.rendered_markdown);
        let final_commit_intent_hash = ContentHash::sha256(format!(
            "krw.final-commit-intent/v2\0{}\0{}",
            identity.run_id,
            answer_bundle_hash.as_str()
        ));
        let session_memory_artifacts = (answer_bundle.answer_ir.is_some()
            || final_output_mode == ModelOutputMode::Markdown)
            .then(|| {
                let (parent_frontier_hash, revision) =
                    session_memory_delta_lineage(state.session_memory.as_ref())?;
                let tickers = memory_tickers(
                    &input.request.context,
                    state.derived_ticker_scope.as_ref(),
                    &state.ledger,
                );
                let delta = match answer_bundle.answer_ir.as_ref() {
                    Some(answer_ir) => completed_turn_delta(CompletedTurnInputV3 {
                        session_id: &input.request.session_id,
                        parent_frontier_hash,
                        revision,
                        run_id: &identity.run_id,
                        final_commit_intent_hash: final_commit_intent_hash.clone(),
                        answer_bundle_hash: answer_bundle_hash.clone(),
                        user_content: &input.request.question,
                        rendered_answer: &answer_bundle.rendered_content,
                        answer_ir,
                        tickers: &tickers,
                        constraints: &[],
                        supersessions: &[],
                        resolved_goals: &[],
                    }),
                    None => completed_markdown_turn_delta(CompletedMarkdownTurnInputV3 {
                        session_id: &input.request.session_id,
                        parent_frontier_hash,
                        revision,
                        run_id: &identity.run_id,
                        final_commit_intent_hash: final_commit_intent_hash.clone(),
                        answer_bundle_hash: answer_bundle_hash.clone(),
                        final_output_hash: final_output_hash.clone(),
                        user_content: &input.request.question,
                        rendered_answer: &answer_bundle.rendered_content,
                        tickers: &tickers,
                        constraints: &[],
                        supersessions: &[],
                        resolved_goals: &[],
                    }),
                }
                .ok()?;
                let delta_hash = delta.content_hash().ok()?;
                let next_frontier_hash = delta.next_frontier_hash().ok()?;
                Some((delta, delta_hash, next_frontier_hash))
            })
            .flatten();
        let (session_memory_delta, session_memory_delta_hash, next_memory_frontier_hash) =
            match session_memory_artifacts {
                Some((delta, delta_hash, next_frontier_hash)) => {
                    (Some(delta), Some(delta_hash), Some(next_frontier_hash))
                }
                None => (None, None, None),
            };
        let commit_envelope_hash = ContentHash::sha256(serde_jcs::to_vec(&serde_json::json!({
            "schema_version": 2,
            "answer_bundle_hash": answer_bundle_hash,
            "session_memory_delta_hash": session_memory_delta_hash,
            "next_memory_frontier_hash": next_memory_frontier_hash,
        }))?);
        let durable_final = DurableFinal {
            mutation: FinalCommitMutation {
                run_id: identity.run_id.clone(),
                fencing_token: identity.fencing_token,
                expected_cancel_generation: identity.expected_cancel_generation,
                mutation_id: mutation_id("commit_final", &identity.run_id, &commit_envelope_hash),
                answer_bundle_hash: answer_bundle_hash.clone(),
                session_memory_delta_hash,
            },
            final_output_hash,
            rendered_message_hash,
            answer_bundle: bundle_value,
            usage: state.usage.clone(),
            session_memory_delta,
            next_memory_frontier_hash,
        };
        let final_status =
            match await_until(deadline, self.persistence.commit_final(&durable_final)).await {
                Ok(Ok(status)) => status,
                Ok(Err(failure)) if failure.delivery == DeliveryCertainty::MayHaveDispatched => {
                    return Err(EngineError::FinalCommitAmbiguous(answer_bundle_hash));
                }
                Ok(Err(failure)) => {
                    return Err(EngineError::Dependency {
                        component: "persistence.commit_final",
                        failure,
                    });
                }
                Err(()) => return Err(EngineError::FinalCommitAmbiguous(answer_bundle_hash)),
            };
        if final_status == FinalStatus::Cancelled {
            return Err(EngineError::Cancelled);
        }
        let terminal_event = state.program.unique_transition_event(
            state.interpreter.current_state(),
            &output,
            |candidate| matches!(candidate.operation, StateOperation::Terminal { .. }),
            "atomic final committed",
        )?;
        state.apply_builtin_artifact(
            BuiltinHandler::CommitOutput,
            &terminal_event,
            output_contract,
            &output,
        )?;
        if !matches!(
            state.interpreter.current_operation()?,
            StateOperation::Terminal {
                disposition: krw_agent_state_artifact::TerminalDisposition::Succeeded,
                ..
            }
        ) {
            return Err(EngineError::WorkflowTerminated("non-success"));
        }
        let execution = execution.transition(ExecutionEvent::CommitSucceeded)?;
        if !execution.is_terminal() {
            return Err(EngineError::Invariant("execution did not become terminal"));
        }
        Ok(Some(RunOutcome {
            answer_bundle,
            answer_bundle_hash,
            final_status,
            logical_action_keys: state.logical_action_keys.iter().cloned().collect(),
            evidence_count: state.ledger.len(),
        }))
    }

    /// Compile deterministic visualizations from the private presentation
    /// channel. Presentation is best-effort by construction: a data-poor or
    /// malformed pack records `presentation_omitted` (visible under
    /// KRW_DEBUG_PRESENTATION) and never fails the committed answer.
    fn compile_visualizations(
        &self,
        presentation_packs: &[Value],
        ledger: &EvidenceLedger,
    ) -> Vec<Value> {
        const MAX_TOTAL_ARTIFACTS: usize = 3;
        let mut artifacts = Vec::new();
        let mut fingerprints = BTreeSet::new();
        for pack in presentation_packs {
            match krw_presentation::compile(&pack) {
                Ok(compiled) => {
                    if compiled.is_empty()
                        && std::env::var("KRW_DEBUG_PRESENTATION").ok().as_deref() == Some("1")
                    {
                        eprintln!(
                            "[KRW_DEBUG_PRESENTATION] presentation_omitted: pack cannot support a chart"
                        );
                    }
                    for artifact in compiled {
                        let fingerprint = artifact
                            .get("semantic_fingerprint")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        if !fingerprint.is_empty() && fingerprints.insert(fingerprint.to_owned()) {
                            artifacts.push(artifact);
                        }
                        if artifacts.len() >= MAX_TOTAL_ARTIFACTS {
                            break;
                        }
                    }
                }
                Err(_) => {
                    if std::env::var("KRW_DEBUG_PRESENTATION").ok().as_deref() == Some("1") {
                        eprintln!(
                            "[KRW_DEBUG_PRESENTATION] presentation_omitted: pack failed structured validation"
                        );
                    }
                }
            }
            if artifacts.len() >= MAX_TOTAL_ARTIFACTS {
                artifacts.truncate(MAX_TOTAL_ARTIFACTS);
                break;
            }
        }
        let allowed_refs = current_presentation_evidence_refs(ledger);
        filter_grounded_visualizations(artifacts, &allowed_refs, MAX_TOTAL_ARTIFACTS)
    }
}
