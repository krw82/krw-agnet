//! Run orchestrator: the single main loop that claims a run, restores
//! recovery state, drives provider turns, dispatches capabilities, and
//! guards fencing/cancel boundaries; all policy lives in sibling modules.

use super::*;

impl<P, C, S> RunEngine<P, C, S>
where
    P: Provider,
    C: CapabilityRuntime,
    S: Persistence,
{
    pub fn run<'a>(
        &'a self,
        input: RunInput<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<RunOutcome, EngineError>> + Send + 'a>> {
        Box::pin(self.run_inner(input))
    }

    #[tracing::instrument(
        skip_all,
        fields(
            run_id = %input.request.run_id,
            tenant_id = %input.request.tenant_id,
        )
    )]
    async fn run_inner(&self, input: RunInput<'_>) -> Result<RunOutcome, EngineError> {
        validate_input(&input, &self.config)?;
        evaluate_admission_rules(input.image, input.request)?;
        let (program, context_planner) = if let Some(plan) = input.execution_plan.as_ref() {
            plan.validate_for(input.image, input.request)?;
            plan.parts()
        } else {
            (
                Arc::new(ProgramRuntime::compile(
                    &input.image.manifest,
                    input.request,
                )?),
                Arc::new(ContextPlanner::compile(input.image)?),
            )
        };
        let started = Instant::now();
        let budget_deadline = started
            .checked_add(Duration::from_millis(input.request.budget.deadline_ms))
            .ok_or(EngineError::InvalidInput("budget deadline overflow"))?;
        let deadline = input.hard_deadline.min(budget_deadline);
        ensure_before(deadline, "admission")?;

        let identity = RunIdentity {
            run_id: input.request.run_id.clone(),
            tenant_id: input.request.tenant_id.clone(),
            fencing_token: input.snapshot.fencing_token,
            expected_cancel_generation: input.snapshot.cancel_generation,
        };
        let recovery = dependency_call(
            deadline,
            "persistence.load_recovery",
            self.persistence.load_recovery(&identity),
        )
        .await?;
        self.guard_control(&identity, deadline).await?;

        let mut execution = ExecutionState::Queued;
        execution = execution.transition(ExecutionEvent::Claim)?;
        execution = execution.transition(ExecutionEvent::Admit)?;
        execution = execution.transition(ExecutionEvent::Start)?;

        let session_memory = prepare_session_memory(input.request)?;
        let mut state = ActiveRun::new(
            input.request.budget.clone(),
            program,
            context_planner,
            session_memory,
            input.runtime_timings.clone(),
        )?;
        state.enter_initial_model_state()?;
        let recovered_execution = self
            .restore_recovery(&input, &mut state, recovery, &identity)
            .await?;
        let recovered_pending = recovered_execution.pending;
        let child_receipt = recovered_execution.child;

        // Answer-always interception: when the run loop terminates with a
        // dependency/budget-class escape (provider/MCP outage that survived
        // the bounded deadline, exhausted capability/output budget), the
        // already-admitted ledger still owes the user a deterministic
        // `UnavailableButAnswerable` final instead of `run.failed`. Integrity,
        // cancellation, fencing, and ambiguous commits keep failing. The
        // interception sits where the frontier/ledger state is final and no
        // new provider turn is needed.
        let outcome = match self
            .drive_run(
                &input,
                &identity,
                &mut state,
                execution,
                recovered_pending,
                child_receipt,
                deadline,
            )
            .await
        {
            Ok(outcome) => outcome,
            // A RETRYABLE dependency (cold MCP pool right after a stack
            // restart, production 2026-09-02: 2 of 10 EN runs escaped to the
            // deterministic fallback 37s in with zero episodes) must not be
            // swallowed by the answer-always catch: propagate it so the
            // executor's deferral lane can re-lease and retry. Only
            // non-retryable/capacity-class errors fall back immediately.
            Err(error) if matches!(&error, EngineError::Dependency { failure, .. } if failure.retryable) =>
            {
                return Err(error);
            }
            Err(error) if error_allows_ledger_fallback(&error) => {
                self.commit_ledger_fallback(&input, &identity, &mut state, error, deadline)
                    .await?
            }
            Err(error) => return Err(error),
        };
        Ok(outcome)
    }

    /// The single main run loop. It exits only with a committed outcome or a
    /// terminal error; the caller owns the answer-always fallback decision.
    #[allow(clippy::too_many_arguments)]
    async fn drive_run(
        &self,
        input: &RunInput<'_>,
        identity: &RunIdentity,
        state: &mut ActiveRun,
        mut execution: ExecutionState,
        mut recovered_pending: Option<RecoveredPendingEpisode>,
        mut child_receipt: Option<ChildExecutionReceipt>,
        deadline: Instant,
    ) -> Result<RunOutcome, EngineError> {
        loop {
            self.guard_control(&identity, deadline).await?;
            if state.finalize_for_output_reserve_before_next_turn(input.image)? {
                self.checkpoint_active_state(&identity, &state, deadline)
                    .await?;
                continue;
            }
            // The ordinary output-reserve path deliberately waits for
            // substantive filing evidence. That is normally right, but it
            // must not leave a thinking-enabled analyst with fewer tokens
            // than the provider can legally accept. When this exact physical
            // boundary is reached, take the image-declared composition edge
            // even if the preceding lookup only produced orientation or
            // partial evidence. The composer receives the bounded limitation
            // context and can still return an honest user-facing answer.
            if state.finalize_for_thinking_floor_before_next_turn(input)? {
                self.checkpoint_active_state(&identity, &state, deadline)
                    .await?;
                continue;
            }
            // Kernel-owned supplemental batching drain: the model batched
            // several independent supplemental reads into one committed
            // episode; the statechart visits one capability per decision, so
            // the kernel replays the queued remainder here — each through the
            // same route/dispatch/ingest path a solo model decision takes,
            // with zero extra provider turns (production 2026-09-01: rejected
            // 3-call and 6-call batches burned a whole GOOGL run).
            while let Some((queued_hash, call)) = state.take_pending_supplemental()? {
                self.guard_control(&identity, deadline).await?;
                if let Some(cached) = state.cached_action_result(&call) {
                    state.complete_cached_capability(
                        input.image,
                        &call,
                        &cached,
                        queued_hash,
                        self.config.max_compacted_context_bytes,
                        true,
                    )?;
                    state.check_conversation_limit(self.config.max_conversation_bytes)?;
                    continue;
                }
                if state
                    .preflight_capability_calls(input.image, std::slice::from_ref(&call))
                    .is_err()
                {
                    // A queued call that no longer passes its pre-action
                    // rules is dropped, not retried: the model sees the
                    // outcome through the next observation either way.
                    continue;
                }
                if state
                    .route_to_capability(&call, queued_hash.clone())
                    .is_err()
                {
                    // The workflow moved somewhere the remaining reads
                    // cannot follow; stop draining.
                    state.pending_supplemental_calls.clear();
                    break;
                }
                state.reserve_capability_call(&call.capability.id, &call.action_key)?;
                let action_context = ActionExecutionContext {
                    identity: &identity,
                    episode_hash: &queued_hash,
                    image: input.image,
                    state: &state,
                    deadline,
                    max_result_bytes: self.config.max_capability_result_bytes,
                };
                let cap_t0 = Instant::now();
                let result = self.execute_action(&action_context, &call).await?;
                state.record_capability_duration_ms(elapsed_millis(cap_t0));
                let invocation = capability_invocation(&identity.run_id, &call);
                self.capabilities
                    .restore_committed_result(&invocation, &result)
                    .await
                    .map_err(|failure| EngineError::Dependency {
                        component: "capability.restore_committed_result",
                        failure,
                    })?;
                if state.commit_research_result(&call, &result).is_err() {
                    continue;
                }
                let result_bytes = serde_jcs::to_vec(&result)?;
                state.record_evidence_bytes(result_bytes.len())?;
                let symbol = call
                    .arguments
                    .get("symbol")
                    .and_then(serde_json::Value::as_str);
                state.ingest(&result, call.capability.id.as_str(), symbol)?;
                state.ingest_scope_projection(&call, &result)?;
                state.append_capability_tool_result(&call, &result)?;
                if capability_result_completes_prerequisite(&result) {
                    state
                        .completed_capabilities
                        .insert(call.capability.id.clone());
                }
                state.record_accepted_action(&call)?;
                if capability_result_cacheable(
                    call.capability
                        .research_action
                        .as_ref()
                        .map(|policy| policy.kind),
                    &result,
                ) {
                    state
                        .action_cache
                        .insert(call.action_key.clone(), result.clone());
                }
                state.complete_capability(
                    input.image,
                    &call,
                    &result,
                    accepted_action_receipt_hash(&call, &result)?,
                )?;
                if capability_result_completes_prerequisite(&result) {
                    state.compact_settled_phase(
                        queued_hash.clone(),
                        self.config.max_compacted_context_bytes,
                    )?;
                }
                state.check_conversation_limit(self.config.max_conversation_bytes)?;
                self.checkpoint_active_state(&identity, &state, deadline)
                    .await?;
            }

            let child_policy = bounded_child::current_policy(input.image, &state)?;
            if child_policy.is_none() {
                // Do not carry image-owned child skill bodies into the parent
                // or a later ordinary phase.
                state.clear_child_skill_context();
            }
            let child_inputs = child_policy
                .as_ref()
                .map(|policy| bounded_child::prepare_inputs(policy, &state))
                .transpose()?;
            if let (Some(policy), Some(inputs)) = (&child_policy, &child_inputs) {
                if child_receipt.is_none() {
                    let mutation =
                        bounded_child::reserve_mutation(&identity, policy, &state, inputs)?;
                    child_receipt = Some(
                        dependency_call(
                            deadline,
                            "persistence.reserve_child",
                            self.persistence.reserve_child(&mutation),
                        )
                        .await?,
                    );
                }
                let receipt = child_receipt.as_ref().ok_or(EngineError::Invariant(
                    "bounded child reservation disappeared",
                ))?;
                bounded_child::validate_recovered_receipt(input.image, &identity, receipt)?;
                bounded_child::ensure_can_continue(receipt, recovered_pending.is_some())?;
            }
            state.reserve_provider_turn(input.image)?;
            let turn_span = tracing::info_span!("turn", turn = state.usage.provider_turns);
            let prompt_t0 = Instant::now();
            let built = turn_span.in_scope(|| -> Result<_, EngineError> {
                tracing::debug!("provider turn began");
                let messages = if child_policy.is_some() {
                    Vec::new()
                } else {
                    std::mem::take(&mut state.messages)
                };
                let mut built = build_provider_request(input, &state, &self.config, messages)?;
                if let (Some(policy), Some(inputs), Some(receipt)) =
                    (&child_policy, &child_inputs, child_receipt.as_ref())
                {
                    let usage = bounded_child::usage(receipt, &state)?;
                    bounded_child::isolate_request(
                        &mut built,
                        input.image,
                        &state,
                        policy,
                        inputs,
                        &state.child_skill_context,
                        &usage,
                    )?;
                }
                state.record_prompt_assembly(
                    built.tool_definitions.clone(),
                    built.episode_context.tool_schema_hash.clone(),
                    built.prompt_receipt_hash.clone(),
                )?;
                Ok(built)
            })?;
            if let Some(timings) = input.runtime_timings.as_ref() {
                timings.add_prompt_build(prompt_t0.elapsed());
            }
            let _ = turn_span;
            let constraint_mode = built.constraint_mode;
            let episode_context = built.episode_context;
            let request = built.request;
            let prepared_request = PreparedMessagesRequest::new(&request)?;
            let request_hash = prepared_request.request_hash().clone();
            if let (Some(inputs), Some(receipt)) = (&child_inputs, child_receipt.as_ref())
                && receipt.stage == krw_agent_bounded_child::ChildStage::Reserved
            {
                let mutation = bounded_child::invoke_mutation(
                    &identity,
                    receipt,
                    &request_hash,
                    &inputs.set_hash,
                );
                child_receipt = Some(
                    dependency_call(
                        deadline,
                        "persistence.invoke_child",
                        self.persistence.invoke_child(&mutation),
                    )
                    .await?,
                );
            }
            let recovered = recovered_pending.take();
            let provider_result = if let Some(recovered) = &recovered {
                if ContentHash::sha256(&recovered.episode.episode_bytes)
                    != recovered.episode.episode_hash
                {
                    return Err(EngineError::RecoveryArtifactMismatch("episode hash"));
                }
                serde_json::from_slice(&recovered.episode.episode_bytes).map_err(EngineError::from)
            } else {
                let provider_t0 = Instant::now();
                let provider_outcome = dependency_call(
                    deadline,
                    "provider",
                    self.provider
                        .complete_prepared(&prepared_request, &episode_context),
                )
                .await;
                let provider_ms = elapsed_millis(provider_t0);
                state.record_provider_duration_ms(provider_ms);
                krw_agent_persistence::metrics::record_provider_turn_duration_seconds(provider_ms);
                provider_outcome
            };
            if request.messages.len() < WIRE_TRUSTED_PREFIX_MESSAGE_COUNT {
                return Err(EngineError::Invariant("trusted prompt prefix disappeared"));
            }
            if child_policy.is_none() {
                state.messages = built.transcript;
            } else if !request.messages[WIRE_TRUSTED_PREFIX_MESSAGE_COUNT..].is_empty() {
                return Err(EngineError::Invariant(
                    "bounded child provider request retained a transcript",
                ));
            }
            let episode = match provider_result {
                Ok(episode) => episode,
                Err(error) => {
                    let Some(directive) = model_recovery_directive(&error) else {
                        return Err(error);
                    };
                    if recovered
                        .as_ref()
                        .is_some_and(|pending| pending.has_action_receipt)
                    {
                        return Err(EngineError::RecoveryArtifactMismatch(
                            "recovered provider failure has an action receipt",
                        ));
                    }
                    if !state.recover_provider_decision(input.image, directive)? {
                        return Err(error);
                    }
                    state.check_conversation_limit(self.config.max_conversation_bytes)?;
                    self.checkpoint_active_state(&identity, &state, deadline)
                        .await?;
                    continue;
                }
            };
            self.guard_control(&identity, deadline).await?;
            validate_episode(
                &episode,
                &request_hash,
                input.image,
                &state.tool_schema_hash,
                &input.snapshot.resolved_model,
                &input.snapshot.provider_api_version,
                input.snapshot.provider_wire_capabilities,
                request.thinking.kind,
            )?;
            state.record_provider_usage(&episode)?;
            if let Some(receipt) = child_receipt.as_ref()
                && child_policy.is_some()
            {
                bounded_child::usage(receipt, &state)?;
            }

            let episode_bytes = serde_jcs::to_vec(&episode)?;
            ensure_size(
                episode_bytes.len(),
                self.config.max_episode_bytes,
                "provider_episode",
            )?;
            let episode_hash = ContentHash::sha256(&episode_bytes);
            if let Some(recovered) = &recovered {
                if episode_hash != recovered.episode.episode_hash {
                    return Err(EngineError::RecoveryArtifactMismatch("episode receipt"));
                }
            } else {
                self.guard_control(&identity, deadline).await?;
                let durable_episode = DurableEpisode {
                    mutation: CheckpointEpisodeMutation {
                        run_id: identity.run_id.clone(),
                        fencing_token: identity.fencing_token,
                        mutation_id: mutation_id("episode", &identity.run_id, &episode_hash),
                        episode_hash: episode_hash.clone(),
                    },
                    episode_bytes,
                };
                dependency_call(
                    deadline,
                    "persistence.checkpoint_episode",
                    self.persistence.checkpoint_episode(&durable_episode),
                )
                .await?;
            }
            state.record_provider_episode_hash(episode_hash.clone());

            let output =
                match classify_provider_output(state.current_model_output_mode()?, &episode) {
                    Ok(output) => output,
                    Err(error) => {
                        let Some(directive) = model_recovery_directive(&error) else {
                            return Err(error);
                        };
                        if recovered
                            .as_ref()
                            .is_some_and(|pending| pending.has_action_receipt)
                        {
                            return Err(EngineError::RecoveryArtifactMismatch(
                                "recovered decision rejection has an action receipt",
                            ));
                        }
                        if !state.recover_model_decision(input.image, &episode, directive)? {
                            return Err(error);
                        }
                        state.check_conversation_limit(self.config.max_conversation_bytes)?;
                        self.checkpoint_active_state(&identity, &state, deadline)
                            .await?;
                        continue;
                    }
                };

            match output {
                ProviderOutputDisposition::TypedJson | ProviderOutputDisposition::Markdown => {
                    execution = execution.transition(ExecutionEvent::BeginVerification)?;
                    let outcome = match self
                        .finish(
                            input,
                            &identity,
                            &episode,
                            &mut *state,
                            execution,
                            constraint_mode,
                            deadline,
                        )
                        .await
                    {
                        Ok(outcome) => outcome,
                        // Finalization-stage failures (answer validation,
                        // storage commit, workflow edges around the final)
                        // keep their own semantics: the composed answer is
                        // either committed, ambiguous, or retried by the
                        // outer executor. A deterministic ledger notice must
                        // never replace an already-composed final.
                        Err(error) => return Err(error),
                    };
                    if let Some(outcome) = outcome {
                        return Ok(outcome);
                    }
                    self.checkpoint_active_state(&identity, &state, deadline)
                        .await?;
                    execution = execution.transition(ExecutionEvent::Retry)?;
                    execution = execution.transition(ExecutionEvent::Resume)?;
                    continue;
                }
                ProviderOutputDisposition::WorkflowTransition => {
                    let transition = match parse_workflow_transition_call(
                        &episode,
                        self.config.max_model_output_bytes,
                    ) {
                        Ok(transition) => transition,
                        Err(error) => {
                            let Some(directive) = model_recovery_directive(&error) else {
                                return Err(error);
                            };
                            if recovered
                                .as_ref()
                                .is_some_and(|pending| pending.has_action_receipt)
                            {
                                return Err(EngineError::RecoveryArtifactMismatch(
                                    "recovered transition rejection has an action receipt",
                                ));
                            }
                            if !state.recover_model_decision(input.image, &episode, directive)? {
                                return Err(error);
                            }
                            state.check_conversation_limit(self.config.max_conversation_bytes)?;
                            self.checkpoint_active_state(&identity, &state, deadline)
                                .await?;
                            continue;
                        }
                    };
                    let facts =
                        kernel_workflow_facts(input.image, input.request, &transition.event)?;
                    if !state.event_has_remaining_target(&transition.event, &facts)? {
                        if recovered
                            .as_ref()
                            .is_some_and(|pending| pending.has_action_receipt)
                        {
                            return Err(EngineError::RecoveryArtifactMismatch(
                                "recovered transition rejection has an action receipt",
                            ));
                        }
                        let error = EngineError::InvalidWorkflowControl;
                        let Some(directive) = model_recovery_directive(&error) else {
                            return Err(error);
                        };
                        if !state.recover_model_decision(input.image, &episode, directive)? {
                            return Err(error);
                        }
                        state.check_conversation_limit(self.config.max_conversation_bytes)?;
                        self.checkpoint_active_state(&identity, &state, deadline)
                            .await?;
                        continue;
                    }
                    // A bounded child owns no parent transcript. Its typed
                    // transition is durable, while an ordinary run receives a
                    // local acknowledgement so the provider's tool-call chain is
                    // complete before the next provider turn.
                    state.handle_model_event(&transition.event, &facts, episode_hash.clone())?;
                    // Release B: judgment notes ride only the final
                    // evidence-sufficient handoff toward composition; a note
                    // on any mid-research transition is dropped.
                    let judgment = transition.judgment.clone();
                    state.capture_analyst_judgment(&transition.event, judgment);
                    if child_policy.is_none() {
                        state.append_workflow_transition_result(&episode, &transition)?;
                        state.compact_settled_phase(
                            episode_hash.clone(),
                            self.config.max_compacted_context_bytes,
                        )?;
                    }
                    state.check_conversation_limit(self.config.max_conversation_bytes)?;
                    self.checkpoint_active_state(&identity, &state, deadline)
                        .await?;
                    continue;
                }
                ProviderOutputDisposition::Capability => {}
            }

            match resolve_local_skill_load(&episode, &state.tool_definitions, input.image) {
                Ok(Some(resolution)) => {
                    apply_local_skill_load_resolution(
                        &mut *state,
                        &episode,
                        &resolution,
                        child_policy.is_some(),
                    )?;
                    state.check_conversation_limit(self.config.max_conversation_bytes)?;
                    self.checkpoint_active_state(&identity, &state, deadline)
                        .await?;
                    continue;
                }
                Ok(None) => {}
                Err(error) => {
                    let Some(directive) = model_recovery_directive(&error) else {
                        return Err(error);
                    };
                    if !state.recover_model_decision(input.image, &episode, directive)? {
                        return Err(error);
                    }
                    state.check_conversation_limit(self.config.max_conversation_bytes)?;
                    self.checkpoint_active_state(&identity, &state, deadline)
                        .await?;
                    continue;
                }
            }

            let prepared = match prepare_calls(
                &episode,
                input,
                &state,
                self.config.max_tool_calls_per_episode,
                self.config.contract_guard.as_ref(),
            ) {
                Ok(prepared) => prepared,
                Err(error) => {
                    if child_policy.is_none()
                        && is_append_context_plan_capacity_rejection(&error)
                        && state.stop_at_append_context_plan_capacity(
                            input.image,
                            &episode,
                            episode_hash.clone(),
                        )?
                    {
                        state.check_conversation_limit(self.config.max_conversation_bytes)?;
                        self.checkpoint_active_state(&identity, &state, deadline)
                            .await?;
                        continue;
                    }
                    let Some(directive) = model_recovery_directive(&error) else {
                        return Err(error);
                    };
                    if recovered
                        .as_ref()
                        .is_some_and(|pending| pending.has_action_receipt)
                    {
                        return Err(EngineError::RecoveryArtifactMismatch(
                            "rejected model proposal episode has an action receipt",
                        ));
                    }
                    if !state.recover_model_decision(input.image, &episode, directive)? {
                        return Err(error);
                    }
                    state.check_conversation_limit(self.config.max_conversation_bytes)?;
                    self.checkpoint_active_state(&identity, &state, deadline)
                        .await?;
                    continue;
                }
            };
            let mut prepared = prepared;
            // A bounded child deliberately has no parent transcript in which
            // to acknowledge unselected alternatives. Keep its contract
            // single-action even though an ordinary research assessment may
            // offer a value-ranked decision set.
            if child_policy.is_some() && prepared.len() != 1 {
                let error = EngineError::WorkflowResolution {
                    outcome: "exactly one bounded child capability action",
                };
                // GLM can legally emit parallel tool_use blocks for an
                // ordinary assessment. A bounded child has no parent
                // transcript in which to acknowledge the unselected blocks,
                // so turn this recoverable provider decision into a repair
                // turn instead of failing the whole research run.
                let Some(directive) = model_recovery_directive(&error) else {
                    return Err(error);
                };
                if recovered
                    .as_ref()
                    .is_some_and(|pending| pending.has_action_receipt)
                {
                    return Err(EngineError::RecoveryArtifactMismatch(
                        "recovered bounded child batch has an action receipt",
                    ));
                }
                if !state.recover_model_decision(input.image, &episode, directive)? {
                    return Err(error);
                }
                state.check_conversation_limit(self.config.max_conversation_bytes)?;
                self.checkpoint_active_state(&identity, &state, deadline)
                    .await?;
                continue;
            }
            let question_only = matches!(
                input.request.context,
                krw_agent_protocol::RunContextV1::QuestionOnly {}
            );
            let decision = match state.decide_research_dispatch(&prepared, question_only) {
                Ok(decision) => decision,
                Err(error) => {
                    let Some(directive) = model_recovery_directive(&error) else {
                        return Err(error);
                    };
                    if recovered
                        .as_ref()
                        .is_some_and(|pending| pending.has_action_receipt)
                    {
                        return Err(EngineError::RecoveryArtifactMismatch(
                            "rejected planner decision has an action receipt",
                        ));
                    }
                    if !state.recover_model_decision(input.image, &episode, directive)? {
                        return Err(error);
                    }
                    state.check_conversation_limit(self.config.max_conversation_bytes)?;
                    self.checkpoint_active_state(&identity, &state, deadline)
                        .await?;
                    continue;
                }
            };
            let selected_index = match decision {
                ResearchDispatchDecision::Execute { selected_index } => selected_index,
                ResearchDispatchDecision::NoPositiveValue(reason) => {
                    if recovered
                        .as_ref()
                        .is_some_and(|pending| pending.has_action_receipt)
                    {
                        return Err(EngineError::RecoveryArtifactMismatch(
                            "no-positive episode has an action receipt",
                        ));
                    }
                    if child_policy.is_some() {
                        let parent_messages = std::mem::take(&mut state.messages);
                        state.append_no_positive_value_result(&episode, &prepared, reason)?;
                        state.messages = parent_messages;
                    } else {
                        state.append_no_positive_value_result(&episode, &prepared, reason)?;
                    }
                    state.check_conversation_limit(self.config.max_conversation_bytes)?;
                    self.checkpoint_active_state(&identity, &state, deadline)
                        .await?;
                    continue;
                }
                ResearchDispatchDecision::ProposalRejected(reason) => {
                    if recovered
                        .as_ref()
                        .is_some_and(|pending| pending.has_action_receipt)
                    {
                        return Err(EngineError::RecoveryArtifactMismatch(
                            "rejected proposal episode has an action receipt",
                        ));
                    }
                    if child_policy.is_some() {
                        let parent_messages = std::mem::take(&mut state.messages);
                        state.append_proposal_rejected_result(&episode, &prepared, reason)?;
                        state.messages = parent_messages;
                    } else {
                        state.append_proposal_rejected_result(&episode, &prepared, reason)?;
                    }
                    state.check_conversation_limit(self.config.max_conversation_bytes)?;
                    self.checkpoint_active_state(&identity, &state, deadline)
                        .await?;
                    continue;
                }
            };
            if selected_index >= prepared.len() {
                return Err(EngineError::ResearchPlannerDecisionMismatch);
            }
            let selected = prepared.remove(selected_index);
            let child_call_kind = child_policy
                .as_ref()
                .map(|policy| bounded_child::authorize_call(policy, &state, &selected))
                .transpose()?;
            // A pre-action rule violation (state-order discipline, for
            // example the news ladder) is a correctable model mistake: route
            // it through the same recovery directive the capability route
            // uses instead of failing the run terminally.
            if let Err(error) =
                state.preflight_capability_calls(input.image, std::slice::from_ref(&selected))
            {
                let Some(directive) = model_recovery_directive(&error) else {
                    return Err(error);
                };
                if recovered
                    .as_ref()
                    .is_some_and(|pending| pending.has_action_receipt)
                {
                    return Err(EngineError::RecoveryArtifactMismatch(
                        "rejected capability preflight has an action receipt",
                    ));
                }
                if !state.recover_model_decision(input.image, &episode, directive)? {
                    return Err(error);
                }
                state.check_conversation_limit(self.config.max_conversation_bytes)?;
                self.checkpoint_active_state(&identity, &state, deadline)
                    .await?;
                continue;
            }
            let pending_child_completion =
                if child_call_kind == Some(bounded_child::ChildCallKind::TypedReturn) {
                    let receipt = child_receipt.as_ref().ok_or(EngineError::Invariant(
                        "typed child return lacks a durable reservation",
                    ))?;
                    Some(bounded_child::complete_mutation(
                        &identity, receipt, &state, &selected,
                    )?)
                } else {
                    None
                };
            if let Err(error) = state.route_to_capability(&selected, episode_hash.clone()) {
                let Some(directive) = model_recovery_directive(&error) else {
                    return Err(error);
                };
                if recovered
                    .as_ref()
                    .is_some_and(|pending| pending.has_action_receipt)
                {
                    return Err(EngineError::RecoveryArtifactMismatch(
                        "rejected capability route has an action receipt",
                    ));
                }
                if !state.recover_model_decision(input.image, &episode, directive)? {
                    return Err(error);
                }
                state.check_conversation_limit(self.config.max_conversation_bytes)?;
                self.checkpoint_active_state(&identity, &state, deadline)
                    .await?;
                continue;
            }
            // Child tool/reasoning pairs remain only in durable child episode
            // receipts; the parent provider chain is never appended to.
            if child_policy.is_none() {
                state.append_assistant(&episode);
                state.append_unselected_research_results(&prepared)?;
            }
            state.check_conversation_limit(self.config.max_conversation_bytes)?;

            for call in [selected] {
                self.guard_control(&identity, deadline).await?;
                if let Some(cached) = state.cached_action_result(&call) {
                    state.complete_cached_capability(
                        input.image,
                        &call,
                        &cached,
                        episode_hash.clone(),
                        self.config.max_compacted_context_bytes,
                        child_policy.is_none(),
                    )?;
                    if let Some(mutation) = pending_child_completion.as_ref()
                        && bounded_child::return_was_accepted(
                            input.image,
                            &state,
                            child_policy.as_ref().ok_or(EngineError::Invariant(
                                "typed child return lost its child policy",
                            ))?,
                        )?
                    {
                        child_receipt = Some(
                            dependency_call(
                                deadline,
                                "persistence.complete_child",
                                self.persistence.complete_child(mutation),
                            )
                            .await?,
                        );
                    }
                    state.check_conversation_limit(self.config.max_conversation_bytes)?;
                    continue;
                }

                state.reserve_capability_call(&call.capability.id, &call.action_key)?;
                if child_call_kind == Some(bounded_child::ChildCallKind::Capability) {
                    let receipt = child_receipt.as_ref().ok_or(EngineError::Invariant(
                        "child capability call lacks a durable reservation",
                    ))?;
                    bounded_child::usage(receipt, &state)?;
                }
                let action_context = ActionExecutionContext {
                    identity: &identity,
                    episode_hash: &episode_hash,
                    image: input.image,
                    state: &state,
                    deadline,
                    max_result_bytes: self.config.max_capability_result_bytes,
                };
                let cap_t0 = Instant::now();
                let result = self.execute_action(&action_context, &call).await?;
                // On success only: charge the wall-clock capability duration
                // against the run budget. Error/timeout paths are observed by
                // the histogram inside `execute_action` but intentionally not
                // accumulated, since a failed dispatch does not consume a
                // successful turn.
                state.record_capability_duration_ms(elapsed_millis(cap_t0));
                let invocation = capability_invocation(&identity.run_id, &call);
                self.capabilities
                    .restore_committed_result(&invocation, &result)
                    .await
                    .map_err(|failure| EngineError::Dependency {
                        component: "capability.restore_committed_result",
                        failure,
                    })?;
                if let Err(error) = state.commit_research_result(&call, &result) {
                    let Some(directive) = model_recovery_directive(&error) else {
                        return Err(error);
                    };
                    if !state.recover_model_decision(input.image, &episode, directive)? {
                        return Err(error);
                    }
                    continue;
                }
                let result_bytes = serde_jcs::to_vec(&result)?;
                state.record_evidence_bytes(result_bytes.len())?;
                let symbol = call
                    .arguments
                    .get("symbol")
                    .and_then(serde_json::Value::as_str);
                state.ingest(&result, call.capability.id.as_str(), symbol)?;
                state.ingest_scope_projection(&call, &result)?;
                if child_policy.is_none() {
                    state.append_capability_tool_result(&call, &result)?;
                }
                if capability_result_completes_prerequisite(&result) {
                    state
                        .completed_capabilities
                        .insert(call.capability.id.clone());
                }
                state.record_accepted_action(&call)?;
                if capability_result_cacheable(
                    call.capability
                        .research_action
                        .as_ref()
                        .map(|policy| policy.kind),
                    &result,
                ) {
                    state
                        .action_cache
                        .insert(call.action_key.clone(), result.clone());
                }
                state.complete_capability(
                    input.image,
                    &call,
                    &result,
                    accepted_action_receipt_hash(&call, &result)?,
                )?;
                if child_policy.is_none() && capability_result_completes_prerequisite(&result) {
                    // See the cached-result branch above. A compaction receipt
                    // binds the new fresh conversation to the exact episode,
                    // committed action, validated state, and EvidenceLedger.
                    state.compact_settled_phase(
                        episode_hash.clone(),
                        self.config.max_compacted_context_bytes,
                    )?;
                }
                if let Some(mutation) = pending_child_completion.as_ref()
                    && bounded_child::return_was_accepted(
                        input.image,
                        &state,
                        child_policy.as_ref().ok_or(EngineError::Invariant(
                            "typed child return lost its child policy",
                        ))?,
                    )?
                {
                    child_receipt = Some(
                        dependency_call(
                            deadline,
                            "persistence.complete_child",
                            self.persistence.complete_child(mutation),
                        )
                        .await?,
                    );
                }
                state.check_conversation_limit(self.config.max_conversation_bytes)?;
            }
            self.checkpoint_active_state(&identity, &state, deadline)
                .await?;
        }
    }

    pub(crate) async fn guard_control(
        &self,
        identity: &RunIdentity,
        deadline: Instant,
    ) -> Result<(), EngineError> {
        let control = dependency_call(
            deadline,
            "persistence.inspect_run",
            self.persistence.inspect_run(identity),
        )
        .await?;
        match control {
            RunControl::Active {
                fencing_token,
                cancel_generation: _,
            } if fencing_token != identity.fencing_token => Err(EngineError::StaleFence {
                expected: identity.fencing_token,
                observed: fencing_token,
            }),
            RunControl::Active {
                cancel_generation, ..
            } if cancel_generation != identity.expected_cancel_generation => {
                Err(EngineError::Cancelled)
            }
            RunControl::Active { .. } => Ok(()),
            RunControl::Cancelled => Err(EngineError::Cancelled),
            RunControl::Finalized => Err(EngineError::AlreadyFinalized),
        }
    }
}
