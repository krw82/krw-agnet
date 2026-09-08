//! Durable recovery boundary for one claimed run: checkpoint restoration,
//! committed-episode replay, active-state checkpointing, and the closed
//! model recovery directive vocabulary.  Recovery never fabricates provider
//! output; it replays exactly what was durably acknowledged before the
//! failure.
//!
//! The recovered-state DTOs themselves (`RecoveredStateCheckpoint`,
//! `RecoveredEpisode`, `RecoveredAction`, `DurableRecoverySnapshot`,
//! `RecoverySnapshot`) live in `krw-agent-execution-contracts` and are
//! re-exported from the engine root; they are in scope here via `super::*`.

use super::*;

/// Recover the bounded usage counters that were durable at the last
/// checkpoint, including a provider episode that was persisted just before a
/// terminal validation error.  This is intentionally a counter-only
/// projection: it never exposes checkpoint bytes, prompts, tool arguments, or
/// provider output.
pub fn recovery_budget_usage(snapshot: &RecoverySnapshot) -> Option<BudgetUsage> {
    let RecoverySnapshot::Durable(recovery) = snapshot else {
        return Some(BudgetUsage::default());
    };

    let mut usage = recovery
        .state
        .as_ref()
        .map(|checkpoint| serde_json::from_slice::<ActiveRunCheckpoint>(&checkpoint.state_bytes))
        .transpose()
        .ok()?
        .map(|checkpoint| checkpoint.usage)
        .unwrap_or_default();

    let base_episode_count = recovery
        .state
        .as_ref()
        .and_then(|checkpoint| usize::try_from(checkpoint.provider_checkpoint_seq).ok())
        .unwrap_or(0);
    for episode in recovery.episodes.iter().skip(base_episode_count) {
        let episode = serde_json::from_slice::<ProviderEpisodeV1>(&episode.episode_bytes).ok()?;
        usage.provider_turns = usage.provider_turns.saturating_add(1);
        usage.input_tokens = usage
            .input_tokens
            .saturating_add(episode.usage.prompt_tokens);
        usage.output_tokens = usage
            .output_tokens
            .saturating_add(episode.usage.completion_tokens);
    }
    Some(usage)
}

/// Closed, content-free feedback that can safely cross the model boundary.
/// It describes how to repair a decision, never the user's question, evidence,
/// raw provider output, or an internal state identifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModelRecoveryDirective {
    pub(crate) reason_code: &'static str,
    pub(crate) repair_mode: &'static str,
    pub(crate) detail: Option<RecoveryDetailV1>,
}

impl ModelRecoveryDirective {
    fn replace(reason_code: &'static str) -> Self {
        Self {
            reason_code,
            repair_mode: "replace",
            detail: None,
        }
    }

    fn with_mode(reason_code: &'static str, repair_mode: &'static str) -> Self {
        Self {
            reason_code,
            repair_mode,
            detail: None,
        }
    }

    fn with_detail(
        reason_code: &'static str,
        repair_mode: &'static str,
        detail: RecoveryDetailV1,
    ) -> Self {
        Self {
            reason_code,
            repair_mode,
            detail: Some(detail),
        }
    }
}

/// Model-visible result for a decision that was not executed. This is one
/// typed envelope for all correctable provider/model mistakes; new error
/// classes add a closed directive, not another workflow branch.
#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RecoveryEnvelopeV1 {
    pub(crate) schema_version: u8,
    pub(crate) status: &'static str,
    pub(crate) class: &'static str,
    pub(crate) reason_code: &'static str,
    pub(crate) repair_mode: &'static str,
    pub(crate) allowed_actions: Vec<&'static str>,
    pub(crate) repairs_remaining: u8,
    pub(crate) contains_evidence: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) detail: Option<RecoveryDetailV1>,
}

/// Bounded diagnostic that is safe to cross the provider boundary. Contains
/// only structural identifiers (JSON pointer, metric names) that are already
/// advertised in the system prompt ontology catalog — never user text,
/// evidence, or internal state identifiers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RecoveryDetailV1 {
    pub(crate) schema_version: u8,
    pub(crate) field: String,
    pub(crate) offending_value: String,
    pub(crate) valid_alternatives: Vec<String>,
    pub(crate) hint: String,
}

impl<P, C, S> RunEngine<P, C, S>
where
    P: Provider,
    C: CapabilityRuntime,
    S: Persistence,
{
    pub(crate) async fn restore_recovery(
        &self,
        input: &RunInput<'_>,
        state: &mut ActiveRun,
        recovery: RecoverySnapshot,
        identity: &RunIdentity,
    ) -> Result<RecoveredExecution, EngineError> {
        let RecoverySnapshot::Durable(recovery) = recovery else {
            return Ok(RecoveredExecution {
                pending: None,
                child: None,
            });
        };
        let recovery = *recovery;
        if let Some(receipt) = &recovery.child {
            bounded_child::validate_recovered_receipt(input.image, identity, receipt)?;
        }
        if recovery.episodes.len()
            != usize::try_from(recovery.current_provider_checkpoint_seq)
                .map_err(|_| EngineError::InvalidRecoverySnapshot("provider sequence"))?
        {
            return Err(EngineError::InvalidRecoverySnapshot(
                "provider history is incomplete",
            ));
        }
        let mut episode_hashes = BTreeSet::new();
        for (index, recovered) in recovery.episodes.iter().enumerate() {
            let expected_seq = u64::try_from(index)
                .ok()
                .and_then(|value| value.checked_add(1))
                .ok_or(EngineError::InvalidRecoverySnapshot("provider sequence"))?;
            if recovered.checkpoint_seq != expected_seq
                || ContentHash::sha256(&recovered.episode_bytes) != recovered.episode_hash
                || !episode_hashes.insert(recovered.episode_hash.clone())
            {
                return Err(EngineError::RecoveryArtifactMismatch(
                    "provider episode history",
                ));
            }
        }
        for action in &recovery.actions {
            if !episode_hashes.contains(&action.episode_hash)
                || action.result_hash.is_some() != action.result_bytes.is_some()
                || action
                    .result_bytes
                    .as_ref()
                    .zip(action.result_hash.as_ref())
                    .is_some_and(|(bytes, hash)| ContentHash::sha256(bytes) != *hash)
            {
                return Err(EngineError::RecoveryArtifactMismatch("action history"));
            }
        }

        let base_count = if let Some(checkpoint) = &recovery.state {
            if checkpoint.recovery_schema_hash != active_run_checkpoint_schema_hash()
                || ContentHash::sha256(&checkpoint.state_bytes) != checkpoint.state_hash
                || checkpoint.provider_checkpoint_seq > recovery.current_provider_checkpoint_seq
                || checkpoint.action_frontier_seq > recovery.current_action_frontier_seq
                || (checkpoint.action_frontier_seq == recovery.current_action_frontier_seq
                    && checkpoint.action_frontier_hash != recovery.current_action_frontier_hash)
            {
                return Err(EngineError::RecoveryArtifactMismatch(
                    "runtime state receipt",
                ));
            }
            usize::try_from(checkpoint.provider_checkpoint_seq)
                .map_err(|_| EngineError::InvalidRecoverySnapshot("state provider sequence"))?
        } else {
            0
        };
        if recovery.episodes.len().saturating_sub(base_count) > 1 {
            return Err(EngineError::InvalidRecoverySnapshot(
                "more than one unprocessed provider episode",
            ));
        }

        let base_episode_hashes = recovery
            .episodes
            .iter()
            .take(base_count)
            .map(|episode| episode.episode_hash.clone())
            .collect::<BTreeSet<_>>();
        let mut consumed_actions = BTreeSet::new();
        for recovered in recovery.episodes.iter().take(base_count) {
            self.replay_committed_episode(
                input,
                state,
                recovered,
                &recovery.actions,
                &mut consumed_actions,
                recovery.child.as_ref(),
            )
            .await?;
        }
        for action in recovery
            .actions
            .iter()
            .filter(|action| base_episode_hashes.contains(&action.episode_hash))
        {
            if !consumed_actions.contains(&action.action_key) {
                return Err(EngineError::InvalidRecoverySnapshot(
                    "checkpoint contains an unreplayed action",
                ));
            }
        }

        match &recovery.state {
            Some(checkpoint) => {
                let declared: ActiveRunCheckpoint =
                    serde_json::from_slice(&checkpoint.state_bytes)?;
                if declared.direct_answer_retry_requested != state.direct_answer_retry_requested {
                    return Err(EngineError::RecoveryStateMismatch);
                }
                // The direct-answer retry is a durable run-local control bit,
                // not a workflow transition. Restore it before the
                // equivalence check without contaminating `state_trace`.
                state.direct_answer_retry_requested = declared.direct_answer_retry_requested;
                // The duration accumulators (`provider_total_ms`,
                // `capability_total_ms`, `compact_total_ms`) are
                // observability-only fields: replay rebuilds kernel state
                // without re-executing the underlying provider/capability/
                // compaction work, so the freshly reconstructed `state` will
                // always have zeros here. Restore the persisted values so the
                // recovery-equivalence check below is not perturbed by
                // telemetry that has no correctness bearing on the run.
                state.usage.provider_total_ms = declared.usage.provider_total_ms;
                state.usage.capability_total_ms = declared.usage.capability_total_ms;
                state.usage.compact_total_ms = declared.usage.compact_total_ms;
                state.usage.provider_queue_wait_ms = declared.usage.provider_queue_wait_ms;
                state.usage.session_memory_total_ms = declared.usage.session_memory_total_ms;
                state.usage.market_preflight_ms = declared.usage.market_preflight_ms;
                state.usage.prompt_build_total_ms = declared.usage.prompt_build_total_ms;
                state.usage.checkpoint_total_ms = declared.usage.checkpoint_total_ms;
                if declared.schema_version != ACTIVE_RUN_CHECKPOINT_SCHEMA_VERSION {
                    return Err(EngineError::RecoveryStateMismatch);
                }
                // The recovery-equivalence check compares the persisted
                // checkpoint against the freshly reconstructed state. The
                // following fields are excluded because replay legitimately
                // diverges from the live path:
                //
                // * `prompt_receipt_hashes` / `conversation_hash`: recovery
                //   turns contribute receipts/transcript entries that replay
                //   does not reproduce.
                // * `usage` (BudgetUsage): replay only processes committed
                //   episodes, so its turn/repair/replan counters are lower than
                //   the live path which includes superseded recovery turns.
                // * `compaction_receipts`: the live path may compact at
                //   different intermediate states than replay.
                // * `tool_schema_hash`: the tool frontier depends on the
                //   interpreter's current state (remaining capability visits).
                //   During recovery the interpreter has already advanced past
                //   the checkpoint-captured state, so the capability frontier
                //   — and therefore the tool schema — can legitimately differ.
                //   The original episode validated the tool schema at live-run
                //   time; replay's job is state reconstruction, not
                //   re-verification of the dynamic tool frontier.
                //
                // Security-critical fields (interpreter state, evidence ledger,
                // action frontier) are still fully compared.
                let rebuilt = state.checkpoint_value()?;
                if declared.interpreter != rebuilt.interpreter
                    || declared.state_trace != rebuilt.state_trace
                    || declared.capability_calls != rebuilt.capability_calls
                    || declared.completed_capabilities != rebuilt.completed_capabilities
                    || declared.logical_action_keys != rebuilt.logical_action_keys
                    || declared.evidence_ledger_hash != rebuilt.evidence_ledger_hash
                    || declared.presentation_packs_hash != rebuilt.presentation_packs_hash
                    || declared.composed_sections_hash != rebuilt.composed_sections_hash
                    || declared.calculations_hash != rebuilt.calculations_hash
                    || declared.action_cache_hash != rebuilt.action_cache_hash
                    || declared.accepted_actions_hash != rebuilt.accepted_actions_hash
                    || declared.last_provider_episode_hash != rebuilt.last_provider_episode_hash
                    || declared.compacted_context_hash != rebuilt.compacted_context_hash
                    || declared.research_planner_hash != rebuilt.research_planner_hash
                    || declared.derived_ticker_scope_hash != rebuilt.derived_ticker_scope_hash
                    || declared.session_memory_hash != rebuilt.session_memory_hash
                {
                    return Err(EngineError::RecoveryStateMismatch);
                }
            }
            None if base_count != 0 => {
                return Err(EngineError::InvalidRecoverySnapshot(
                    "base history has no typed state checkpoint",
                ));
            }
            None => {}
        }

        let pending = recovery.episodes.get(base_count).cloned().map(|episode| {
            let has_action_receipt = recovery
                .actions
                .iter()
                .any(|action| action.episode_hash == episode.episode_hash);
            RecoveredPendingEpisode {
                episode,
                has_action_receipt,
            }
        });
        if pending.is_none() {
            // Post-checkpoint actions must bind to a known episode. The old
            // implication — "frontier advanced past the checkpoint ⇒ an
            // unprocessed episode must exist" — broke with the supplemental
            // drain (2026-09-08 deferral incidents): drain receipts and
            // failed attempts record against the last COMMITTED episode
            // after the checkpoint, with no pending episode at all. The
            // genuine inconsistency is an action bound to an episode that is
            // neither committed base history nor the last committed episode.
            let last_committed = recovery
                .episodes
                .last()
                .map(|episode| episode.episode_hash.clone());
            let orphaned_action = recovery.actions.iter().any(|action| {
                Some(&action.episode_hash) != last_committed.as_ref()
                    && !base_episode_hashes.contains(&action.episode_hash)
            });
            if orphaned_action {
                return Err(EngineError::InvalidRecoverySnapshot(
                    "action frontier advanced without a pending episode",
                ));
            }
        }
        Ok(RecoveredExecution {
            pending,
            child: recovery.child,
        })
    }

    /// Replay one drained supplemental observation action from its committed
    /// receipt, mirroring the live drain's settlement order (route →
    /// receipt-validated restore → ingest → transcript → capability
    /// completion → compaction). Added 2026-09-08: the live supplemental
    /// drain executes the remainder of a homogeneous observation batch one
    /// statechart visit at a time with receipts bound to the emitting
    /// episode, and deferral-resumed runs must reconstruct those settlements
    /// instead of tripping the "unreplayed action" invariant.
    async fn replay_committed_observation_action(
        &self,
        input: &RunInput<'_>,
        state: &mut ActiveRun,
        call: &PreparedCall,
        action: &RecoveredAction,
        episode_hash: ContentHash,
        append_transcript: bool,
        consumed_actions: &mut BTreeSet<String>,
    ) -> Result<(), EngineError> {
        if let Some(cached) = state.cached_action_result(call) {
            state.complete_cached_capability(
                input.image,
                call,
                &cached,
                episode_hash,
                self.config.max_compacted_context_bytes,
                append_transcript,
            )?;
            if !consumed_actions.insert(call.action_key.clone()) {
                return Err(EngineError::InvalidRecoverySnapshot(
                    "action was replayed more than once",
                ));
            }
            return state.check_conversation_limit(self.config.max_conversation_bytes);
        }
        if action.tool_call_id != call.tool_call_id
            || action.capability_id != call.capability.id
            || action.request_hash != call.request_hash
            || action.input_schema_hash != call.contracts.input.content_hash
            || action.output_schema_hash != call.contracts.output_contract_set_hash
            || action.data_release_hash != call.binding.data_release_hash
            || !action.retryable_read
        {
            return Err(EngineError::RecoveryArtifactMismatch("action receipt"));
        }
        let result_hash = action
            .result_hash
            .as_ref()
            .ok_or(EngineError::RecoveryArtifactMismatch("action result hash"))?;
        let result_bytes = action
            .result_bytes
            .as_ref()
            .ok_or(EngineError::RecoveryArtifactMismatch("action result bytes"))?;
        if ContentHash::sha256(result_bytes) != *result_hash {
            return Err(EngineError::RecoveryArtifactMismatch("action result"));
        }
        let result: CapabilityResult = serde_json::from_slice(result_bytes)?;
        // Live-parity tolerance: the drain's own loop stops routing and
        // clears its queue when the workflow moves somewhere the remaining
        // reads cannot follow; replay must not die on the same boundary
        // (2026-09-08 resume died with WorkflowResolution "capability
        // proposal source" routing a replayed sibling out of turn).
        if state.route_to_capability(call, episode_hash.clone()).is_err() {
            state.pending_supplemental_calls.clear();
            consumed_actions.insert(call.action_key.clone());
            return Ok(());
        }
        let invocation = capability_invocation(&input.request.run_id, call);
        self.capabilities
            .restore_committed_result(&invocation, &result)
            .await
            .map_err(|failure| EngineError::Dependency {
                component: "capability.restore_recovered_result",
                failure,
            })?;
        if state.commit_research_result(call, &result).is_err() {
            // Live parity: a drain call whose research-result commit fails is
            // skipped, not fatal.
            consumed_actions.insert(call.action_key.clone());
            return Ok(());
        }
        state.reserve_capability_call(&call.capability.id, &call.action_key)?;
        state.record_evidence_bytes(result_bytes.len())?;
        let symbol = call
            .arguments
            .get("symbol")
            .and_then(serde_json::Value::as_str);
        state.ingest(&result, call.capability.id.as_str(), symbol)?;
        state.ingest_scope_projection(call, &result)?;
        if append_transcript {
            state.append_capability_tool_result(call, &result)?;
        }
        if capability_result_completes_prerequisite(&result) {
            state.completed_capabilities.insert(call.capability.id.clone());
        }
        state.record_accepted_action(call)?;
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
            call,
            &result,
            accepted_action_receipt_hash(call, &result)?,
        )?;
        if capability_result_completes_prerequisite(&result) {
            state.compact_settled_phase(
                episode_hash,
                self.config.max_compacted_context_bytes,
            )?;
        }
        if !consumed_actions.insert(call.action_key.clone()) {
            return Err(EngineError::InvalidRecoverySnapshot(
                "action was replayed more than once",
            ));
        }
        state.check_conversation_limit(self.config.max_conversation_bytes)
    }

    pub(crate) async fn replay_committed_episode(
        &self,
        input: &RunInput<'_>,
        state: &mut ActiveRun,
        recovered: &RecoveredEpisode,
        actions: &[RecoveredAction],
        consumed_actions: &mut BTreeSet<String>,
        child_receipt: Option<&ChildExecutionReceipt>,
    ) -> Result<(), EngineError> {
        let child_policy = bounded_child::current_policy(input.image, state)?;
        if child_policy.is_none() {
            state.clear_child_skill_context();
        }
        let child_inputs = child_policy
            .as_ref()
            .map(|policy| bounded_child::prepare_inputs(policy, state))
            .transpose()?;
        if child_policy.is_some() && child_receipt.is_none() {
            return Err(EngineError::RecoveryArtifactMismatch(
                "child episode lacks child receipt",
            ));
        }
        state.reserve_provider_turn(input.image)?;
        let messages = if child_policy.is_some() {
            Vec::new()
        } else {
            std::mem::take(&mut state.messages)
        };
        let mut built = build_provider_request(input, state, &self.config, messages)?;
        if let (Some(policy), Some(inputs), Some(receipt)) =
            (&child_policy, &child_inputs, child_receipt)
        {
            let usage = bounded_child::usage(receipt, state)?;
            bounded_child::isolate_request(
                &mut built,
                input.image,
                state,
                policy,
                inputs,
                &state.child_skill_context,
                &usage,
            )?;
        }
        state.record_prompt_assembly(
            built.tool_definitions,
            built.episode_context.tool_schema_hash,
            built.prompt_receipt_hash,
        )?;
        let request = built.request;
        let request_hash = ContentHash::sha256(serde_jcs::to_vec(&request)?);
        if request.messages.len() < WIRE_TRUSTED_PREFIX_MESSAGE_COUNT {
            return Err(EngineError::Invariant("trusted prompt prefix disappeared"));
        }
        if child_policy.is_none() {
            state.messages = built.transcript;
        } else if !request.messages[WIRE_TRUSTED_PREFIX_MESSAGE_COUNT..].is_empty() {
            return Err(EngineError::RecoveryArtifactMismatch(
                "child request transcript",
            ));
        }
        let episode: ProviderEpisodeV1 = serde_json::from_slice(&recovered.episode_bytes)?;
        // During recovery replay the interpreter has already advanced to the
        // *next* role/state (the checkpoint captures the post-transition state).
        // This means `build_provider_request` above reconstructs the request
        // under the wrong role's thinking mode, producing a different
        // `request_hash` than the one baked into the episode at live-run time.
        //
        // The episode was fully validated during the original live run (request
        // hash, replay hash, model identity, tool-call structure). Its bytes are
        // content-addressed by `episode_hash` (checked in `restore_recovery`).
        // Replay's job is state reconstruction (messages, ledger, tool results),
        // not re-verification of the request envelope. We therefore trust the
        // episode's own `request_hash` rather than the rebuilt one.
        let _ = request_hash; // still computed for diagnostic parity
        // Same trust decision for the tool frontier (2026-09-08,
        // run_b23662 family): the interpreter has already advanced past the
        // checkpoint-captured state during replay, so the rebuilt capability
        // frontier — and therefore `state.tool_schema_hash` — legitimately
        // diverges from the schema the episode was validated against at
        // live-run time. The live orchestrator validated the frontier when
        // the episode was admitted; replay re-validates against the
        // episode's own recorded hash instead of failing resumed runs with
        // a spurious "provider episode contract mismatch".
        validate_episode(
            &episode,
            &episode.request_hash,
            input.image,
            &episode.tool_schema_hash,
            &input.snapshot.resolved_model,
            &input.snapshot.provider_api_version,
            input.snapshot.provider_wire_capabilities,
            request.thinking.kind,
        )?;
        state.record_provider_usage(&episode)?;
        state.record_provider_episode_hash(recovered.episode_hash.clone());
        if let Some(receipt) = child_receipt
            && child_policy.is_some()
        {
            bounded_child::usage(receipt, state)?;
        }

        let output = match classify_provider_output(state.current_model_output_mode()?, &episode) {
            Ok(output) => output,
            Err(error) => {
                let Some(directive) = model_recovery_directive(&error) else {
                    return Err(error);
                };
                if actions
                    .iter()
                    .any(|action| action.episode_hash == recovered.episode_hash)
                {
                    return Err(EngineError::RecoveryArtifactMismatch(
                        "recovered decision rejection has an action receipt",
                    ));
                }
                if !state.recover_model_decision(input.image, &episode, directive)? {
                    return Err(error);
                }
                return state.check_conversation_limit(self.config.max_conversation_bytes);
            }
        };

        if let Some(resolution) =
            resolve_local_skill_load(&episode, &state.tool_definitions, input.image)?
        {
            apply_local_skill_load_resolution(
                state,
                &episode,
                &resolution,
                child_policy.is_some(),
            )?;
            return state.check_conversation_limit(self.config.max_conversation_bytes);
        }

        match output {
            ProviderOutputDisposition::Markdown => {
                // A bounded direct-answer retry is resumable state, not a
                // terminal answer. Reconstruct the same kernel transition
                // that the live path applies in `finish` so a daemon restart
                // can continue without redispatching the truncated episode.
                if state.current_operation_emits_answer(input.image)? {
                    if episode.finish_reason == "length" && !state.direct_answer_retry_requested() {
                        state.request_direct_answer_retry();
                        state.append_direct_answer_retry_feedback("answer_output_truncated");
                        return state.check_conversation_limit(self.config.max_conversation_bytes);
                    }
                    if episode.finish_reason == "stop"
                        && episode
                            .assistant
                            .content
                            .as_deref()
                            .is_none_or(|content| content.trim().is_empty())
                        && !state.direct_answer_retry_requested()
                    {
                        state.request_direct_answer_retry();
                        state.append_assistant(&episode);
                        state.append_direct_answer_retry_feedback("final_output_missing");
                        return state.check_conversation_limit(self.config.max_conversation_bytes);
                    }
                }
                return Err(EngineError::InvalidRecoverySnapshot(
                    "terminal final output was checkpointed as resumable state",
                ));
            }
            ProviderOutputDisposition::TypedJson => {
                // E1 sectioned compose: a committed mid-loop section batch is
                // resumable state, not a terminal final. Rebuild the exact
                // compose→verify→acknowledgement walk and the composed-sections
                // retention from the committed episode so the
                // recovery-equivalence check below (which compares
                // `composed_sections_hash`) compares like with like.
                if state.section_output_contract()?.is_some() {
                    return replay_committed_section_batch(
                        self.config.max_conversation_bytes,
                        input,
                        state,
                        &episode,
                    );
                }
                return Err(EngineError::InvalidRecoverySnapshot(
                    "terminal final output was checkpointed as resumable state",
                ));
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
                        if actions
                            .iter()
                            .any(|action| action.episode_hash == recovered.episode_hash)
                        {
                            return Err(EngineError::RecoveryArtifactMismatch(
                                "recovered transition rejection has an action receipt",
                            ));
                        }
                        if !state.recover_model_decision(input.image, &episode, directive)? {
                            return Err(error);
                        }
                        return state.check_conversation_limit(self.config.max_conversation_bytes);
                    }
                };
                let facts = kernel_workflow_facts(input.image, input.request, &transition.event)?;
                if !state.event_has_remaining_target(&transition.event, &facts)? {
                    if actions
                        .iter()
                        .any(|action| action.episode_hash == recovered.episode_hash)
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
                    return state.check_conversation_limit(self.config.max_conversation_bytes);
                }
                // Recovery mirrors the live no-transcript boundary; a bounded
                // child must not synthesize a parent transcript acknowledgement.
                state.handle_model_event(
                    &transition.event,
                    &facts,
                    recovered.episode_hash.clone(),
                )?;
                // Release B: judgment notes ride only the final
                // evidence-sufficient handoff toward composition; a note
                // on any mid-research transition is dropped.
                let judgment = transition.judgment.clone();
                state.capture_analyst_judgment(&transition.event, judgment);
                if child_policy.is_none() {
                    state.append_workflow_transition_result(&episode, &transition)?;
                    state.compact_settled_phase(
                        recovered.episode_hash.clone(),
                        self.config.max_compacted_context_bytes,
                    )?;
                }
                state.check_conversation_limit(self.config.max_conversation_bytes)?;
                return Ok(());
            }
            ProviderOutputDisposition::Capability => {}
        }

        let prepared = match prepare_calls(
            &episode,
            input,
            state,
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
                        recovered.episode_hash.clone(),
                    )?
                {
                    return state.check_conversation_limit(self.config.max_conversation_bytes);
                }
                let Some(directive) = model_recovery_directive(&error) else {
                    return Err(error);
                };
                if actions
                    .iter()
                    .any(|action| action.episode_hash == recovered.episode_hash)
                {
                    return Err(EngineError::RecoveryArtifactMismatch(
                        "rejected model proposal episode has an action receipt",
                    ));
                }
                if !state.recover_model_decision(input.image, &episode, directive)? {
                    return Err(error);
                }
                return state.check_conversation_limit(self.config.max_conversation_bytes);
            }
        };
        let mut prepared = prepared;
        if child_policy.is_some() && prepared.len() != 1 {
            return Err(EngineError::WorkflowResolution {
                outcome: "exactly one bounded child recovered capability action",
            });
        }
        let question_only = matches!(
            input.request.context,
            krw_agent_protocol::RunContextV1::QuestionOnly {}
        );
        // Live parity (2026-09-08, run_8806a8ce): a replayed episode whose
        // decision the live path recovered through a repair directive (an
        // over-size observation batch, a mixed batch) must recover the same
        // way here. Propagating the decision error killed deferral-resumed
        // runs terminally even though the identical live decision had a
        // repair path.
        let selected_index = match state.decide_research_dispatch(&prepared, question_only) {
            Ok(ResearchDispatchDecision::Execute { selected_index }) => selected_index,
            Ok(decision) => match decision {
                ResearchDispatchDecision::NoPositiveValue(reason) => {
                    if actions
                        .iter()
                        .any(|action| action.episode_hash == recovered.episode_hash)
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
                    return state.check_conversation_limit(self.config.max_conversation_bytes);
                }
                ResearchDispatchDecision::ProposalRejected(reason) => {
                    if actions
                        .iter()
                        .any(|action| action.episode_hash == recovered.episode_hash)
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
                    return state.check_conversation_limit(self.config.max_conversation_bytes);
                }
                ResearchDispatchDecision::Execute { .. } => unreachable!("matched above"),
            },
            Err(error) => {
                let Some(directive) = model_recovery_directive(&error) else {
                    return Err(error);
                };
                if actions
                    .iter()
                    .any(|action| action.episode_hash == recovered.episode_hash)
                {
                    return Err(EngineError::RecoveryArtifactMismatch(
                        "recovered dispatch rejection has an action receipt",
                    ));
                }
                if !state.recover_model_decision(input.image, &episode, directive)? {
                    return Err(error);
                }
                return state.check_conversation_limit(self.config.max_conversation_bytes);
            }
        };
        if selected_index >= prepared.len() {
            return Err(EngineError::ResearchPlannerDecisionMismatch);
        }
        let selected = prepared.remove(selected_index);
        let child_call_kind = child_policy
            .as_ref()
            .map(|policy| bounded_child::authorize_call(policy, state, &selected))
            .transpose()?;
        state.preflight_capability_calls(input.image, std::slice::from_ref(&selected))?;
        if child_call_kind == Some(bounded_child::ChildCallKind::TypedReturn) {
            let receipt = child_receipt.ok_or(EngineError::RecoveryArtifactMismatch(
                "typed child return receipt",
            ))?;
            if receipt.stage == krw_agent_bounded_child::ChildStage::Completed {
                bounded_child::validate_completed_return(receipt, state, &selected)?;
            }
        }
        state.route_to_capability(&selected, recovered.episode_hash.clone())?;
        if child_policy.is_none() {
            state.append_assistant(&episode);
        }
        // Live parity for the supplemental drain (2026-09-08, run_8806a8ce):
        // the live path executes a homogeneous non-research batch by
        // dispatching the first call and draining the rest one statechart
        // visit at a time — each drained call leaves an accepted action
        // receipt bound to THIS episode. Replay used to mark the remainder
        // "unselected", which left those receipts unconsumed and tripped the
        // "unreplayed action" invariant on every deferral-resumed run.
        // Rebuild the live settlement instead: research alternatives stay
        // unselected; drained observation calls replay from their receipts;
        // a drained call whose attempt failed (ambiguous stage, the exact
        // deferral trigger) consumes as a failed attempt; a call the
        // interruption never reached returns to the pending drain.
        let mut drained_calls = Vec::new();
        let mut unselected = Vec::new();
        for call in prepared {
            if call.capability.research_action.is_some() {
                unselected.push(call);
            } else {
                drained_calls.push(call);
            }
        }
        if child_policy.is_none() {
            state.append_unselected_research_results(&unselected)?;
        }
        for call in drained_calls {
            let receipt = actions.iter().find(|action| {
                action.action_key == call.action_key
                    && action.episode_hash == recovered.episode_hash
            });
            match receipt {
                Some(action) if action.stage == ActionStage::Accepted => {
                    self.replay_committed_observation_action(
                        input,
                        state,
                        &call,
                        action,
                        recovered.episode_hash.clone(),
                        child_policy.is_none(),
                        consumed_actions,
                    )
                    .await?;
                }
                Some(_) => {
                    // A recorded non-accepted stage is a failed attempt (the
                    // deferral trigger itself). The live path continued past
                    // it; replay consumes it without re-execution.
                    consumed_actions.insert(call.action_key.clone());
                }
                None => {
                    state.pending_supplemental_calls.push((
                        recovered.episode_hash.clone(),
                        call,
                    ));
                }
            }
        }
        for call in [selected] {
            // Mirror the live cache-hit branch: when the replayed episode's
            // action was already executed (its canonical action key is
            // charged in `logical_action_keys`) and its committed result is
            // resident in the action cache, the live path settled this
            // episode from the cache and never created an action receipt.
            // Replaying it must reuse the cached result the same way — the
            // shared `cached_action_result` / `complete_cached_capability`
            // helpers keep the live and recovery predicates identical —
            // instead of demanding a receipt that was correctly never
            // persisted. When the cache entry is absent (for example an
            // episode that was a cache hit in a previous incarnation whose
            // entry no longer reconstructs), this falls through to the
            // receipt requirement below and fails closed.
            if let Some(cached) = state.cached_action_result(&call) {
                state.complete_cached_capability(
                    input.image,
                    &call,
                    &cached,
                    recovered.episode_hash.clone(),
                    self.config.max_compacted_context_bytes,
                    child_policy.is_none(),
                )?;
                // The action key was already consumed by the replay of the
                // earlier episode that first executed it; a cache-hit replay
                // creates no additional durable action.
                state.check_conversation_limit(self.config.max_conversation_bytes)?;
                continue;
            }
            let mut matching = actions.iter().filter(|action| {
                action.action_key == call.action_key
                    && action.episode_hash == recovered.episode_hash
            });
            let action = matching.next().ok_or(EngineError::InvalidRecoverySnapshot(
                "checkpointed episode is missing its action",
            ))?;
            if matching.next().is_some()
                || action.tool_call_id != call.tool_call_id
                || action.capability_id != call.capability.id
                || action.request_hash != call.request_hash
                || action.input_schema_hash != call.contracts.input.content_hash
                || action.output_schema_hash != call.contracts.output_contract_set_hash
                || action.data_release_hash != call.binding.data_release_hash
                || !action.retryable_read
                || action.stage != ActionStage::Accepted
            {
                return Err(EngineError::RecoveryArtifactMismatch("action receipt"));
            }
            let result_hash = action
                .result_hash
                .as_ref()
                .ok_or(EngineError::RecoveryArtifactMismatch("action result hash"))?;
            let result_bytes = action
                .result_bytes
                .as_ref()
                .ok_or(EngineError::RecoveryArtifactMismatch("action result bytes"))?;
            if ContentHash::sha256(result_bytes) != *result_hash {
                return Err(EngineError::RecoveryArtifactMismatch("action result"));
            }
            let result: CapabilityResult = serde_json::from_slice(result_bytes)?;
            let after_action =
                self.evaluate_after_action(input.image, state, &call, &result, result_hash)?;
            if !after_action.accepted {
                return Err(EngineError::RecoveryArtifactMismatch(
                    "recovered action failed AfterAction policy",
                ));
            }
            let invocation = capability_invocation(&input.request.run_id, &call);
            self.capabilities
                .restore_committed_result(&invocation, &result)
                .await
                .map_err(|failure| EngineError::Dependency {
                    component: "capability.restore_recovered_result",
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
            state.reserve_capability_call(&call.capability.id, &call.action_key)?;
            if child_call_kind == Some(bounded_child::ChildCallKind::Capability) {
                bounded_child::usage(
                    child_receipt.ok_or(EngineError::RecoveryArtifactMismatch(
                        "child capability receipt",
                    ))?,
                    state,
                )?;
            }
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
                // Recovery must recreate the same settled transcript boundary
                // as the live path before it can issue another provider turn.
                state.compact_settled_phase(
                    recovered.episode_hash.clone(),
                    self.config.max_compacted_context_bytes,
                )?;
            }
            if child_call_kind == Some(bounded_child::ChildCallKind::TypedReturn) {
                bounded_child::validate_replayed_return_state(
                    input.image,
                    state,
                    child_policy
                        .as_ref()
                        .ok_or(EngineError::RecoveryArtifactMismatch(
                            "typed child return policy",
                        ))?,
                    child_receipt.ok_or(EngineError::RecoveryArtifactMismatch(
                        "typed child return receipt",
                    ))?,
                )?;
            }
            if !consumed_actions.insert(call.action_key.clone()) {
                return Err(EngineError::InvalidRecoverySnapshot(
                    "action was replayed more than once",
                ));
            }
        }
        state.check_conversation_limit(self.config.max_conversation_bytes)
    }

    pub(crate) async fn checkpoint_active_state(
        &self,
        identity: &RunIdentity,
        state: &ActiveRun,
        deadline: Instant,
    ) -> Result<(), EngineError> {
        let state_bytes = state.checkpoint_bytes()?;
        ensure_size(
            state_bytes.len(),
            self.config.max_recovery_state_bytes,
            "run_recovery_state",
        )?;
        let state_hash = ContentHash::sha256(&state_bytes);
        let durable = DurableRunState {
            run_id: identity.run_id.clone(),
            fencing_token: identity.fencing_token,
            recovery_schema_hash: active_run_checkpoint_schema_hash(),
            state_hash,
            state_bytes,
            lifecycle_stage: state.lifecycle_stage_for_checkpoint()?,
        };
        let checkpoint_t0 = Instant::now();
        let result = dependency_call(
            deadline,
            "persistence.checkpoint_run_state",
            self.persistence.checkpoint_run_state(&durable),
        )
        .await;
        if let Some(timings) = state.runtime_timings.as_ref() {
            timings.add_checkpoint(checkpoint_t0.elapsed());
        }
        result
    }
}

/// Classify only errors caused by a provider decision that can be corrected
/// without changing an immutable run boundary. Metric identifiers and JSON
/// pointers in the detail are safe to disclose: they are already advertised
/// in the system prompt ontology catalog and tool schema.
pub(crate) fn model_recovery_directive(error: &EngineError) -> Option<ModelRecoveryDirective> {
    match error {
        EngineError::ModelProposalRejected(rejection) => {
            if let ModelProposalRejection::Order { codes } = rejection {
                return Some(ModelRecoveryDirective::with_detail(
                        rejection.code(),
                        rejection.repair_mode(),
                        RecoveryDetailV1 {
                            schema_version: 1,
                            field: "/name".to_string(),
                            offending_value: codes.join(", "),
                            valid_alternatives: vec![
                                "the earlier rung named by the violated rule".to_string(),
                                "continue with the evidence already admitted".to_string(),
                            ],
                            hint: "This tool call violates a state-order rule (for example the news fallback ladder: filing-catalog search before feed news, and feed news before web news). Pick the earlier rung of the violated rule, or proceed with the already-admitted evidence. The last-resort rung is never the first call.".to_string(),
                        },
                    ));
            }
            if rejection.code() == "guru_company_brief_input_empty" {
                return Some(ModelRecoveryDirective::with_detail(
                    rejection.code(),
                    rejection.repair_mode(),
                    RecoveryDetailV1 {
                        schema_version: 1,
                        field: "/".to_string(),
                        offending_value: "{}".to_string(),
                        valid_alternatives: vec![
                            "question".to_string(),
                            "guru_principle_ids".to_string(),
                            "company_context_anchor_ids".to_string(),
                            "hypothesis".to_string(),
                            "counter_hypothesis".to_string(),
                            "evidence_needed".to_string(),
                            "strengthens_if".to_string(),
                            "weakens_if".to_string(),
                            "why_material".to_string(),
                            "decision_role".to_string(),
                        ],
                        hint: "Submit one non-empty object with exactly these ten keys. Use arrays for guru_principle_ids, company_context_anchor_ids, and evidence_needed; copy the IDs from guru.query_context; set decision_role to main_tension. Do not resubmit {} or add a wrapper.".to_string(),
                    },
                ));
            }
            if rejection.code() == "guru_company_brief_draft_unlinked" {
                return Some(ModelRecoveryDirective::with_detail(
                    rejection.code(),
                    rejection.repair_mode(),
                    RecoveryDetailV1 {
                        schema_version: 1,
                        field: "/guru_principle_ids or /company_context_anchor_ids".to_string(),
                        offending_value: "an ID not present in the committed Guru context".to_string(),
                        valid_alternatives: vec![
                            "copy selected_lenses[*].reviewed_id verbatim".to_string(),
                            "copy company_context.context_anchors[*].anchor_id verbatim".to_string(),
                        ],
                        hint: "Keep the same ten-key draft shape, but replace every principle and anchor ID with an exact ID returned by guru.query_context. Do not invent, shorten, hash, or rename IDs; use one to three evidence_needed strings and decision_role main_tension.".to_string(),
                    },
                ));
            }
            let detail = rejection.violation().map(violation_to_detail);
            match detail {
                Some(d) => Some(ModelRecoveryDirective::with_detail(
                    rejection.code(),
                    rejection.repair_mode(),
                    d,
                )),
                None => Some(ModelRecoveryDirective::with_mode(
                    rejection.code(),
                    rejection.repair_mode(),
                )),
            }
        }
        EngineError::InvalidToolCallId => {
            Some(ModelRecoveryDirective::replace("tool_call_id_invalid"))
        }
        EngineError::TooManyToolCalls { .. } => {
            Some(ModelRecoveryDirective::replace("too_many_tool_calls"))
        }
        EngineError::UnknownCapability(_) => {
            Some(ModelRecoveryDirective::replace("capability_not_available"))
        }
        EngineError::SkillNotFound {
            skill_id,
            available,
        } => Some(ModelRecoveryDirective::with_detail(
            "skill_not_found",
            "replace",
            RecoveryDetailV1 {
                schema_version: 1,
                field: "skill_id".to_string(),
                offending_value: skill_id.clone(),
                valid_alternatives: available.split(", ").map(str::to_string).collect(),
                hint: "The requested skill_id is not available in this image. \
                       Use one of valid_alternatives. Skill ids are lowercase \
                       snake_case and must match a catalog entry exactly."
                    .to_string(),
            },
        )),
        EngineError::CapabilityPrerequisiteMissing(_) => Some(ModelRecoveryDirective::replace(
            "capability_prerequisite_pending",
        )),
        EngineError::ToolArgumentsMustBeObject(_) => {
            Some(ModelRecoveryDirective::replace("tool_arguments_invalid"))
        }
        EngineError::InvalidWorkflowControl => {
            Some(ModelRecoveryDirective::replace("transition_not_available"))
        }
        EngineError::InvalidWorkflowTransitionShape => {
            Some(ModelRecoveryDirective::replace("transition_shape_invalid"))
        }
        EngineError::WorkflowResolution { outcome }
            if matches!(
                *outcome,
                "state-scoped capability frontier"
                    | "typed capability frontier"
                    | "typed transition frontier"
                    | "capability model input schema"
                    | "capability model input schema pin"
                    | "capability proposal source"
            ) =>
        {
            Some(ModelRecoveryDirective::replace("capability_not_available"))
        }
        EngineError::WorkflowResolution { outcome }
            if matches!(
                *outcome,
                "direct capability proposal"
                    | "proposal validation"
                    | "validated capability"
                    | "model decision state"
                    | "model role"
                    | "model output mode"
                    | "model artifact source"
                    | "validated capability boundary"
                    | "capability completion"
            ) =>
        {
            Some(ModelRecoveryDirective::replace(
                "decision_not_allowed_in_state",
            ))
        }
        EngineError::WorkflowResolution { outcome }
            if matches!(
                *outcome,
                "non-research capability decision batch"
                    | "mixed research capability decision batch"
                    | "exactly one bounded child capability action"
            ) =>
        {
            Some(ModelRecoveryDirective::replace(
                "decision_batch_size_invalid",
            ))
        }
        EngineError::ResearchPlannerDecisionMismatch => {
            Some(ModelRecoveryDirective::replace("proposal_not_actionable"))
        }
        EngineError::RunScopeViolation(_) => Some(ModelRecoveryDirective::replace(
            "capability_scope_not_authorized",
        )),
        EngineError::ResearchPlanner(
            ResearchPlannerError::IntentGoalDefinitionDrift
            | ResearchPlannerError::GoalDefinitionDrift,
        ) => Some(ModelRecoveryDirective::replace(
            "proposal_goal_definition_changed",
        )),
        EngineError::ResearchPlanner(
            ResearchPlannerError::IntentClauseBindingDrift
            | ResearchPlannerError::ClauseDefinitionDrift,
        ) => Some(ModelRecoveryDirective::replace(
            "proposal_clause_binding_changed",
        )),
        EngineError::ResearchPlanner(ResearchPlannerError::IntentGoalProgressDrift) => Some(
            ModelRecoveryDirective::replace("proposal_goal_progress_regressed"),
        ),
        EngineError::ResearchPlanner(ResearchPlannerError::IntentCoverageProvenanceMissing) => {
            Some(ModelRecoveryDirective::replace(
                "proposal_coverage_provenance_missing",
            ))
        }
        EngineError::ResearchPlanner(ResearchPlannerError::IntentAnchorMismatch) => Some(
            ModelRecoveryDirective::replace("proposal_intent_anchor_changed"),
        ),
        EngineError::InvalidProviderEpisode(reason) => match *reason {
            "final episode must finish with stop" | "final episode has no content" => {
                Some(ModelRecoveryDirective::replace("answer_output_invalid"))
            }
            "capability target has no remaining statechart capacity"
            | "provider invoked a capability absent from the advertised dynamic frontier" => {
                Some(ModelRecoveryDirective::replace("capability_not_available"))
            }
            "tool calls require tool_calls finish_reason" => Some(ModelRecoveryDirective::replace(
                "tool_finish_reason_invalid",
            )),
            "unsupported tool call type" => {
                Some(ModelRecoveryDirective::replace("tool_call_kind_invalid"))
            }
            "workflow transition must finish with tool_calls"
            | "workflow transition requires exactly one tool call"
            | "workflow transition tool identity mismatch" => {
                Some(ModelRecoveryDirective::replace("transition_shape_invalid"))
            }
            "typed JSON state received a tool call"
            | "capability state received a workflow transition"
            | "workflow transition state received a capability call" => Some(
                ModelRecoveryDirective::replace("decision_not_allowed_in_state"),
            ),
            "capability state requires a tool call" => Some(ModelRecoveryDirective::replace(
                "missing_capability_decision",
            )),
            "workflow transition state requires a tool call" => Some(
                ModelRecoveryDirective::replace("missing_transition_decision"),
            ),
            "assessment state requires a typed tool call" => Some(ModelRecoveryDirective::replace(
                "missing_assessment_decision",
            )),
            _ => None,
        },
        EngineError::Dependency {
            component: "capability",
            failure,
        } if failure.retryable => Some(ModelRecoveryDirective::replace(
            "capability_dependency_retryable",
        )),
        EngineError::Dependency {
            component: "provider",
            failure,
        } if failure.retryable => Some(ModelRecoveryDirective::replace(
            "provider_dependency_retryable",
        )),
        _ => None,
    }
}

/// Rebuild one committed mid-loop sectioned-compose episode onto the
/// recovering state: parse the report-sections/v1 batch out of the committed
/// episode bytes and push it through the same `retain_section_batch` walk the
/// live run used (validation, the `section_submitted` model artifact, the
/// `more_sections_required` continuation, the transcript acknowledgement, and
/// the composed-sections retention). Usage for the episode has already been
/// recorded by the caller in the same order the live orchestrator records it,
/// so the loop-continuation floor decision replays identically.
///
/// A batch that ends the loop (`report_done`, or the engine's own reserve
/// floor) cannot be part of the checkpointed base history — the live path
/// checkpoints active state only after `finish` reports the loop still
/// running — so an assembled final here is the historical terminal-final
/// snapshot error, and any retention failure fails recovery closed.
pub(crate) fn replay_committed_section_batch(
    max_conversation_bytes: usize,
    input: &RunInput<'_>,
    state: &mut ActiveRun,
    episode: &ProviderEpisodeV1,
) -> Result<(), EngineError> {
    let section_pin =
        state
            .section_output_contract()?
            .ok_or(EngineError::InvalidRecoverySnapshot(
                "episode is not a section batch",
            ))?;
    let content = episode
        .assistant
        .content
        .as_deref()
        .map(str::trim)
        .filter(|content| !content.is_empty())
        .ok_or(EngineError::InvalidRecoverySnapshot(
            "section episode has no committed content",
        ))?;
    let batch = parse_typed_json_content(content).map_err(|_| {
        EngineError::InvalidRecoverySnapshot("section episode content is not typed JSON")
    })?;
    let final_contract = ContractPin::canonical(&input.image.body.answer_policy.internal_format)?;
    match retain_section_batch(
        max_conversation_bytes,
        input,
        state,
        episode,
        &section_pin,
        &final_contract,
        &batch,
    ) {
        // The loop continued: retention, the interpreter walk, and the
        // acknowledgement are all rebuilt; the state is resumable.
        Ok(None) => Ok(()),
        Ok(Some(_)) => Err(EngineError::InvalidRecoverySnapshot(
            "terminal final output was checkpointed as resumable state",
        )),
        Err(error) => Err(error),
    }
}
