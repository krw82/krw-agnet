//! Active-run program state: budgets, evidence ledger, accepted-action
//! frontier, kernel event application, compaction boundaries, and the
//! durable checkpoint projection. One `ActiveRun` is the mutable heart of
//! a claimed run; every mutation flows through typed interpreter
//! admissions so the checkpoint stays a faithful projection of committed
//! state.

use super::*;
use krw_agent_evidence::{AnswerSection, Claim, MAX_ANSWER_SECTIONS};

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct AcceptedActionCommitment<'a> {
    action_key: &'a str,
    capability_id: &'a str,
    input_contract: &'a str,
    input_hash: &'a ContentHash,
    sealed_input_retained: bool,
}

pub(crate) struct ActiveRun {
    pub(crate) messages: Vec<RunEngineMessage>,
    /// Child-only progressive-disclosure context. Kept out of `messages` so
    /// the parent's transcript cannot be smuggled into a bounded child, and
    /// the loaded body cannot leak back when the child returns.
    pub(crate) child_skill_context: ChildSkillContext,
    pub(crate) usage: BudgetUsage,
    pub(crate) limits: BudgetLimits,
    pub(crate) capability_calls: BTreeMap<String, u16>,
    pub(crate) completed_capabilities: BTreeSet<String>,
    pub(crate) logical_action_keys: BTreeSet<String>,
    pub(crate) action_cache: BTreeMap<String, CapabilityResult>,
    pub(crate) accepted_actions: Vec<AcceptedActionRef>,
    pub(crate) ledger: EvidenceLedger,
    pub(crate) calculations: BTreeMap<String, Calculation>,
    /// Release B: bounded judgment notes captured on the final
    /// evidence-sufficient transition. Advisory only — they ride the next
    /// compaction boundary into the composer view and never enter the
    /// evidence ledger. Deliberately not part of the recovery checkpoint:
    /// after a crash the writer falls back to re-deriving the judgment from
    /// retained facts, which is the pre-B behavior.
    pub(crate) analyst_judgment: Vec<AnalystJudgmentNote>,
    pub(crate) presentation_packs: Vec<Value>,
    /// E1 sectioned compose: report-sections/v1 batches accepted by
    /// `retain_composed_section` across the engine-owned compose→verify
    /// loop. Batches accumulate verbatim; assembly into the final answer IR
    /// happens once the loop ends (writer `report_done` or the engine's own
    /// budget floor). Checkpoint v15 projects this field as
    /// `composed_sections_hash`; replay rebuilds the accumulation from the
    /// committed section artifacts (recovery.rs), so crash-recovery
    /// equivalence holds mid-loop.
    pub(crate) composed_sections: Vec<Value>,
    pub(crate) program: Arc<ProgramRuntime>,
    pub(crate) interpreter: StateInterpreter,
    pub(crate) artifact_validator: ArtifactValidator,
    pub(crate) context_planner: Arc<ContextPlanner>,
    pub(crate) session_memory: Option<PreparedSessionMemory>,
    pub(crate) compacted_context: Option<PreparedCompactedContext>,
    pub(crate) tool_definitions: Vec<ProviderToolDefinition>,
    pub(crate) tool_schema_hash: ContentHash,
    pub(crate) prompt_receipt_hashes: Vec<ContentHash>,
    pub(crate) compaction_receipts: Vec<CompactionReceipt>,
    pub(crate) last_provider_episode_hash: Option<ContentHash>,
    pub(crate) state_trace: Vec<String>,
    pub(crate) direct_answer_retry_requested: bool,
    /// One-shot: the next non-answer decision turn retries with private
    /// thinking disabled after its predecessor truncated on `length`
    /// mid-reasoning (production INTC 2026-09-02: an assess turn burned its
    /// whole cap thinking; the generic recovery turn then emitted
    /// assess-lane deliberation as prose).
    pub(crate) decision_retry_requested: std::sync::atomic::AtomicBool,
    /// Supplemental reads the model batched into one committed episode. The
    /// statechart admits one non-research capability per decision, so the
    /// kernel drains the remainder at the next model-decision state — one
    /// capability visit each, zero extra provider turns. Transient by
    /// design: a daemon restart loses the remainder and the model re-proposes.
    pub(crate) pending_supplemental_calls: Vec<(ContentHash, PreparedCall)>,
    pub(crate) research_planner: ResearchPlanner,
    pub(crate) derived_ticker_scope: Option<DerivedTickerScope>,
    pub(crate) runtime_timings: Option<Arc<RuntimeStageTimings>>,
}

/// Scope derived from a committed, typed capability result. It is deliberately
/// small and contains only a canonical ticker set plus provenance hashes; the
/// raw feed payload stays in the encrypted action artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DerivedTickerScope {
    pub(crate) producer_capability_id: String,
    pub(crate) output_contract: String,
    pub(crate) source_hash: ContentHash,
    pub(crate) tickers: Vec<String>,
}

pub(crate) const ACTIVE_RUN_CHECKPOINT_SCHEMA_VERSION: u16 = 15;
pub(crate) const ACTIVE_RUN_CHECKPOINT_SCHEMA: &str = r#"
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "additionalProperties": false,
  "properties": {
    "accepted_actions_hash": {"type": "string"},
    "action_cache_hash": {"type": "string"},
    "calculations_hash": {"type": "string"},
    "capability_calls": {
      "additionalProperties": {"maximum": 65535, "minimum": 0, "type": "integer"},
      "type": "object"
    },
    "compacted_context_hash": {"type": ["string", "null"]},
    "compaction_receipts": {"items": {"type": "object"}, "type": "array"},
    "completed_capabilities": {"items": {"type": "string"}, "type": "array"},
    "composed_sections_hash": {"type": "string"},
    "conversation_hash": {"type": "string"},
    "derived_ticker_scope_hash": {"type": ["string", "null"]},
    "evidence_ledger_hash": {"type": "string"},
    "presentation_packs_hash": {"type": "string"},
    "interpreter": {"type": "object"},
    "last_provider_episode_hash": {"type": ["string", "null"]},
    "logical_action_keys": {"items": {"type": "string"}, "type": "array"},
    "prompt_receipt_hashes": {"items": {"type": "string"}, "type": "array"},
    "research_planner_hash": {"type": "string"},
    "schema_version": {"const": 15},
    "session_memory_hash": {"type": ["string", "null"]},
    "state_trace": {"items": {"type": "string"}, "type": "array"},
    "direct_answer_retry_requested": {"type": "boolean"},
    "tool_schema_hash": {"type": "string"},
    "usage": {"type": "object"}
  },
  "required": [
    "schema_version",
    "interpreter",
    "state_trace",
    "direct_answer_retry_requested",
    "usage",
    "capability_calls",
    "completed_capabilities",
    "logical_action_keys",
    "conversation_hash",
    "evidence_ledger_hash",
    "presentation_packs_hash",
    "composed_sections_hash",
    "calculations_hash",
    "action_cache_hash",
    "accepted_actions_hash",
    "tool_schema_hash",
    "prompt_receipt_hashes",
    "compaction_receipts",
    "last_provider_episode_hash",
    "compacted_context_hash",
    "research_planner_hash",
    "derived_ticker_scope_hash",
    "session_memory_hash"
  ],
  "type": "object"
}
"#;

pub fn active_run_checkpoint_schema_hash() -> ContentHash {
    ContentHash::sha256(ACTIVE_RUN_CHECKPOINT_SCHEMA)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ActiveRunCheckpoint {
    pub(crate) schema_version: u16,
    pub(crate) interpreter: InterpreterCheckpointV1,
    pub(crate) state_trace: Vec<String>,
    pub(crate) direct_answer_retry_requested: bool,
    pub(crate) usage: BudgetUsage,
    pub(crate) capability_calls: BTreeMap<String, u16>,
    pub(crate) completed_capabilities: BTreeSet<String>,
    pub(crate) logical_action_keys: BTreeSet<String>,
    pub(crate) conversation_hash: ContentHash,
    pub(crate) evidence_ledger_hash: ContentHash,
    pub(crate) presentation_packs_hash: ContentHash,
    /// E1 sectioned compose: content hash over the accumulated
    /// report-sections/v1 batches retained so far. Like
    /// `presentation_packs_hash`, the payload itself is rebuilt during
    /// replay from the committed section artifacts and compared here.
    pub(crate) composed_sections_hash: ContentHash,
    pub(crate) calculations_hash: ContentHash,
    pub(crate) action_cache_hash: ContentHash,
    pub(crate) accepted_actions_hash: ContentHash,
    pub(crate) tool_schema_hash: ContentHash,
    pub(crate) prompt_receipt_hashes: Vec<ContentHash>,
    pub(crate) compaction_receipts: Vec<CompactionReceipt>,
    pub(crate) last_provider_episode_hash: Option<ContentHash>,
    pub(crate) compacted_context_hash: Option<ContentHash>,
    pub(crate) research_planner_hash: ContentHash,
    pub(crate) derived_ticker_scope_hash: Option<ContentHash>,
    pub(crate) session_memory_hash: Option<ContentHash>,
}

impl fmt::Debug for ActiveRun {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ActiveRun")
            .field("message_count", &self.messages.len())
            .field("usage", &self.usage)
            .field("limits", &self.limits)
            .field("completed_capabilities", &self.completed_capabilities)
            .field("logical_action_count", &self.logical_action_keys.len())
            .field("cached_action_count", &self.action_cache.len())
            .field("accepted_action_ref_count", &self.accepted_actions.len())
            .field("evidence_count", &self.ledger.len())
            .field("calculation_count", &self.calculations.len())
            .field("presentation_pack_count", &self.presentation_packs.len())
            .field("typed_interpreter", &self.interpreter.checkpoint().ok())
            .field("session_memory", &self.session_memory)
            .field("compacted_context", &self.compacted_context)
            .field("tool_schema_hash", &self.tool_schema_hash)
            .field("prompt_receipt_count", &self.prompt_receipt_hashes.len())
            .field("compaction_receipt_count", &self.compaction_receipts.len())
            .field(
                "last_provider_episode_hash",
                &self.last_provider_episode_hash,
            )
            .field("state_trace", &self.state_trace)
            .field(
                "research_planner_hash",
                &self.research_planner.checkpoint_hash().ok(),
            )
            .field("derived_ticker_scope", &self.derived_ticker_scope)
            .finish_non_exhaustive()
    }
}

impl Drop for ActiveRun {
    fn drop(&mut self) {
        for message in &mut self.messages {
            message.scrub_sensitive();
        }
        for pack in &mut self.presentation_packs {
            scrub_json(pack);
        }
        for tool in &mut self.tool_definitions {
            tool.scrub_sensitive();
        }
    }
}

impl ActiveRun {
    pub(crate) fn new(
        limits: BudgetLimits,
        program: Arc<ProgramRuntime>,
        context_planner: Arc<ContextPlanner>,
        session_memory: Option<PreparedSessionMemory>,
        runtime_timings: Option<Arc<RuntimeStageTimings>>,
    ) -> Result<Self, EngineError> {
        let interpreter = StateInterpreter::new(program.typed_program.clone())?;
        let initial_state = program
            .state(interpreter.current_state())?
            .stable_id
            .clone();
        Ok(Self {
            messages: Vec::new(),
            child_skill_context: ChildSkillContext::default(),
            usage: BudgetUsage::default(),
            limits,
            capability_calls: BTreeMap::new(),
            completed_capabilities: BTreeSet::new(),
            logical_action_keys: BTreeSet::new(),
            action_cache: BTreeMap::new(),
            accepted_actions: Vec::new(),
            ledger: EvidenceLedger::default(),
            calculations: BTreeMap::new(),
            analyst_judgment: Vec::new(),
            presentation_packs: Vec::new(),
            composed_sections: Vec::new(),
            program,
            interpreter,
            artifact_validator: ArtifactValidator::default(),
            context_planner,
            session_memory,
            compacted_context: None,
            tool_definitions: Vec::new(),
            tool_schema_hash: ContentHash::sha256(serde_jcs::to_vec(
                &Vec::<ProviderToolDefinition>::new(),
            )?),
            prompt_receipt_hashes: Vec::new(),
            compaction_receipts: Vec::new(),
            last_provider_episode_hash: None,
            state_trace: vec![initial_state],
            direct_answer_retry_requested: false,
            decision_retry_requested: std::sync::atomic::AtomicBool::new(false),
            pending_supplemental_calls: Vec::new(),
            research_planner: ResearchPlanner::new(ScoringWeights::default())?,
            derived_ticker_scope: None,
            runtime_timings,
        })
    }

    pub(crate) fn merge_runtime_timings(&mut self) {
        let Some(timings) = &self.runtime_timings else {
            return;
        };
        let snapshot = timings.snapshot();
        self.usage.provider_queue_wait_ms = self
            .usage
            .provider_queue_wait_ms
            .saturating_add(snapshot.provider_queue_wait_ms);
        self.usage.session_memory_total_ms = self
            .usage
            .session_memory_total_ms
            .saturating_add(snapshot.session_memory_total_ms);
        self.usage.market_preflight_ms = self
            .usage
            .market_preflight_ms
            .saturating_add(snapshot.market_preflight_ms);
        self.usage.prompt_build_total_ms = self
            .usage
            .prompt_build_total_ms
            .saturating_add(snapshot.prompt_build_total_ms);
        self.usage.checkpoint_total_ms = self
            .usage
            .checkpoint_total_ms
            .saturating_add(snapshot.checkpoint_total_ms);
    }

    pub(crate) fn enter_initial_model_state(&mut self) -> Result<(), EngineError> {
        if !matches!(
            self.interpreter.current_operation()?,
            StateOperation::Builtin {
                handler: BuiltinHandler::InitializeRun,
                ..
            }
        ) {
            return Err(EngineError::InvalidStateProgram);
        }
        let payload = serde_json::json!({});
        let event = self.program.unique_transition_event(
            self.interpreter.current_state(),
            &payload,
            |_| true,
            "initial transition",
        )?;
        self.apply_builtin_artifact(
            BuiltinHandler::InitializeRun,
            &event,
            ContractPin::canonical(STATE_FACTS_V1)?,
            &payload,
        )?;
        self.require_model_state()
    }

    pub(crate) fn require_model_state(&self) -> Result<(), EngineError> {
        match self.interpreter.current_operation()? {
            StateOperation::ModelDecision { .. } => Ok(()),
            _ => Err(EngineError::WorkflowResolution {
                outcome: "model decision state",
            }),
        }
    }

    pub(crate) fn current_role_id(&self) -> Result<&str, EngineError> {
        match self.interpreter.current_operation()? {
            StateOperation::ModelDecision { role_id, .. } => Ok(role_id),
            _ => Err(EngineError::WorkflowResolution {
                outcome: "model role",
            }),
        }
    }

    pub(crate) fn current_state(&self) -> Result<&CompiledState, EngineError> {
        self.program.state(self.interpreter.current_state())
    }

    /// Return whether a compiled state can be entered again before exposing
    /// it to the provider. The typed interpreter remains the mutation
    /// authority; this is only a non-mutating admission check that prevents
    /// impossible actions and workflow events from entering the prompt.
    pub(crate) fn state_has_remaining_visit(
        &self,
        state: &CompiledState,
    ) -> Result<bool, EngineError> {
        Ok(self.interpreter.remaining_visits(&state.stable_id)? > 0)
    }

    pub(crate) fn capability_has_remaining_visit(
        &self,
        capability_id: &str,
    ) -> Result<bool, EngineError> {
        let state = self.program.capability_state(capability_id)?;
        self.state_has_remaining_visit(state)
    }

    pub(crate) fn capability_has_remaining_budget(&self, capability_id: &str) -> bool {
        if self.usage.capability_calls >= self.limits.max_capability_calls {
            return false;
        }
        self.limits
            .capability_call_limits
            .get(capability_id)
            .is_none_or(|limit| {
                self.capability_calls
                    .get(capability_id)
                    .copied()
                    .unwrap_or(0)
                    < *limit
            })
    }

    /// Filter the statically compiled capability frontier by the exact
    /// remaining statechart capacity and deployment bindings for this run.
    /// `ContextPlanner` owns the immutable image frontier; this per-run
    /// overlay owns only dynamic resource availability and is reproduced from
    /// the checkpoint on resume. A model must never be offered an external
    /// capability that the resolved deployment cannot execute.
    pub(crate) fn available_capability_ids(
        &self,
        context: &CompiledStateContext,
        image: &AgentImageManifest,
        deployment: &DeploymentBinding,
    ) -> Result<BTreeSet<String>, EngineError> {
        let mut ids = BTreeSet::new();
        for schema in context.capability_schemas.iter() {
            // Role-scoped local capabilities such as `skill.load` have no
            // workflow statechart node — they are short-circuited at dispatch
            // and never consume a state visit. When a role's compiled context
            // advertises one, treat the absent state node as always available.
            match self.capability_has_remaining_visit(&schema.capability_id) {
                Ok(remaining) => {
                    if remaining
                        && self.capability_has_remaining_budget(&schema.capability_id)
                        && capability_has_deployment_binding(
                            image,
                            deployment,
                            &schema.capability_id,
                        )?
                    {
                        ids.insert(schema.capability_id.clone());
                    }
                }
                Err(EngineError::CapabilityStateMappingUnavailable) => {
                    ids.insert(schema.capability_id.clone());
                }
                Err(error) => return Err(error),
            }
        }
        Ok(ids)
    }

    /// Return only transitions that a provider may select through the local
    /// `krw_agent_transition` tool. Capability-targeted edges deliberately do
    /// not appear here: the provider selects those by calling the advertised
    /// capability itself, and `route_to_capability` resolves the edge from the
    /// validated call. Mixing both control paths lets a model select a
    /// capability edge as a transition, which leaves the kernel outside a
    /// model-decision state after it records the event.
    pub(crate) fn available_model_transition_events(
        &self,
        image: &AgentImageManifest,
        request: &RunRequest,
    ) -> Result<Vec<String>, EngineError> {
        let current_numeric_id = self.current_state()?.numeric_id;
        let mut events = BTreeSet::new();
        for transition in self
            .program
            .workflow
            .transitions
            .iter()
            .filter(|transition| transition.from == current_numeric_id)
        {
            let next = self
                .program
                .workflow
                .states
                .iter()
                .find(|state| state.numeric_id == transition.to)
                .ok_or(EngineError::InvalidStateProgram)?;
            if !matches!(next.operation, StateOperation::ModelDecision { .. })
                || !self.state_has_remaining_visit(next)?
            {
                continue;
            }
            // These edges are entirely kernel-owned budget recovery, never a
            // model choice. Advertising them let a planner skip from orientation
            // to Markdown before a substantive filing read, producing a
            // polished but evidence-free "not found" answer. The kernel
            // invokes each edge only after it has admitted substantive evidence
            // or exhausted the bounded repair budget.
            if transition.event == "output_budget_reserved"
                || transition.event == "proposal_unrecoverable"
            {
                continue;
            }
            let facts = kernel_workflow_facts(image, request, &transition.event)?;
            if !transition.guard.matches(&facts)
                || !self.event_has_remaining_target(&transition.event, &facts)?
            {
                continue;
            }
            events.insert(transition.event.clone());
        }
        Ok(events.into_iter().collect())
    }

    /// Whether a provider-selected workflow event still has a unique legal
    /// target at this exact checkpoint. This checks only statechart capacity;
    /// provider-visible tool availability is enforced separately by the
    /// dynamic tool frontier.
    pub(crate) fn event_has_remaining_target(
        &self,
        event: &str,
        facts: &Value,
    ) -> Result<bool, EngineError> {
        let current = self.current_state()?;
        let mut matching = 0_usize;
        let mut available = 0_usize;
        for transition in self
            .program
            .workflow
            .transitions
            .iter()
            .filter(|transition| {
                transition.from == current.numeric_id
                    && transition.event == event
                    && transition.guard.matches(facts)
            })
        {
            matching = matching
                .checked_add(1)
                .ok_or(EngineError::CounterOverflow("workflow_transition_matches"))?;
            let target = self
                .program
                .workflow
                .states
                .iter()
                .find(|state| state.numeric_id == transition.to)
                .ok_or(EngineError::InvalidStateProgram)?;
            if self.state_has_remaining_visit(target)? {
                available = available
                    .checked_add(1)
                    .ok_or(EngineError::CounterOverflow(
                        "workflow_available_transitions",
                    ))?;
            }
        }
        Ok(matching == 1 && available == 1)
    }

    /// Resolve a kernel-emitted transition only when its destination can still
    /// be entered in this exact run. Model events take the same admission path
    /// in `handle_model_event`; deterministic capability completion must not
    /// bypass it and surface an interpreter-level visit-limit failure.
    pub(crate) fn unique_available_transition_event(
        &self,
        expected_event: Option<&str>,
        facts: &Value,
        target_matches: impl Fn(&CompiledState) -> bool,
        outcome: &'static str,
    ) -> Result<String, EngineError> {
        let current_numeric_id = self.current_state()?.numeric_id;
        let mut candidates = Vec::new();
        for transition in self
            .program
            .workflow
            .transitions
            .iter()
            .filter(|transition| {
                transition.from == current_numeric_id
                    && expected_event.is_none_or(|event| transition.event == event)
                    && transition.guard.matches(facts)
            })
        {
            let target = self
                .program
                .workflow
                .states
                .iter()
                .find(|state| state.numeric_id == transition.to)
                .ok_or(EngineError::InvalidStateProgram)?;
            if target_matches(target) && self.state_has_remaining_visit(target)? {
                candidates.push(transition.event.clone());
            }
        }
        match candidates.as_slice() {
            [event] => Ok(event.clone()),
            _ => Err(EngineError::WorkflowResolution { outcome }),
        }
    }

    pub(crate) fn current_operation_emits_answer(
        &self,
        image: &AgentImageManifest,
    ) -> Result<bool, EngineError> {
        let answer_contract = ContractPin::canonical(&image.body.answer_policy.internal_format)?;
        // E1 sectioned compose: a compose state whose declared output grammar
        // is the report-sections/v1 batch contract emits the answer surface
        // one section turn at a time. Treating those turns as answer turns is
        // what keeps every output-reserve trigger off the loop: the section
        // composer draws the full remaining output budget (not the
        // reserve-reduced research lane) and the answer-turn exclusions in
        // `should_finalize_for_output_reserve` hold mid-loop.
        let section_contract = ContractPin::canonical(REPORT_SECTIONS_V1)?;
        Ok(matches!(
            self.interpreter.current_operation()?,
            StateOperation::ModelDecision {
                output_mode: ModelOutputMode::TypedJson | ModelOutputMode::Markdown,
                output_contracts,
                ..
            } if output_contracts.contains(&answer_contract)
                || output_contracts.contains(&section_contract)
        ))
    }

    /// The per-state sectioned compose grammar: `Some(REPORT_SECTIONS_V1)`
    /// when the current compose state declares the report-sections/v1 batch
    /// contract as its model-output grammar, `None` for every ordinary
    /// compose/product state.
    pub(crate) fn section_output_contract(&self) -> Result<Option<ContractPin>, EngineError> {
        let section_contract = ContractPin::canonical(REPORT_SECTIONS_V1)?;
        Ok(matches!(
            self.interpreter.current_operation()?,
            StateOperation::ModelDecision { output_contracts, .. }
                if output_contracts.contains(&section_contract)
        )
        .then_some(section_contract))
    }

    pub(crate) fn lifecycle_stage_for_checkpoint(
        &self,
    ) -> Result<Option<RunLifecycleStage>, EngineError> {
        let is_answer_operation = matches!(
            self.interpreter.current_operation()?,
            StateOperation::ModelDecision {
                output_mode: ModelOutputMode::TypedJson | ModelOutputMode::Markdown,
                output_contracts,
                ..
            } if output_contracts.contains(&self.program.answer_contract)
        );
        Ok(is_answer_operation.then_some(RunLifecycleStage::Composing))
    }

    pub(crate) fn current_model_output_mode(&self) -> Result<ModelOutputMode, EngineError> {
        match self.interpreter.current_operation()? {
            StateOperation::ModelDecision { output_mode, .. } => Ok(*output_mode),
            _ => Err(EngineError::WorkflowResolution {
                outcome: "model output mode",
            }),
        }
    }

    pub(crate) fn remaining_output_tokens(&self) -> Result<u32, EngineError> {
        // A provider may report completion tokens slightly above the declared
        // allowance (see `record_provider_usage`). Saturate so an over-report
        // behaves exactly like being at the limit — zero remaining, i.e. the
        // existing `NoRemainingOutputBudget` path — instead of a
        // `CounterOverflow` that would bypass the answer-always ledger
        // fallback after the response itself was already accepted.
        Ok(self
            .limits
            .max_output_tokens
            .saturating_sub(self.usage.output_tokens))
    }

    /// Preserve the answer-producing turn before cumulative prompt reuse can
    /// consume the full run input allowance. This is intentionally a soft
    /// *research* stop, not a new model-visible rule: after substantive
    /// evidence exists the kernel follows the image's existing composition
    /// edge and the composer receives the compacted, already-admitted facts.
    ///
    /// The budget is charged from provider-reported input usage only after a
    /// turn completes, so this check runs at every settled boundary. Keeping
    /// one fifth of the envelope available leaves room for the final answer
    /// prompt without suppressing the evidence-gathering turns that came
    /// before it.
    pub(crate) fn input_budget_answer_reserve_reached(&self) -> bool {
        const RESERVE_NUMERATOR: u64 = 4;
        const RESERVE_DENOMINATOR: u64 = 5;

        u64::from(self.usage.input_tokens) * RESERVE_DENOMINATOR
            >= u64::from(self.limits.max_input_tokens) * RESERVE_NUMERATOR
    }

    pub(crate) fn should_finalize_for_output_reserve(
        &self,
        image: &AgentImageManifest,
    ) -> Result<bool, EngineError> {
        if self.input_budget_answer_reserve_reached()
            && !self.current_operation_emits_answer(image)?
        {
            return Ok(true);
        }

        if let Some(reserve) =
            image.effective_final_output_reserve_tokens(&self.program.workflow.id)?
        {
            let minimum_research_turn = image
                .body
                .answer_policy
                .minimum_research_turn_tokens
                .unwrap_or_default();
            let finalization_threshold = reserve
                .checked_add(minimum_research_turn)
                .ok_or(EngineError::CounterOverflow("final_output_reserve"))?;
            if self.remaining_output_tokens()? <= finalization_threshold {
                return Ok(true);
            }
        }

        // A workflow can legitimately spend several model turns selecting and
        // checking evidence. Once only one provider turn remains, preserve it
        // for the answer-producing composer instead of attempting another
        // research/assessment decision that can only end in a budget failure.
        // This is an image-declared fallback, never a kernel-invented answer:
        // it activates only when the current state is not already a composer.
        let remaining_provider_turns = self
            .limits
            .max_provider_turns
            .saturating_sub(self.usage.provider_turns);
        if remaining_provider_turns == 1 && !self.current_operation_emits_answer(image)? {
            return Ok(true);
        }

        // A capability result can be the last research result that the image
        // already allows.  The following ingest builtin has no provider cost,
        // but its normal `ingested` destination can be an exhausted analyst
        // state.  Do not turn a successfully retrieved final evidence batch
        // into a workflow failure or invent another analyst turn: use the
        // image-declared composition fallback that normally protects the
        // answer token reserve.
        let answer_contract = ContractPin::canonical(&image.body.answer_policy.internal_format)?;
        finalize_after_exhausted_ingest_successor(
            &self.program.workflow,
            self.current_state()?.numeric_id,
            |state| self.state_has_remaining_visit(state),
            &answer_contract,
        )
    }

    /// Orientation and market snapshots are deliberately retained as
    /// unverified context, but they are not filing research evidence. They
    /// must never unlock the output-reserve escape hatch before a real filing
    /// read has been admitted.
    pub(crate) fn has_substantive_research_evidence(&self) -> bool {
        self.ledger
            .iter()
            .any(|(_evidence_id, record)| record.directness != Directness::Unverified)
    }

    pub(crate) fn apply_typed_artifact(
        &mut self,
        event: &str,
        declared_contract: ContractPin,
        payload: &Value,
        producer: ArtifactProducer,
    ) -> Result<(), EngineError> {
        let state_id = self.interpreter.current_state().to_owned();
        let identity = StateIdentity {
            image_hash: self.program.image_hash.clone(),
            workflow_id: self.program.workflow.id.clone(),
            state_id,
        };
        let operation = self.interpreter.current_operation()?.clone();
        let lineage_refs = if let Some(artifact) = self.interpreter.last_artifact() {
            vec![ArtifactLineageRef {
                artifact_hash: artifact.artifact_hash()?,
                relation: LineageRelation::PriorState,
            }]
        } else {
            Vec::new()
        };
        let artifact = self.artifact_validator.seal_for_operation(
            &identity,
            &operation,
            StateArtifactDraft {
                producer,
                event: event.to_owned(),
                declared_contract,
                payload: payload.clone(),
                lineage_refs,
            },
        )?;

        // Apply to a clone so a rejected transition cannot partially mutate
        // the live run. The typed interpreter is the sole workflow authority.
        let mut typed = self.interpreter.clone();
        typed.apply_artifact(&self.artifact_validator, &artifact)?;
        let entered = self.program.state(typed.current_state())?.stable_id.clone();
        self.interpreter = typed;
        self.state_trace.push(entered);
        Ok(())
    }

    pub(crate) fn apply_builtin_artifact(
        &mut self,
        handler: BuiltinHandler,
        event: &str,
        contract: ContractPin,
        payload: &Value,
    ) -> Result<(), EngineError> {
        self.apply_typed_artifact(
            event,
            contract,
            payload,
            ArtifactProducer::Builtin { handler },
        )
    }

    pub(crate) fn apply_model_artifact(
        &mut self,
        event: &str,
        contract: ContractPin,
        payload: &Value,
        provider_episode_hash: ContentHash,
    ) -> Result<(), EngineError> {
        let role_id = match self.interpreter.current_operation()? {
            StateOperation::ModelDecision { role_id, .. } => role_id.clone(),
            _ => {
                return Err(EngineError::WorkflowResolution {
                    outcome: "model artifact source",
                });
            }
        };
        self.apply_typed_artifact(
            event,
            contract,
            payload,
            ArtifactProducer::Model {
                role_id,
                provider_episode_hash,
            },
        )
    }

    pub(crate) fn apply_kernel_artifact(
        &mut self,
        reason: KernelArtifactReason,
        event: &str,
        payload: &Value,
        provider_episode_hash: ContentHash,
    ) -> Result<(), EngineError> {
        self.apply_typed_artifact(
            event,
            ContractPin::canonical(STATE_FACTS_V1)?,
            payload,
            ArtifactProducer::Kernel {
                reason,
                provider_episode_hash,
            },
        )
    }

    pub(crate) fn apply_capability_artifact(
        &mut self,
        capability_id: &str,
        event: &str,
        contract: ContractPin,
        payload: &Value,
        action_key: &str,
        action_receipt_hash: ContentHash,
    ) -> Result<(), EngineError> {
        self.apply_typed_artifact(
            event,
            contract,
            payload,
            ArtifactProducer::Capability {
                capability_id: capability_id.to_owned(),
                action_key: ContentHash::parse(action_key.to_owned())?,
                action_receipt_hash,
            },
        )
    }

    pub(crate) fn handle_model_event(
        &mut self,
        event: &str,
        facts: &Value,
        provider_episode_hash: ContentHash,
    ) -> Result<(), EngineError> {
        let current = self.current_state()?;
        if !matches!(current.operation, StateOperation::ModelDecision { .. }) || event.is_empty() {
            return Err(EngineError::InvalidWorkflowControl);
        }
        if !self.event_has_remaining_target(event, facts)? {
            // A provider must never be allowed to turn a stale or exhausted
            // workflow option into an interpreter-level visit-limit failure.
            // The exact model text remains in the encrypted episode; this is
            // only a typed admission failure at the control boundary.
            return Err(EngineError::InvalidWorkflowControl);
        }
        self.apply_model_artifact(
            event,
            ContractPin::canonical(STATE_FACTS_V1)?,
            facts,
            provider_episode_hash,
        )?;
        match self.current_state()?.terminal {
            Some(TerminalDisposition::Failed) => Err(EngineError::WorkflowTerminated("failed")),
            Some(TerminalDisposition::Stopped) => Err(EngineError::WorkflowTerminated("stopped")),
            Some(TerminalDisposition::Succeeded) => Err(EngineError::InvalidWorkflowControl),
            None => self.require_model_state(),
        }
    }

    /// Upper bound on kernel-drained supplemental reads per committed
    /// episode. The statechart still enforces its own per-capability visit
    /// and action limits on every drained call.
    pub(crate) const MAX_SUPPLEMENTAL_BATCH: usize = 4;

    /// Pop the next queued supplemental read, but only while the workflow is
    /// parked at a model-decision state (each drained call routes through the
    /// same single-capability transition the model would have taken). Calls
    /// whose frontier expired are dropped; a route failure clears the queue —
    /// the workflow has moved somewhere the remaining reads cannot follow.
    pub(crate) fn take_pending_supplemental(
        &mut self,
    ) -> Result<Option<(ContentHash, PreparedCall)>, EngineError> {
        if !matches!(
            self.interpreter.current_operation()?,
            StateOperation::ModelDecision { .. }
        ) {
            return Ok(None);
        }
        while let Some((episode_hash, call)) = self.pending_supplemental_calls.pop() {
            if !self.capability_has_remaining_visit(&call.capability.id)? {
                continue;
            }
            return Ok(Some((episode_hash, call)));
        }
        Ok(None)
    }

    pub(crate) fn route_to_capability(
        &mut self,
        call: &PreparedCall,
        provider_episode_hash: ContentHash,
    ) -> Result<(), EngineError> {
        if !matches!(
            self.interpreter.current_operation()?,
            StateOperation::ModelDecision { .. }
        ) {
            return Err(EngineError::WorkflowResolution {
                outcome: "capability proposal source",
            });
        }
        let target_numeric = self
            .program
            .capability_state(&call.capability.id)?
            .numeric_id;
        let contract = call.model_input_contract.clone();
        let direct = self.program.unique_transition_event(
            self.interpreter.current_state(),
            &call.proposed_arguments,
            |state| state.numeric_id == target_numeric,
            "direct capability proposal",
        );
        if let Ok(event) = direct {
            self.apply_model_artifact(
                &event,
                contract,
                &call.proposed_arguments,
                provider_episode_hash,
            )?;
            return Ok(());
        }

        let validation_event = self.program.unique_transition_event(
            self.interpreter.current_state(),
            &call.proposed_arguments,
            |state| {
                matches!(
                    state.operation,
                    StateOperation::Builtin {
                        handler: BuiltinHandler::ValidateArtifact,
                        ..
                    }
                )
            },
            "proposal validation",
        )?;
        self.apply_model_artifact(
            &validation_event,
            contract.clone(),
            &call.proposed_arguments,
            provider_episode_hash,
        )?;
        let dispatch_event = self.program.unique_transition_event(
            self.interpreter.current_state(),
            &call.proposed_arguments,
            |state| state.numeric_id == target_numeric,
            "validated capability",
        )?;
        self.apply_builtin_artifact(
            BuiltinHandler::ValidateArtifact,
            &dispatch_event,
            contract,
            &call.proposed_arguments,
        )?;
        match self.interpreter.current_operation()? {
            StateOperation::CapabilityAction { capability_id, .. }
                if capability_id == &call.capability.id =>
            {
                Ok(())
            }
            _ => Err(EngineError::WorkflowResolution {
                outcome: "validated capability boundary",
            }),
        }
    }

    pub(crate) fn complete_capability(
        &mut self,
        image: &AgentImageManifest,
        call: &PreparedCall,
        result: &CapabilityResult,
        action_receipt_hash: ContentHash,
    ) -> Result<(), EngineError> {
        if !matches!(
            self.interpreter.current_operation()?,
            StateOperation::CapabilityAction { capability_id, .. }
                if capability_id == &call.capability.id
        ) {
            return Err(EngineError::WorkflowResolution {
                outcome: "capability completion",
            });
        }
        let correction_required = result
            .provider_content
            .get("status")
            .and_then(Value::as_str)
            == Some("input_correction_required")
            || result
                .provider_content
                .get("violations")
                .and_then(Value::as_array)
                .is_some_and(|violations| !violations.is_empty());
        let preliminary_correction_facts = serde_json::json!({
            "correction_required": correction_required,
            "answerability": result.answerability,
        });
        let correction_recovery_available = correction_required
            && self
                .unique_available_transition_event(
                    Some("correction_required"),
                    &preliminary_correction_facts,
                    |state| matches!(state.operation, StateOperation::ModelDecision { .. }),
                    "capability correction recovery",
                )
                .is_ok();
        let transition_facts = serde_json::json!({
            "correction_required": correction_required,
            "correction_recovery_available": correction_recovery_available,
            "answerability": result.answerability,
        });
        let event = if correction_required && !correction_recovery_available {
            self.unique_available_transition_event(
                Some("correction_unresolved"),
                &transition_facts,
                |state| matches!(state.operation, StateOperation::ModelDecision { .. }),
                "capability correction unresolved",
            )?
        } else {
            self.unique_available_transition_event(
                correction_required.then_some("correction_required"),
                &transition_facts,
                |state| {
                    if correction_required {
                        matches!(state.operation, StateOperation::ModelDecision { .. })
                    } else {
                        matches!(
                            state.operation,
                            StateOperation::Builtin {
                                handler: BuiltinHandler::IngestEvidence,
                                ..
                            }
                        )
                    }
                },
                "capability result",
            )?
        };
        let normalized_contract = call
            .contracts
            .outputs
            .iter()
            .find(|contract| contract.id == NORMALIZED_CAPABILITY_RESULT_V1)
            .ok_or_else(|| {
                EngineError::MissingNormalizedOutputContract(call.capability.id.clone())
            })?;
        let mut result_without_presentation = result.clone();
        result_without_presentation.presentation = None;
        let result_payload = serde_json::to_value(&result_without_presentation)?;
        self.apply_capability_artifact(
            &call.capability.id,
            &event,
            ContractPin {
                id: normalized_contract.id.clone(),
                content_hash: normalized_contract.content_hash.clone(),
            },
            &result_payload,
            &call.action_key,
            action_receipt_hash,
        )?;
        if correction_required {
            return self.require_model_state();
        }
        let output_budget_reserved = self.should_finalize_for_output_reserve(image)?
            && self.has_substantive_research_evidence();
        let facts = serde_json::json!({
            "ingested": true,
            "answerability": result.answerability,
            "output_budget_reserved": output_budget_reserved,
        });
        let next = if output_budget_reserved {
            let answer_contract =
                ContractPin::canonical(&image.body.answer_policy.internal_format)?;
            self.unique_available_transition_event(
                Some("output_budget_reserved"),
                &facts,
                |state| {
                    matches!(
                        &state.operation,
                        StateOperation::ModelDecision {
                            output_mode: ModelOutputMode::TypedJson | ModelOutputMode::Markdown,
                            output_contracts,
                            ..
                        } if output_contracts.contains(&answer_contract)
                    )
                },
                "output reserve finalization",
            )?
        } else {
            self.unique_available_transition_event(
                Some("ingested"),
                &facts,
                |state| matches!(state.operation, StateOperation::ModelDecision { .. }),
                "evidence ingested",
            )?
        };
        self.apply_builtin_artifact(
            BuiltinHandler::IngestEvidence,
            &next,
            ContractPin::canonical(STATE_FACTS_V1)?,
            &facts,
        )?;
        self.require_model_state()
    }

    /// When an otherwise recoverable model decision has consumed the final
    /// viable research turn, do not spend the reserved answer budget on a
    /// doomed correction loop.  This is deliberately narrower than normal
    /// recovery: it requires admitted evidence and an image-declared edge to
    /// an answer-producing compose state.
    pub(crate) fn finalize_for_output_reserve(
        &mut self,
        image: &AgentImageManifest,
        provider_episode_hash: &ContentHash,
    ) -> Result<bool, EngineError> {
        if !self.should_finalize_for_output_reserve(image)? {
            return Ok(false);
        }
        self.finalize_for_answer_phase_on_budget_boundary(image, provider_episode_hash, false)
    }

    /// Reserve the answer-producing phase when the next research decision
    /// cannot meet the provider's minimum thinking request size. Unlike the
    /// normal reserve path this intentionally permits an evidence-poor
    /// composition: the alternative is a local request-construction failure
    /// and no answer at all. The image must still explicitly expose an
    /// `output_budget_reserved` edge to an answer-producing state.
    pub(crate) fn finalize_for_thinking_floor_before_next_turn(
        &mut self,
        input: &RunInput<'_>,
    ) -> Result<bool, EngineError> {
        if self.current_operation_emits_answer(input.image)?
            || !thinking_turn_is_below_provider_minimum(input, self)?
        {
            return Ok(false);
        }
        let Some(provider_episode_hash) = self.last_provider_episode_hash.clone() else {
            return Ok(false);
        };
        self.finalize_for_answer_phase_on_budget_boundary(input.image, &provider_episode_hash, true)
    }

    pub(crate) fn finalize_for_answer_phase_on_budget_boundary(
        &mut self,
        image: &AgentImageManifest,
        provider_episode_hash: &ContentHash,
        allow_without_substantive_evidence: bool,
    ) -> Result<bool, EngineError> {
        // E1 sectioned compose: a section turn IS an answer turn, so the
        // loop's only budget termination is the engine-owned
        // `section_loop_may_continue` check. This immunity is structural and
        // engine-side — it must not depend on the image declining to declare
        // an `output_budget_reserved` edge from a sectioned compose state,
        // because taking that edge mid-loop would strand the retained
        // batches in a state whose composer no longer accepts section
        // batches.
        if self.section_output_contract()?.is_some() {
            return Ok(false);
        }
        let input_budget_reserved = self.input_budget_answer_reserve_reached();
        let substantive_evidence_available = self.has_substantive_research_evidence();
        if !substantive_evidence_available && !allow_without_substantive_evidence {
            return Ok(false);
        }
        if !matches!(
            self.interpreter.current_operation()?,
            StateOperation::ModelDecision { .. }
        ) {
            return Ok(false);
        }
        let current = self.current_state()?;
        let declares_fallback = self.program.workflow.transitions.iter().any(|transition| {
            transition.from == current.numeric_id && transition.event == "output_budget_reserved"
        });
        if !declares_fallback {
            return Ok(false);
        }
        let answer_contract = ContractPin::canonical(&image.body.answer_policy.internal_format)?;
        let facts = serde_json::json!({
            "output_budget_reserved": true,
            "input_budget_reserved": input_budget_reserved,
            "admitted_evidence_available": substantive_evidence_available,
            "thinking_floor_recovery": allow_without_substantive_evidence,
        });
        let event = self.unique_available_transition_event(
            Some("output_budget_reserved"),
            &facts,
            |state| {
                matches!(
                    &state.operation,
                    StateOperation::ModelDecision {
                        output_mode: ModelOutputMode::TypedJson | ModelOutputMode::Markdown,
                        output_contracts,
                        ..
                    } if output_contracts.contains(&answer_contract)
                )
            },
            "output reserve recovery finalization",
        )?;
        self.apply_kernel_artifact(
            if input_budget_reserved {
                KernelArtifactReason::InputBudgetReserved
            } else {
                KernelArtifactReason::OutputBudgetReserved
            },
            &event,
            &facts,
            provider_episode_hash.clone(),
        )?;
        self.require_model_state()?;
        Ok(true)
    }

    pub(crate) fn finalize_for_output_reserve_before_next_turn(
        &mut self,
        image: &AgentImageManifest,
    ) -> Result<bool, EngineError> {
        let Some(provider_episode_hash) = self.last_provider_episode_hash.clone() else {
            return Ok(false);
        };
        self.finalize_for_output_reserve(image, &provider_episode_hash)
    }

    /// A second context-plan proposal can be semantically valid yet have no
    /// room in the immutable root `SearchPlan`.  This is a kernel-known
    /// physical capacity boundary, not a question-specific model mistake.
    /// When the workflow explicitly offers a stop edge to an answer-producing
    /// state, preserve the already admitted evidence and finish the run rather
    /// than spending the final repair turns on a plan that can never fit.
    pub(crate) fn stop_at_append_context_plan_capacity(
        &mut self,
        image: &AgentImageManifest,
        episode: &ProviderEpisodeV1,
        provider_episode_hash: ContentHash,
    ) -> Result<bool, EngineError> {
        let [call] = episode.assistant.tool_calls.as_slice() else {
            return Ok(false);
        };
        if !is_append_context_plan_provider_tool(call.function.name.as_str()) {
            return Ok(false);
        }
        if !matches!(
            self.interpreter.current_operation()?,
            StateOperation::ModelDecision { .. }
        ) {
            return Ok(false);
        }

        let facts = serde_json::json!({
            "stop_reason": "context_plan_capacity_reached",
            "admitted_evidence_available": !self.ledger.is_empty(),
        });
        if !self.event_has_remaining_target("no_positive_value_action", &facts)? {
            return Ok(false);
        }
        let answer_contract = ContractPin::canonical(&image.body.answer_policy.internal_format)?;
        let event = match self.unique_available_transition_event(
            Some("no_positive_value_action"),
            &facts,
            |state| {
                matches!(
                    &state.operation,
                    StateOperation::ModelDecision {
                        output_mode: ModelOutputMode::TypedJson | ModelOutputMode::Markdown,
                        output_contracts,
                        ..
                    } if output_contracts.contains(&answer_contract)
                )
            },
            "append context capacity finalization",
        ) {
            Ok(event) => event,
            Err(EngineError::WorkflowResolution { .. }) => return Ok(false),
            Err(error) => return Err(error),
        };

        self.append_assistant(episode);
        self.append_tool_result(
            &call.id,
            &serde_json::json!({
                "schema_version": 1,
                "status": "not_dispatched",
                "reason_code": "no_positive_value_action",
                "stop_reason": "context_plan_capacity_reached",
                "contains_evidence": false,
            }),
        )?;
        self.apply_kernel_artifact(
            KernelArtifactReason::RejectedModelCapabilityProposal,
            &event,
            &facts,
            provider_episode_hash,
        )?;
        self.require_model_state()?;
        Ok(true)
    }

    /// Terminal-shape companion to `stop_at_append_context_plan_capacity`.
    /// When the bounded repair budget is exhausted while the planner keeps
    /// producing rejected decisions — for example a feed event whose company
    /// sits outside the covered corpus, so every proposal is scope-rejected —
    /// a workflow that declares a `proposal_unrecoverable` edge stays alive:
    /// the kernel moves to the assessment lane, which owns the qualified
    /// stop edges into composition and the feed premise the run already
    /// ingested.
    pub(crate) fn stop_at_proposal_repair_exhaustion(
        &mut self,
        episode: &ProviderEpisodeV1,
        provider_episode_hash: ContentHash,
    ) -> Result<bool, EngineError> {
        if !matches!(
            self.interpreter.current_operation()?,
            StateOperation::ModelDecision { .. }
        ) {
            return Ok(false);
        }
        // A multi-call episode cannot be acknowledged with one receipt; keep
        // the original failure for those.
        if episode.assistant.tool_calls.len() > 1 {
            return Ok(false);
        }
        let facts = serde_json::json!({
            "stop_reason": "proposal_repair_budget_exhausted",
            "admitted_evidence_available": !self.ledger.is_empty(),
        });
        if !self.event_has_remaining_target("proposal_unrecoverable", &facts)? {
            return Ok(false);
        }
        let event = match self.unique_available_transition_event(
            Some("proposal_unrecoverable"),
            &facts,
            |state| matches!(&state.operation, StateOperation::ModelDecision { .. }),
            "proposal repair exhaustion stop",
        ) {
            Ok(event) => event,
            Err(EngineError::WorkflowResolution { .. }) => return Ok(false),
            Err(error) => return Err(error),
        };
        self.append_assistant(episode);
        if let Some(call) = episode.assistant.tool_calls.first() {
            self.append_tool_result(
                &call.id,
                &serde_json::json!({
                    "schema_version": 1,
                    "status": "not_dispatched",
                    "reason_code": "proposal_repair_budget_exhausted",
                    "contains_evidence": false,
                }),
            )?;
        }
        self.apply_kernel_artifact(
            KernelArtifactReason::RejectedModelCapabilityProposal,
            &event,
            &facts,
            provider_episode_hash,
        )?;
        self.require_model_state()?;
        Ok(true)
    }

    /// Reserve one globally bounded model-output repair. Both final-output
    /// repair and model-visible recovery consume the same execution budget, so a
    /// model cannot turn independent recovery lanes into an unbounded dialogue.
    pub(crate) fn reserve_repair(&mut self) -> Result<bool, EngineError> {
        if self.usage.repairs >= self.limits.max_repairs {
            return Ok(false);
        }
        self.usage.repairs = self
            .usage
            .repairs
            .checked_add(1)
            .ok_or(EngineError::CounterOverflow("repairs"))?;
        self.check_budget()?;
        Ok(true)
    }

    /// Return one closed recovery result to the model without changing the
    /// workflow state. This is the common agent loop: a model may revise an
    /// invalid decision, choose another advertised tool, or finish with the
    /// evidence already present. The kernel never infers the research choice
    /// on its behalf.
    pub(crate) fn recover_model_decision(
        &mut self,
        image: &AgentImageManifest,
        episode: &ProviderEpisodeV1,
        directive: ModelRecoveryDirective,
    ) -> Result<bool, EngineError> {
        let episode_hash = ContentHash::sha256(serde_jcs::to_vec(episode)?);
        if self.finalize_for_output_reserve(image, &episode_hash)? {
            return Ok(true);
        }
        if !matches!(
            self.interpreter.current_operation()?,
            StateOperation::ModelDecision { .. }
        ) {
            return Ok(false);
        }
        if !self.reserve_repair()? {
            // The bounded repair budget is exhausted. A workflow that
            // declares a kernel-owned `proposal_unrecoverable` edge can still
            // keep this run on its bounded-answer path instead of failing.
            return self.stop_at_proposal_repair_exhaustion(episode, episode_hash);
        }

        let envelope = self.model_recovery_envelope(directive)?;
        if self.can_acknowledge_recovery_with_tool_results(episode) {
            self.append_assistant(episode);
            for call in &episode.assistant.tool_calls {
                self.append_tool_result(&call.id, &envelope)?;
            }
        } else {
            // Do not replay an unadvertised or malformed tool call. A fresh
            // kernel user message preserves the valid transcript and gives
            // Flash a closed correction target. When a diagnostic detail is
            // present, the message explicitly tells the model how to read it.
            let guidance = if envelope.get("detail").is_some() {
                "KRW kernel rejected the preceding decision. Read the recovery result's \
                 detail.field to find where the error is, detail.offending_value for what \
                 was wrong, and detail.valid_alternatives for what you may use instead. \
                 Fix the error and resubmit."
            } else {
                "KRW kernel did not execute the preceding decision. Continue the current \
                 research using only the currently advertised output mode and tools."
            };
            self.messages.push(RunEngineMessage::user(format!(
                "{guidance} Recovery result: {}",
                serde_jcs::to_string(&envelope)?
            )));
        }
        self.require_model_state()?;
        Ok(true)
    }

    /// Recovery path for a transient provider dependency failure (e.g. a
    /// malformed SSE frame from `DeepSeek` under load). Unlike
    /// [`recover_model_decision`](Self::recover_model_decision), there is no
    /// episode to acknowledge or replay — the provider call itself failed
    /// before any model output was produced. We consume the same bounded
    /// repair budget and inject a fresh user message so the loop re-enters a
    /// provider turn with the identical request.
    pub(crate) fn recover_provider_decision(
        &mut self,
        image: &AgentImageManifest,
        directive: ModelRecoveryDirective,
    ) -> Result<bool, EngineError> {
        if !matches!(
            self.interpreter.current_operation()?,
            StateOperation::ModelDecision { .. }
        ) || !self.reserve_repair()?
        {
            return Ok(false);
        }
        let _ = image;
        let envelope = self.model_recovery_envelope(directive)?;
        self.messages.push(RunEngineMessage::user(format!(
            "KRW kernel could not obtain a provider response for the preceding turn \
             (transient dependency failure). Re-emit the same decision. Recovery result: {}",
            serde_jcs::to_string(&envelope)?
        )));
        self.require_model_state()?;
        Ok(true)
    }

    pub(crate) fn model_recovery_envelope(
        &self,
        directive: ModelRecoveryDirective,
    ) -> Result<Value, EngineError> {
        let allowed_actions = match self.current_model_output_mode()? {
            ModelOutputMode::CapabilityCall => vec!["revise_research"],
            ModelOutputMode::WorkflowTransition => vec!["select_allowed_transition"],
            ModelOutputMode::CapabilityOrWorkflowTransition => {
                vec!["revise_research", "select_allowed_transition"]
            }
            ModelOutputMode::TypedJson => vec!["repair_output"],
            ModelOutputMode::Markdown => vec!["continue_markdown"],
        };
        Ok(serde_json::to_value(RecoveryEnvelopeV1 {
            schema_version: 1,
            status: "recovery_required",
            class: "model_correctable",
            reason_code: directive.reason_code,
            repair_mode: directive.repair_mode,
            allowed_actions,
            // `reserve_repair` has already incremented usage.repairs by 1 at this
            // point, so the remaining budget is what is left after this turn.
            repairs_remaining: self.limits.max_repairs.saturating_sub(self.usage.repairs),
            contains_evidence: false,
            detail: directive.detail,
        })?)
    }

    pub(crate) fn can_acknowledge_recovery_with_tool_results(
        &self,
        episode: &ProviderEpisodeV1,
    ) -> bool {
        let mut call_ids = BTreeSet::new();
        !episode.assistant.tool_calls.is_empty()
            && episode.assistant.tool_calls.iter().all(|call| {
                call.kind == ToolCallKind::Function
                    && !call.id.is_empty()
                    && call_ids.insert(call.id.as_str())
                    && self
                        .tool_definitions
                        .iter()
                        .any(|definition| definition.name() == call.function.name.as_str())
            })
    }

    pub(crate) fn apply_answer_repair(
        &mut self,
        issue_code: &'static str,
    ) -> Result<bool, EngineError> {
        let handler = match self.interpreter.current_operation()? {
            StateOperation::Builtin { handler, .. }
                if matches!(
                    handler,
                    BuiltinHandler::ValidateArtifact | BuiltinHandler::VerifyOutput
                ) =>
            {
                *handler
            }
            _ => return Ok(false),
        };
        let facts = serde_json::json!({
            "verification_ok": false,
            "issue_code": issue_code,
        });
        let event = match self.program.unique_transition_event(
            self.interpreter.current_state(),
            &facts,
            |candidate| matches!(candidate.operation, StateOperation::ModelDecision { .. }),
            "typed output repair",
        ) {
            Ok(event) => event,
            Err(EngineError::WorkflowResolution { .. }) => return Ok(false),
            Err(error) => return Err(error),
        };
        if !self.reserve_repair()? {
            return Ok(false);
        }
        self.apply_builtin_artifact(
            handler,
            &event,
            ContractPin::canonical(STATE_FACTS_V1)?,
            &facts,
        )?;
        self.require_model_state()?;
        Ok(true)
    }

    pub(crate) fn append_repair_feedback(&mut self, contract_id: &str, code: &'static str) {
        self.messages.push(RunEngineMessage::user(format!(
            "KRW kernel rejected the previous {contract_id} output with code {code}. Repair only that draft; do not add unsupported facts. Return only corrected {contract_id} JSON."
        )));
    }

    /// Acknowledge one retained report-sections/v1 batch so the next compose
    /// turn continues the report instead of re-proposing the same sections.
    pub(crate) fn append_section_batch_ack(&mut self, retained: usize) {
        self.messages.push(RunEngineMessage::user(format!(
            "KRW kernel retained the submitted report-sections/v1 batch ({retained} sections accumulated). Continue the report: emit the next batch with new section_ids only, or finish with continuation \"report_done\" and exactly three follow_up_questions."
        )));
    }

    /// Markdown composition is intentionally not JSON. Keep its bounded
    /// retry instruction separate from structured repair feedback so a
    /// length/empty-content recovery cannot ask the composer to emit a
    /// literal contract object.
    pub(crate) fn append_direct_answer_retry_feedback(&mut self, code: &'static str) {
        self.messages.push(RunEngineMessage::user(format!(
            "KRW kernel could not accept the previous final answer ({code}). Write the complete investor-facing answer again in plain Markdown. Do not output JSON, internal workflow details, or a checklist; use only the evidence already admitted in this run."
        )));
    }

    /// Follows the same bounded-retry pattern as
    /// [`Self::append_direct_answer_retry_feedback`], but names the content
    /// defect the deterministic Markdown gate found so the rewrite targets
    /// the framing instead of guessing.
    pub(crate) fn append_answer_content_gate_feedback(&mut self, issue: &str) {
        self.messages.push(RunEngineMessage::user(format!(
            "KRW kernel rejected the previous final answer: {issue} Write the complete investor-facing answer again in plain Markdown. Lead with the supported judgment (direction first); walk the arithmetic out loud from the admitted aggregates and calculations; give each competing reading its evidence; state any limitation beside the affected claim, never as the answer's frame; do not name internal systems or tools. Use only the evidence already admitted in this run."
        )));
    }

    pub(crate) fn direct_answer_retry_requested(&self) -> bool {
        self.direct_answer_retry_requested
    }

    /// Events whose transition hands research to composition. Judgment notes
    /// are accepted only here; any other transition discards them so a
    /// mid-research note cannot linger into a later answer.
    const JUDGMENT_CARRYING_EVENTS: [&'static str; 3] = [
        "evidence_sufficient",
        "no_positive_value_action",
        "output_budget_reserved",
    ];

    /// Release B: capture the analyst's bounded judgment notes from the
    /// evidence-sufficient handoff. The notes are already validated
    /// (count/length/confidence) by the transition parser.
    pub(crate) fn capture_analyst_judgment(
        &mut self,
        event: &str,
        judgment: Vec<AnalystJudgmentNote>,
    ) {
        if Self::JUDGMENT_CARRYING_EVENTS.contains(&event) {
            self.analyst_judgment = judgment;
        }
    }

    pub(crate) fn request_direct_answer_retry(&mut self) {
        self.direct_answer_retry_requested = true;
    }

    pub(crate) fn reserve_provider_turn(
        &mut self,
        image: &AgentImageManifest,
    ) -> Result<(), EngineError> {
        self.usage.provider_turns = self
            .usage
            .provider_turns
            .checked_add(1)
            .ok_or(EngineError::CounterOverflow("provider_turns"))?;
        if self.direct_answer_retry_requested || self.current_operation_emits_answer(image)? {
            // The direct-answer retry holds an inviolable one-turn grant. A
            // research-heavy transcript can already sit past the cumulative
            // input cap (production GLM 2026-09-01 AMZN: 193k > 168k input
            // killed the retry turn before dispatch and degraded a run that
            // had earned its answer), and the output grant is floored in
            // `provider_turn_policy`. Only the provider-turn ceiling still
            // applies to this turn; the token overshoot is bounded by the
            // single retry request.
            if u64::from(self.usage.provider_turns) > u64::from(self.limits.max_provider_turns) {
                return Err(EngineError::Contract(
                    krw_agent_protocol::ContractError::BudgetExceeded {
                        resource: "provider_turns",
                        used: u64::from(self.usage.provider_turns),
                        limit: u64::from(self.limits.max_provider_turns),
                    },
                ));
            }
            return Ok(());
        }
        self.check_budget()
    }

    pub(crate) fn record_prompt_assembly(
        &mut self,
        tool_definitions: Vec<ProviderToolDefinition>,
        tool_schema_hash: ContentHash,
        prompt_receipt_hash: ContentHash,
    ) -> Result<(), EngineError> {
        let expected = usize::from(self.usage.provider_turns);
        if self.prompt_receipt_hashes.len().checked_add(1) != Some(expected) {
            return Err(EngineError::Invariant(
                "prompt assembly receipt sequence is not contiguous",
            ));
        }
        self.tool_definitions = tool_definitions;
        self.tool_schema_hash = tool_schema_hash;
        self.prompt_receipt_hashes.push(prompt_receipt_hash);
        Ok(())
    }

    pub(crate) fn compact_settled_phase(
        &mut self,
        provider_episode_hash: ContentHash,
        max_context_bytes: usize,
    ) -> Result<(), EngineError> {
        let compact_t0 = Instant::now();
        let mut source_messages = std::mem::take(&mut self.messages);
        // `compact` consumes the Anthropic wire shape. Convert the internal
        // transcript to `ProviderMessage` for the duration of the call; the
        // original `RunEngineMessage` vector is restored afterwards.
        let wire_source_messages = source_messages
            .iter()
            .map(RunEngineMessage::to_provider_message_for_serialization)
            .collect::<Vec<_>>();
        let output = (|| -> Result<_, EngineError> {
            let artifact = self
                .interpreter
                .last_artifact()
                .ok_or(EngineError::Invariant(
                    "settled provider phase has no validated state artifact",
                ))?;
            let evidence_refs = self
                .ledger
                .iter()
                .map(|(_, record)| record.content_hash.clone())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            let boundary = PhaseCompactionBoundaryV1::seal(
                artifact,
                &ProviderReplayState::Settled {
                    provider_episode_hash,
                },
                evidence_refs,
            )?;
            Ok(compact(&CompactionInput {
                boundary: &boundary,
                state_artifact: artifact,
                source_messages: &wire_source_messages,
                ledger: &self.ledger,
                calculations: &self.calculations,
                analyst_judgment: &self.analyst_judgment,
                research_projection: self.research_planner.projection(),
                max_context_bytes,
            })?)
        })();
        for message in &mut source_messages {
            message.scrub_sensitive();
        }
        let output = output?;
        let receipt_hash = output.receipt.receipt_hash()?;
        let context_hash = output.receipt.compacted_context_hash.clone();
        let boundary_hash = output.receipt.boundary_hash.clone();
        self.compaction_receipts.push(output.receipt.clone());
        // Retain the typed context (cloned before the canonical is consumed) so
        // a role-filtered view can be derived at prompt-build time. The clone
        // is bounded by MAX_COMPACTED_CONTEXT_BYTES (<= 2 MiB).
        let context = output.context.clone();
        let canonical = output.into_canonical_context();
        self.compacted_context = Some(PreparedCompactedContext {
            canonical,
            context,
            context_hash,
            receipt_hash,
            boundary_hash,
        });
        self.record_compact_duration_ms(elapsed_millis(compact_t0));
        Ok(())
    }

    pub(crate) fn reserve_replan(&mut self) -> Result<(), EngineError> {
        self.usage.replans = self
            .usage
            .replans
            .checked_add(1)
            .ok_or(EngineError::CounterOverflow("replans"))?;
        self.check_budget()
    }

    pub(crate) fn has_replan_budget(&self) -> bool {
        self.usage.replans < self.limits.max_replans
    }

    /// Evaluate every provider-proposed read candidate as one bounded decision
    /// set. The model is free to propose alternatives (or to take a typed
    /// workflow transition instead); the planner chooses only among those
    /// candidates using the committed evidence frontier and pinned costs.
    ///
    /// Non-research capabilities remain serial. They are not silently mixed
    /// with research candidates because that would make a provider tool batch
    /// an implicit statechart fork.
    pub(crate) fn decide_research_dispatch(
        &mut self,
        calls: &[PreparedCall],
        question_only: bool,
    ) -> Result<ResearchDispatchDecision, EngineError> {
        let first = calls
            .first()
            .ok_or(EngineError::Invariant("provider decision set is empty"))?;
        for call in calls {
            if !self.capability_has_remaining_visit(&call.capability.id)? {
                // This can occur only if a provider episode bypassed the
                // dynamic frontier check. Do not let a later interpreter
                // transition turn that protocol violation into a misleading
                // state visit failure.
                return Err(EngineError::InvalidProviderEpisode(
                    "capability target has no remaining statechart capacity",
                ));
            }
        }

        let Some(_) = first.capability.research_action.as_ref() else {
            if calls.len() == 1 {
                return Ok(ResearchDispatchDecision::Execute { selected_index: 0 });
            }
            // Kernel-owned supplemental batching (2026-09-01): models
            // naturally batch independent supplemental reads (filing events,
            // openbb price history, news) and punishing the batch costs a
            // repair turn per attempt — production GOOGL burned its whole
            // budget on rejected 3-call and 6-call batches. Accept a
            // homogeneous non-research batch: the first call dispatches now,
            // the kernel drains the rest one statechart visit at a time.
            if calls.len() <= Self::MAX_SUPPLEMENTAL_BATCH
                && calls
                    .iter()
                    .all(|call| call.capability.research_action.is_none())
                && let Some(episode_hash) = self.last_provider_episode_hash.clone()
            {
                self.pending_supplemental_calls.extend(
                    calls[1..]
                        .iter()
                        .cloned()
                        .map(|call| (episode_hash.clone(), call)),
                );
                return Ok(ResearchDispatchDecision::Execute { selected_index: 0 });
            }
            return Err(EngineError::WorkflowResolution {
                outcome: "non-research capability decision batch",
            });
        };
        if calls
            .iter()
            .skip(1)
            .any(|call| call.capability.research_action.is_none())
        {
            return Err(EngineError::WorkflowResolution {
                outcome: "mixed research capability decision batch",
            });
        }

        let proposals = calls
            .iter()
            .map(|call| {
                call.capability
                    .research_action
                    .as_ref()
                    .map(|policy| research_candidate(call, policy))
                    .ok_or(EngineError::Invariant(
                        "validated research decision batch lost action policy",
                    ))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let evaluated = u16::try_from(calls.len()).unwrap_or(u16::MAX);
        match self.research_planner.select(&proposals)? {
            PlannerDecision::Execute {
                proposal_id,
                score,
                reason,
                evaluated: observed,
            } if observed == evaluated => {
                let selected_index = calls
                    .iter()
                    .position(|call| call.tool_call_id == proposal_id)
                    .ok_or(EngineError::ResearchPlannerDecisionMismatch)?;
                let selected_policy = calls[selected_index]
                    .capability
                    .research_action
                    .as_ref()
                    .ok_or(EngineError::ResearchPlannerDecisionMismatch)?;
                match (selected_policy.kind, reason, score) {
                    (ImageResearchActionKind::Context, SelectionReason::InitialContext, None)
                        if calls.len() == 1 =>
                    {
                        Ok(ResearchDispatchDecision::Execute { selected_index })
                    }
                    (_, SelectionReason::PositiveExpectedValue, Some(score)) if score > 0 => {
                        if !self.has_replan_budget() {
                            return Ok(ResearchDispatchDecision::NoPositiveValue(
                                ResearchStopReason::ReplanBudgetExhausted,
                            ));
                        }
                        self.reserve_replan()?;
                        Ok(ResearchDispatchDecision::Execute { selected_index })
                    }
                    _ => Err(EngineError::ResearchPlannerDecisionMismatch),
                }
            }
            PlannerDecision::NoPositiveValue {
                evaluated: observed,
                reason,
            } if observed == evaluated => {
                if reason == NoPositiveReason::NoFrontier {
                    if self.has_replan_budget() {
                        self.reserve_replan()?;
                        Ok(ResearchDispatchDecision::NoPositiveValue(
                            ResearchStopReason::NoFrontier,
                        ))
                    } else {
                        Ok(ResearchDispatchDecision::NoPositiveValue(
                            ResearchStopReason::ReplanBudgetExhausted,
                        ))
                    }
                } else if question_only && reason == NoPositiveReason::ProposalUnmapped {
                    // Free-door market-plane override (live 2026-09-04,
                    // run_3d4bb3ff): a question_only universe plan maps
                    // proposals through discovery clauses, so a
                    // market-plane read for the issuer the question itself
                    // names — or any canonical ticker the doctrine admits —
                    // can be unmapped even though the scope validator
                    // already blessed the ticker. Execute the batch leader
                    // instead of burning a replan and teaching the model
                    // the door is closed. Company runs keep the strict
                    // candidate mapping (their trusted scope always has
                    // candidates).
                    //
                    // Observation-batch drain (live 2026-09-07, PLTR
                    // run_0b02e610): the doctrine admits a ≤4 homogeneous
                    // observation batch, but executing only the leader
                    // starved answers down to a single read (quote-only)
                    // and burned repair turns on re-emitted batches. Drain
                    // the remaining calls through the same kernel-owned
                    // queue the supplemental lane uses; every drained call
                    // still routes through its own statechart transition
                    // (visit, action-limit, scope, and contract checks).
                    if calls.len() <= Self::MAX_SUPPLEMENTAL_BATCH
                        && let Some(episode_hash) = self.last_provider_episode_hash.clone()
                    {
                        self.pending_supplemental_calls.extend(
                            calls[1..]
                                .iter()
                                .cloned()
                                .map(|call| (episode_hash.clone(), call)),
                        );
                    }
                    Ok(ResearchDispatchDecision::Execute { selected_index: 0 })
                } else if self.has_replan_budget() {
                    self.reserve_replan()?;
                    Ok(ResearchDispatchDecision::ProposalRejected(reason))
                } else {
                    Ok(ResearchDispatchDecision::NoPositiveValue(
                        ResearchStopReason::ReplanBudgetExhausted,
                    ))
                }
            }
            _ => Err(EngineError::ResearchPlannerDecisionMismatch),
        }
    }

    pub(crate) fn append_no_positive_value_result(
        &mut self,
        episode: &ProviderEpisodeV1,
        calls: &[PreparedCall],
        _reason: ResearchStopReason,
    ) -> Result<(), EngineError> {
        // The provider needs one result per speculative tool call before it
        // can emit the next state transition. A declined call is not evidence,
        // though, so acknowledge it with a neutral empty result rather than a
        // workflow reason that a later composer could repeat to the user.
        self.append_assistant(episode);
        for call in calls {
            self.append_tool_result(
                &call.tool_call_id,
                &serde_json::json!({
                    "schema_version": 1,
                    "status": "complete",
                    "contains_evidence": false,
                }),
            )?;
        }
        Ok(())
    }

    pub(crate) fn append_proposal_rejected_result(
        &mut self,
        episode: &ProviderEpisodeV1,
        calls: &[PreparedCall],
        reason: NoPositiveReason,
    ) -> Result<(), EngineError> {
        self.append_assistant(episode);
        for call in calls {
            self.append_tool_result(
                &call.tool_call_id,
                &serde_json::json!({
                    "schema_version": 1,
                    "status": "not_dispatched",
                    "reason_code": "proposal_rejected",
                    "rejection": rejection_reason_code(reason),
                    "contains_evidence": false
                }),
            )?;
        }
        Ok(())
    }

    /// Close every model tool call that lost the deterministic value-of-
    /// information comparison. The selected call is completed later with its
    /// real capability result; unselected calls are never routed, charged, or
    /// recorded as actions.
    pub(crate) fn append_unselected_research_results(
        &mut self,
        calls: &[PreparedCall],
    ) -> Result<(), EngineError> {
        for call in calls {
            self.append_tool_result(
                &call.tool_call_id,
                &serde_json::json!({
                    "schema_version": 1,
                    "status": "not_dispatched",
                    "reason_code": "lower_value_candidate",
                    "contains_evidence": false,
                }),
            )?;
        }
        Ok(())
    }

    pub(crate) fn commit_research_result(
        &mut self,
        call: &PreparedCall,
        result: &CapabilityResult,
    ) -> Result<(), EngineError> {
        let Some(policy) = call.capability.research_action.as_ref() else {
            return Ok(());
        };
        let fingerprint = research_fingerprint(call);
        let mut next = self.research_planner.clone();
        match policy.kind {
            ImageResearchActionKind::Context => {
                if is_input_correction(result) {
                    next.record_rejected_context(fingerprint)?;
                } else {
                    let bytes =
                        zeroize::Zeroizing::new(serde_jcs::to_vec(&result.provider_content)?);
                    let mut research_state = parse_research_state(bytes.as_slice())
                        .map_err(ResearchPlannerError::from)?;
                    research_state.plan = canonicalize_normalized_plan_exchange(
                        &call.arguments,
                        &research_state.plan,
                    )?;
                    if next.projection().is_none() {
                        next.record_initial_context(fingerprint)?;
                    } else {
                        next.record_completed(fingerprint)?;
                    }
                    // The evidence ledger is the single source of truth for
                    // company orientation: `ontology.company_context` commits
                    // its advisory records there before the canonical
                    // `query_context` read, so distilling the vocabulary here
                    // promotes exactly the trusted facts — never the raw
                    // capability payload — into the planning projection.
                    let ledger_records = self
                        .ledger
                        .iter()
                        .map(|(_, record)| record.clone())
                        .collect::<Vec<_>>();
                    let orientation = company_orientation_vocabulary(&ledger_records);
                    if let Some(receipt) = &call.research_intent_receipt {
                        next.ingest_research_state_for_intent(
                            &research_state,
                            receipt,
                            &orientation,
                        )?;
                    } else {
                        next.ingest_research_state(&research_state, &orientation)?;
                    }
                }
            }
            ImageResearchActionKind::Targeted | ImageResearchActionKind::Trace => {
                next.record_completed(fingerprint)?;
                if let Some(status) = Self::supplemental_retrieval_status(policy.kind, result) {
                    next.record_supplemental_retrieval_status(status.clone());
                    if let Some(warning) =
                        Self::supplemental_retrieval_warning_for_status(policy.kind, &status)
                    {
                        next.record_supplemental_retrieval_warning(warning);
                    }
                }
            }
            ImageResearchActionKind::Observation => {
                // An observation completes its own action fingerprint only:
                // whether the clause it informed moved stays the evidence
                // ledger's decision, and the openbb envelope is not a
                // supplemental filing read status.
                next.record_completed(fingerprint)?;
            }
        }
        self.research_planner = next;
        Ok(())
    }

    /// Classify a failed supplemental read without adding a new terminal
    /// condition. The immediate model turn retains the raw payload; this
    /// fixed, non-sensitive marker is what survives the next compaction and
    /// tells later roles that the result does not prove company non-disclosure.
    fn supplemental_retrieval_status(
        kind: ImageResearchActionKind,
        result: &CapabilityResult,
    ) -> Option<SupplementalReadStatus> {
        let payload = &result.provider_content;
        match kind {
            ImageResearchActionKind::Targeted => {
                supplemental_status_for_targeted_payload(payload).ok()
            }
            ImageResearchActionKind::Trace => supplemental_status_for_trace_payload(payload).ok(),
            ImageResearchActionKind::Context | ImageResearchActionKind::Observation => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn supplemental_retrieval_warning(
        kind: ImageResearchActionKind,
        result: &CapabilityResult,
    ) -> Option<&'static str> {
        let status = Self::supplemental_retrieval_status(kind, result)?;
        Self::supplemental_retrieval_warning_for_status(kind, &status)
    }

    fn supplemental_retrieval_warning_for_status(
        kind: ImageResearchActionKind,
        status: &SupplementalReadStatus,
    ) -> Option<&'static str> {
        Some(match (kind, status.kind) {
            (_, SupplementalReadKind::Retrieved) if status.has_more => match kind {
                ImageResearchActionKind::Targeted => "supplemental_targeted_query_truncated",
                ImageResearchActionKind::Trace => "supplemental_trace_truncated",
                ImageResearchActionKind::Context | ImageResearchActionKind::Observation => {
                    return None;
                }
            },
            (ImageResearchActionKind::Observation, _) => return None,
            (ImageResearchActionKind::Targeted, SupplementalReadKind::Empty) => {
                "supplemental_targeted_query_empty"
            }
            (ImageResearchActionKind::Trace, SupplementalReadKind::Empty) => {
                "supplemental_trace_empty"
            }
            (ImageResearchActionKind::Targeted, SupplementalReadKind::Ambiguous) => {
                "supplemental_targeted_query_ambiguous"
            }
            (ImageResearchActionKind::Trace, SupplementalReadKind::Ambiguous) => {
                "supplemental_trace_ambiguous"
            }
            (ImageResearchActionKind::Targeted, SupplementalReadKind::InputRejected) => {
                "supplemental_targeted_query_input_not_accepted"
            }
            (ImageResearchActionKind::Trace, SupplementalReadKind::InputRejected) => {
                "supplemental_trace_input_not_accepted"
            }
            (ImageResearchActionKind::Targeted, SupplementalReadKind::NotFound) => {
                "supplemental_targeted_query_not_found"
            }
            (ImageResearchActionKind::Trace, SupplementalReadKind::NotFound) => {
                "supplemental_trace_not_found"
            }
            (ImageResearchActionKind::Targeted, SupplementalReadKind::ApplicationError) => {
                "supplemental_targeted_query_unavailable"
            }
            (ImageResearchActionKind::Trace, SupplementalReadKind::ApplicationError) => {
                "supplemental_trace_unavailable"
            }
            (_, SupplementalReadKind::Retrieved) => return None,
            (ImageResearchActionKind::Context, _) => return None,
        })
    }

    pub(crate) fn record_provider_usage(
        &mut self,
        episode: &ProviderEpisodeV1,
    ) -> Result<(), EngineError> {
        self.usage.input_tokens = self
            .usage
            .input_tokens
            .checked_add(episode.usage.prompt_tokens)
            .ok_or(EngineError::CounterOverflow("input_tokens"))?;
        self.usage.output_tokens = self
            .usage
            .output_tokens
            .checked_add(episode.usage.completion_tokens)
            .ok_or(EngineError::CounterOverflow("output_tokens"))?;
        // Provider usage is known only after the response is received. A
        // provider can report a value just above the request allowance (for
        // example due to provider-side token accounting). Keep the receipt
        // exact, but do not erase an already-received, contract-valid final
        // response. Every later provider admission still passes through
        // `reserve_provider_turn`, whose budget check closes the run to any
        // additional model call once this usage is over the declared limit.
        Ok(())
    }

    /// Accumulate wall-clock time spent inside one provider turn (the
    /// `Provider::complete` future). Saturating on overflow keeps an inflated
    /// measurement from turning into a kernel panic.
    pub(crate) fn record_provider_duration_ms(&mut self, ms: u64) {
        self.usage.provider_total_ms = self.usage.provider_total_ms.saturating_add(ms);
    }

    /// Accumulate wall-clock time spent inside one capability dispatch.
    pub(crate) fn record_capability_duration_ms(&mut self, ms: u64) {
        self.usage.capability_total_ms = self.usage.capability_total_ms.saturating_add(ms);
    }

    /// Accumulate wall-clock time spent inside phase compaction.
    pub(crate) fn record_compact_duration_ms(&mut self, ms: u64) {
        self.usage.compact_total_ms = self.usage.compact_total_ms.saturating_add(ms);
    }

    pub(crate) fn record_provider_episode_hash(&mut self, episode_hash: ContentHash) {
        self.last_provider_episode_hash = Some(episode_hash);
    }

    pub(crate) fn reserve_capability_call(
        &mut self,
        capability_id: &str,
        action_key: &str,
    ) -> Result<(), EngineError> {
        if !self.logical_action_keys.insert(action_key.into()) {
            return Ok(());
        }
        self.usage.capability_calls = self
            .usage
            .capability_calls
            .checked_add(1)
            .ok_or(EngineError::CounterOverflow("capability_calls"))?;
        let calls = self
            .capability_calls
            .entry(capability_id.into())
            .or_default();
        *calls = calls
            .checked_add(1)
            .ok_or(EngineError::CounterOverflow("capability_calls_by_id"))?;
        if let Some(limit) = self.limits.capability_call_limits.get(capability_id)
            && *calls > *limit
        {
            return Err(EngineError::CapabilityBudgetExceeded {
                capability_id: capability_id.into(),
                used: *calls,
                limit: *limit,
            });
        }
        self.check_budget()
    }

    pub(crate) fn record_accepted_action(
        &mut self,
        call: &PreparedCall,
    ) -> Result<(), EngineError> {
        let retain_input = call.capability.retain_canonical_input;
        if let Some(existing) = self
            .accepted_actions
            .iter()
            .find(|action| action.action_key == call.action_key)
        {
            let retained_matches = match (&existing.sealed_input, retain_input) {
                (Some(bytes), true) => bytes.as_ref() == call.canonical_arguments.as_slice(),
                (None, false) => true,
                _ => false,
            };
            if existing.capability_id != call.capability.id
                || existing.input_contract != call.capability.input_contract
                || existing.input_hash != call.request_hash
                || !retained_matches
            {
                return Err(EngineError::Invariant(
                    "accepted action reference conflicts with its durable action",
                ));
            }
            return Ok(());
        }
        self.accepted_actions.push(AcceptedActionRef {
            action_key: call.action_key.clone(),
            capability_id: call.capability.id.clone(),
            input_contract: call.capability.input_contract.clone(),
            input_hash: call.request_hash.clone(),
            sealed_input: retain_input.then(|| call.canonical_arguments.clone().into_boxed_slice()),
        });
        Ok(())
    }

    pub(crate) fn ensure_accepted_action(&self, call: &PreparedCall) -> Result<(), EngineError> {
        let Some(existing) = self
            .accepted_actions
            .iter()
            .find(|action| action.action_key == call.action_key)
        else {
            return Err(EngineError::Invariant(
                "cached action lacks its accepted action reference",
            ));
        };
        if existing.capability_id != call.capability.id
            || existing.input_contract != call.capability.input_contract
            || existing.input_hash != call.request_hash
        {
            return Err(EngineError::Invariant(
                "cached action reference differs from the prepared action",
            ));
        }
        Ok(())
    }

    /// Shared cache-hit detection for a prepared call: the canonical action
    /// key (computed identically on the live and recovery paths by
    /// `prepare_calls`) was already executed — it is charged in
    /// `logical_action_keys` — and its committed result is resident in the
    /// action cache. The live orchestrator branch and the recovery replay of
    /// a committed episode must agree on this predicate, so it lives here
    /// once instead of being duplicated at both call sites.
    pub(crate) fn cached_action_result(&self, call: &PreparedCall) -> Option<CapabilityResult> {
        if self.logical_action_keys.contains(&call.action_key) {
            self.action_cache.get(&call.action_key).cloned()
        } else {
            None
        }
    }

    /// Settle a prepared call from its cached committed result. This is the
    /// single body shared by the live cache-hit branch and recovery replay of
    /// a cache-hit episode: the cached result is re-projected and the
    /// capability is completed exactly as the first execution left it, with
    /// no fresh dispatch, no evidence re-ingestion, no new accepted-action
    /// reference, and no new logical action key.
    pub(crate) fn complete_cached_capability(
        &mut self,
        image: &AgentImageManifest,
        call: &PreparedCall,
        cached: &CapabilityResult,
        episode_hash: ContentHash,
        max_compacted_context_bytes: usize,
        append_transcript: bool,
    ) -> Result<(), EngineError> {
        self.ensure_accepted_action(call)?;
        self.ingest_scope_projection(call, cached)?;
        if append_transcript {
            self.append_capability_tool_result(call, cached)?;
        }
        if capability_result_completes_prerequisite(cached) {
            self.completed_capabilities
                .insert(call.capability.id.clone());
        }
        self.complete_capability(
            image,
            call,
            cached,
            accepted_action_receipt_hash(call, cached)?,
        )?;
        if append_transcript && capability_result_completes_prerequisite(cached) {
            // Every admitted external read is a settled provider boundary.
            // Rebuild the next turn from kernel-owned evidence rather than
            // replaying a raw direct-mode tool call into a thinking-mode
            // request.
            self.compact_settled_phase(episode_hash, max_compacted_context_bytes)?;
        }
        Ok(())
    }

    pub(crate) fn preflight_capability_calls(
        &self,
        image: &AgentImageManifest,
        calls: &[PreparedCall],
    ) -> Result<(), EngineError> {
        let mut logical_action_keys = self.logical_action_keys.clone();
        let mut capability_calls = self.capability_calls.clone();
        let mut usage = self.usage.clone();
        let mut state_trace = self.state_trace.clone();
        let mut plan_evaluated = self.current_state()?.kind != StateKind::Plan;
        for call in calls {
            if self.action_cache.contains_key(&call.action_key)
                || !logical_action_keys.insert(call.action_key.clone())
            {
                continue;
            }
            usage.capability_calls = usage
                .capability_calls
                .checked_add(1)
                .ok_or(EngineError::CounterOverflow("capability_calls"))?;
            let used = capability_calls
                .entry(call.capability.id.clone())
                .or_default();
            *used = used
                .checked_add(1)
                .ok_or(EngineError::CounterOverflow("capability_calls_by_id"))?;
            if let Some(limit) = self.limits.capability_call_limits.get(&call.capability.id)
                && *used > *limit
            {
                return Err(EngineError::CapabilityBudgetExceeded {
                    capability_id: call.capability.id.clone(),
                    used: *used,
                    limit: *limit,
                });
            }
            if !plan_evaluated {
                // Plan rules govern the model-authored contract, not the
                // derived physical MCP input. This keeps `ResearchIntent`
                // validation independent from root `SearchPlan` transport
                // validation and prevents a policy rule from accidentally
                // treating a compiler-owned field as model authority.
                evaluate_rules(image, RulePhase::Plan, &call.proposed_arguments, None)?;
                plan_evaluated = true;
            }
            let mut candidate_trace = state_trace.clone();
            candidate_trace.push(call.state_id.clone());
            let rule_input =
                action_rule_input(&call.arguments, &candidate_trace, &capability_calls)?;
            // A pre-action rule violation is a correctable proposal
            // discipline error (for example calling the last-resort web-news
            // rung before the filing-catalog or feed rungs), not a run
            // integrity failure. Convert it to a bounded model rejection so
            // the existing recovery machinery teaches the model the violated
            // order and the run continues with the earlier rung — the
            // terminal-failure behaviour measured 2026-08-31 killed runs
            // that one steering mistake.
            if let Err(EngineError::PhaseRuleViolations {
                phase: RulePhase::PreAction,
                violations,
            }) = evaluate_rules(image, RulePhase::PreAction, &rule_input, None)
            {
                return Err(EngineError::ModelProposalRejected(
                    ModelProposalRejection::Order { codes: violations },
                ));
            }
            state_trace = candidate_trace;
        }
        usage.ensure_within(&self.limits)?;
        Ok(())
    }

    pub(crate) fn validate_post_action(
        &self,
        image: &AgentImageManifest,
        call: &PreparedCall,
        result: &CapabilityResult,
    ) -> Result<(), EngineError> {
        // A typed query-context correction is a successful protocol response,
        // but it is deliberately not a ResearchState. Its own canonical
        // contract was already validated before this point; ResearchState-only
        // post-action rules apply after the corrected request succeeds.
        if is_input_correction(result) {
            return Ok(());
        }
        let mut state_trace = self.state_trace.clone();
        state_trace.push(call.state_id.clone());
        let rule_input = action_rule_input(
            &result.provider_content,
            &state_trace,
            &self.capability_calls,
        )?;
        evaluate_rules(
            image,
            RulePhase::PostAction,
            &rule_input,
            Some(call.capability.result_ingest),
        )
    }

    pub(crate) fn record_evidence_bytes(&mut self, bytes: usize) -> Result<(), EngineError> {
        let bytes = u64::try_from(bytes).map_err(|_| EngineError::CounterOverflow("evidence"))?;
        self.usage.evidence_bytes = self
            .usage
            .evidence_bytes
            .checked_add(bytes)
            .ok_or(EngineError::CounterOverflow("evidence_bytes"))?;
        self.check_budget()
    }

    pub(crate) fn check_budget(&self) -> Result<(), EngineError> {
        self.usage.ensure_within(&self.limits)?;
        Ok(())
    }

    pub(crate) fn append_assistant(&mut self, episode: &ProviderEpisodeV1) {
        let mut assistant = episode.assistant.clone();
        // A tool-calling turn can carry arbitrary explanatory text alongside
        // its function call. That text is neither an admitted result nor a
        // user-facing answer; retaining it can make a later composer echo a
        // rejected dispatch or workflow detail. Preserve thinking provenance
        // and the exact tool calls, but omit that non-authoritative prose from
        // the model transcript.
        if !assistant.tool_calls.is_empty() {
            assistant.content = None;
        }
        self.messages
            .push(RunEngineMessage::from_assistant(assistant));
    }

    /// Retain only the image-owned bodies returned by local `skill.load` for a
    /// bounded child. Model prose, reasoning, and any other tool result are
    /// intentionally discarded at this boundary. The next child request gets
    /// these bodies through `bounded_child::isolate_request`, never through the
    /// parent conversation transcript.
    pub(crate) fn retain_child_skill_context(
        &mut self,
        resolution: &LocalSkillLoadResolution,
    ) -> Result<(), EngineError> {
        for (_, result) in &resolution.loaded {
            let object = result
                .provider_content
                .as_object()
                .ok_or(EngineError::Invariant("skill.load result is not an object"))?;
            let skill_id = object
                .get("skill_id")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .ok_or(EngineError::Invariant("skill.load result has no skill_id"))?;
            let body = object
                .get("content")
                .and_then(Value::as_str)
                .ok_or(EngineError::Invariant("skill.load result has no content"))?;
            // Re-loading the same immutable blob does not add context or spend
            // more prompt bytes. This also keeps repeated model calls bounded.
            if self
                .child_skill_context
                .loaded
                .iter()
                .any(|loaded| loaded.skill_id == skill_id)
            {
                continue;
            }
            let added_bytes = skill_id
                .len()
                .checked_add(body.len())
                .ok_or(EngineError::CounterOverflow("child skill context bytes"))?;
            let total = self
                .child_skill_context
                .bytes
                .checked_add(added_bytes)
                .ok_or(EngineError::CounterOverflow("child skill context bytes"))?;
            if total > MAX_BOUNDED_CHILD_SKILL_CONTEXT_BYTES {
                return Err(EngineError::SizeLimit {
                    resource: "bounded_child_skill_context",
                    observed: total,
                    limit: MAX_BOUNDED_CHILD_SKILL_CONTEXT_BYTES,
                });
            }
            self.child_skill_context.loaded.push(ChildSkillBody {
                skill_id: skill_id.to_owned(),
                body: body.to_owned(),
            });
            self.child_skill_context.bytes = total;
        }
        self.child_skill_context.deferred_tool_calls = self
            .child_skill_context
            .deferred_tool_calls
            .checked_add(resolution.deferred_tool_call_ids.len())
            .ok_or(EngineError::CounterOverflow("child deferred skill calls"))?;
        Ok(())
    }

    pub(crate) fn clear_child_skill_context(&mut self) {
        for skill in &mut self.child_skill_context.loaded {
            skill.skill_id.zeroize();
            skill.body.zeroize();
        }
        self.child_skill_context.loaded.clear();
        self.child_skill_context.deferred_tool_calls = 0;
        self.child_skill_context.bytes = 0;
    }

    pub(crate) fn append_workflow_transition_result(
        &mut self,
        episode: &ProviderEpisodeV1,
        transition: &WorkflowTransitionCall,
    ) -> Result<(), EngineError> {
        self.append_assistant(episode);
        self.append_tool_result(
            &transition.tool_call_id,
            &serde_json::json!({
                "schema_version": 1,
                "status": "transition_accepted",
                "event": transition.event,
                "contains_evidence": false,
            }),
        )
    }

    pub(crate) fn append_tool_result(
        &mut self,
        tool_call_id: &str,
        content: &Value,
    ) -> Result<(), EngineError> {
        // `ToolResultMessage` owns the provider's text-only JSON rule. A
        // typed capability object cannot enter the transcript as an object.
        self.messages.push(RunEngineMessage::from_tool_result(
            &ToolResultMessage::from_value(tool_call_id, content)?,
        ));
        Ok(())
    }

    pub(crate) fn append_capability_tool_result(
        &mut self,
        call: &PreparedCall,
        result: &CapabilityResult,
    ) -> Result<(), EngineError> {
        let ladder_dispatched = self
            .capability_calls
            .keys()
            .any(|capability_id| EVENT_LADDER_CAPABILITY_IDS.contains(&capability_id.as_str()));
        let ontology_targeted_dispatched = self.capability_calls.keys().any(|capability_id| {
            matches!(capability_id.as_str(), "ontology.query" | "ontology.trace")
        });
        let market_observation_dispatched = self.capability_calls.keys().any(|capability_id| {
            matches!(
                capability_id.as_str(),
                "market.snapshot"
                    | "market.series"
                    | "macro.series"
                    | "openbb.price_history"
                    | "openbb.quote"
                    | "openbb.macro_series"
            )
        });
        let mut content = model_visible_capability_result(
            call,
            result,
            ladder_dispatched,
            ontology_targeted_dispatched,
            market_observation_dispatched,
        );
        // The no-data fallback note must reach the model even when the
        // observation call carries no research-intent receipt (the visible
        // projection early-returns raw content without one), so the kernel
        // attaches it to the transcript copy directly.
        if let Some(hint) = crate::capability_dispatch::model_observation_no_data_hint(
            &call.capability.id,
            &result.provider_content,
        ) {
            if let Some(object) = content.as_object_mut() {
                object.insert("kernel_observation_no_data_hint".into(), hint);
            }
        }
        self.append_tool_result(&call.tool_call_id, &content)
    }

    pub(crate) fn check_conversation_limit(&self, limit: usize) -> Result<(), EngineError> {
        let bytes = serde_jcs::to_vec(&self.messages)?.len();
        ensure_size(bytes, limit, "provider_conversation")
    }

    pub(crate) fn ingest(&mut self, result: &CapabilityResult) -> Result<(), EngineError> {
        if let Some(pack) = result.presentation.as_ref()
            && presentation_pack_matches_result(pack, &result.provider_content)
        {
            self.retain_presentation_pack(pack);
        }
        for record in &result.evidence {
            self.ledger.append(record.clone())?;
        }
        if let Some(answerability) = result.answerability {
            self.ledger.set_answerability(answerability);
        } else {
            // Targeted/trace mappings intentionally do not carry a new
            // plan-level coverage verdict. When one of those precise reads
            // actually returns a directly supported fact, do not let a stale
            // broad-query `NotAnswerable` verdict suppress the recovered fact.
            // The ledger can reopen only to QualifiedOnly; it never grants a
            // strong conclusion without a fresh canonical ResearchState.
            self.ledger
                .reopen_qualified_after_substantive_supplement(&result.evidence);
        }
        for calculation in &result.calculations {
            self.ledger.append_calculation(calculation.clone())?;
            match self.calculations.get(&calculation.calculation_id) {
                Some(existing) if existing != calculation => {
                    return Err(EngineError::CalculationConflict(
                        calculation.calculation_id.clone(),
                    ));
                }
                Some(_) => {}
                None => {
                    self.calculations
                        .insert(calculation.calculation_id.clone(), calculation.clone());
                }
            }
        }
        Ok(())
    }

    /// Retain only bounded, structurally recognizable presentation data. A
    /// presentation defect is intentionally non-fatal: research evidence and
    /// the final text answer must continue even when the optional chart is
    /// discarded.
    pub(crate) fn retain_presentation_pack(&mut self, pack: &Value) {
        const MAX_PACKS_PER_RUN: usize = 16;
        const MAX_PACK_BYTES: usize = 64 * 1024;
        const MAX_SERIES: usize = 8;
        const MAX_POINTS: usize = 12;
        if self.presentation_packs.len() >= MAX_PACKS_PER_RUN
            || pack.get("schema_version").and_then(Value::as_u64) != Some(2)
        {
            return;
        }
        let Some(series) = pack.get("series").and_then(Value::as_array) else {
            return;
        };
        if series.len() > MAX_SERIES
            || series.iter().any(|item| {
                item.get("points")
                    .and_then(Value::as_array)
                    .map_or(true, |points| points.len() > MAX_POINTS)
            })
        {
            return;
        }
        let Ok(bytes) = serde_jcs::to_vec(pack) else {
            return;
        };
        if bytes.len() > MAX_PACK_BYTES {
            return;
        }
        let hash = ContentHash::sha256(&bytes);
        if self
            .presentation_packs
            .iter()
            .filter_map(|existing| serde_jcs::to_vec(existing).ok())
            .all(|existing| ContentHash::sha256(existing) != hash)
        {
            self.presentation_packs.push(pack.clone());
        }
    }

    /// Number of distinct section ids retained across every accepted
    /// report-sections/v1 batch (the assembler's deduplicated section count).
    pub(crate) fn composed_sections_len(&self) -> usize {
        self.composed_section_ids().len()
    }

    /// Distinct section count this run would hold if `batch` were retained
    /// (the projected acknowledgement count used before the batch is
    /// actually pushed).
    pub(crate) fn composed_sections_len_with(&self, batch: &Value) -> usize {
        let mut ids = self.composed_section_ids();
        if let Some(sections) = batch.get("sections").and_then(Value::as_array) {
            for section in sections {
                if let Some(id) = section.get("section_id").and_then(Value::as_str) {
                    ids.insert(id.to_owned());
                }
            }
        }
        ids.len()
    }

    /// Every section id already retained by earlier batches.
    fn composed_section_ids(&self) -> BTreeSet<String> {
        self.composed_sections
            .iter()
            .filter_map(|batch| batch.get("sections").and_then(Value::as_array))
            .flat_map(|sections| sections.iter())
            .filter_map(|section| section.get("section_id").and_then(Value::as_str))
            .map(str::to_owned)
            .collect()
    }

    /// Validate one report-sections/v1 batch without retaining it. Bounded
    /// per batch by the contract (16 sections), per run by
    /// `MAX_ANSWER_SECTIONS`, and by bytes so a runaway composer cannot
    /// balloon the checkpoint. A batch that re-issues only already-retained
    /// section ids is an error (repair lane), never a silent merge: pure
    /// composer repetition must surface as a loop error. Partial overlap
    /// with earlier batches is tolerated and deduplicated at assembly.
    pub(crate) fn validate_composed_section(&self, batch: &Value) -> Result<(), EngineError> {
        const MAX_BATCH_BYTES: usize = 256 * 1024;
        const MAX_SECTIONS_PER_BATCH: usize = 16;
        if batch.get("schema_version").and_then(Value::as_u64) != Some(1) {
            return Err(EngineError::AnswerValidation(vec![
                "unsupported_section_schema".to_owned(),
            ]));
        }
        let sections = batch
            .get("sections")
            .and_then(Value::as_array)
            .ok_or_else(|| EngineError::AnswerValidation(vec!["missing_sections".into()]))?;
        if sections.is_empty() {
            return Err(EngineError::AnswerValidation(vec![
                "empty_section_batch".into(),
            ]));
        }
        if sections.len() > MAX_SECTIONS_PER_BATCH {
            return Err(EngineError::AnswerValidation(vec![
                "too_many_sections_in_batch".into(),
            ]));
        }
        let bytes = serde_jcs::to_vec(batch)?;
        if bytes.len() > MAX_BATCH_BYTES {
            return Err(EngineError::AnswerValidation(vec![
                "section_batch_too_large".into(),
            ]));
        }
        let retained = self.composed_section_ids();
        let mut incoming = BTreeSet::new();
        for section in sections {
            let id = section
                .get("section_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            // An empty or repeated id inside one batch is malformed repetition.
            if id.trim().is_empty() || !incoming.insert(id.to_owned()) {
                return Err(EngineError::AnswerValidation(vec![
                    "duplicate_section_id".into(),
                ]));
            }
        }
        if incoming.iter().all(|id| retained.contains(id)) {
            // Every section of this batch is a re-issue: the composer is
            // looping. Reject the batch so the bounded repair lane can name
            // the repetition instead of accumulating a no-op.
            return Err(EngineError::AnswerValidation(vec![
                "duplicate_section_id".into(),
            ]));
        }
        if self.composed_sections_len() + sections.len() > MAX_ANSWER_SECTIONS {
            return Err(EngineError::AnswerValidation(vec![
                "too_many_sections".into(),
            ]));
        }
        Ok(())
    }

    /// Retain one validated report-sections/v1 batch (validate, then push).
    pub(crate) fn retain_composed_section(&mut self, batch: &Value) -> Result<(), EngineError> {
        self.validate_composed_section(batch)?;
        self.composed_sections.push(batch.clone());
        Ok(())
    }

    /// Engine-owned loop bound: one more section turn is dispatched only
    /// while the reserve floor (effective final output reserve + the
    /// minimum research turn) is still available. The four output-reserve
    /// triggers never fire mid-loop; this check IS the loop's budget
    /// termination and uses exactly the threshold trigger 2 protects.
    pub(crate) fn section_loop_may_continue(
        &self,
        image: &AgentImageManifest,
    ) -> Result<bool, EngineError> {
        let Some(reserve) =
            image.effective_final_output_reserve_tokens(&self.program.workflow.id)?
        else {
            return Ok(true);
        };
        let minimum = image
            .body
            .answer_policy
            .minimum_research_turn_tokens
            .unwrap_or_default();
        Ok(self.remaining_output_tokens()? > reserve.saturating_add(minimum))
    }

    /// Merge report-sections/v1 batches in `order_hint` / arrival order:
    /// sections deduplicate by id (first issue wins), claims and
    /// calculations merge by id, and follow-up questions are taken from the
    /// last `final_batch` (the sanitizer's existing rules enforce the exact
    /// policy count at validation time — the assembler never invents any).
    /// Callers assemble from `composed_sections` directly.
    pub(crate) fn assemble_answer_ir_from(
        batches: &[Value],
        policy: &AnswerPolicy,
    ) -> Result<AnswerIr, EngineError> {
        let mut ordered_sections = Vec::new();
        let mut claims: Vec<Claim> = Vec::new();
        let mut calculations: Vec<Calculation> = Vec::new();
        let mut seen_claim_ids = BTreeSet::new();
        let mut seen_calculation_ids = BTreeSet::new();
        let mut follow_up_questions = Vec::new();
        let mut arrival = 0usize;
        for batch in batches {
            if let Some(sections) = batch.get("sections").and_then(Value::as_array) {
                for section in sections {
                    let order_hint = section
                        .get("order_hint")
                        .and_then(Value::as_u64)
                        .unwrap_or_default();
                    ordered_sections.push((order_hint, arrival, section.clone()));
                    arrival = arrival.saturating_add(1);
                }
            }
            for claim in batch
                .get("claims")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or_default()
            {
                let claim: Claim = serde_json::from_value(claim.clone()).map_err(|_| {
                    EngineError::AnswerValidation(vec!["section_claim_unparseable".into()])
                })?;
                if seen_claim_ids.insert(claim.claim_id.clone()) {
                    claims.push(claim);
                }
            }
            for calculation in batch
                .get("calculations")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or_default()
            {
                let calculation: Calculation = serde_json::from_value(calculation.clone())
                    .map_err(|_| {
                        EngineError::AnswerValidation(vec![
                            "section_calculation_unparseable".into(),
                        ])
                    })?;
                if seen_calculation_ids.insert(calculation.calculation_id.clone()) {
                    calculations.push(calculation);
                }
            }
            if batch.get("batch_kind").and_then(Value::as_str) == Some("final_batch") {
                follow_up_questions = batch
                    .get("follow_up_questions")
                    .and_then(Value::as_array)
                    .map(Vec::as_slice)
                    .unwrap_or_default()
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect();
            }
        }
        ordered_sections.sort_by_key(|(order_hint, arrival, _)| (*order_hint, *arrival));
        let mut seen_section_ids = BTreeSet::new();
        let mut sections = Vec::new();
        for (_, _, section) in ordered_sections {
            let section_id = section
                .get("section_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            if !seen_section_ids.insert(section_id.clone()) {
                continue;
            }
            let heading = section
                .get("heading")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            // The assembled IR keeps the batch's own heading; `intent` is the
            // answer-ir's short section label, bounded to its contract limit.
            let intent: String = heading.chars().take(128).collect();
            let claim_ids = section
                .get("claim_ids")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or_default()
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect();
            sections.push(AnswerSection {
                section_id,
                heading,
                intent,
                claim_ids,
                disclosed_uncertainty: None,
            });
        }
        // Defensive cardinality only: a final_batch cannot exceed the
        // contract's three follow-ups, and the sanitizer's existing policy
        // rules (warn/truncate) stay the single enforcement point.
        follow_up_questions.truncate(policy.exact_follow_up_count);
        Ok(AnswerIr {
            schema_version: 2,
            locale: "ko-KR".to_owned(),
            sections,
            claims,
            calculations,
            follow_up_questions,
        })
    }

    pub(crate) fn ingest_scope_projection(
        &mut self,
        call: &PreparedCall,
        result: &CapabilityResult,
    ) -> Result<(), EngineError> {
        if is_input_correction(result) {
            return Ok(());
        }
        let Some(candidate) = derive_result_scope_projection(
            &call.capability,
            &call.capability.id,
            &result.provider_content,
        )?
        else {
            return Ok(());
        };
        match &self.derived_ticker_scope {
            None => self.derived_ticker_scope = Some(candidate),
            Some(existing) if existing == &candidate => {}
            Some(_) => {
                return Err(EngineError::RunScopeViolation(
                    "a second capability attempted to replace the committed derived ticker scope",
                ));
            }
        }
        Ok(())
    }

    pub(crate) fn checkpoint_value(&self) -> Result<ActiveRunCheckpoint, EngineError> {
        let action_cache_hashes = self
            .action_cache
            .iter()
            .map(|(key, value)| {
                serde_jcs::to_vec(value)
                    .map(|bytes| (key.clone(), ContentHash::sha256(bytes)))
                    .map_err(EngineError::from)
            })
            .collect::<Result<BTreeMap<_, _>, _>>()?;
        let accepted_action_commitments = self
            .accepted_actions
            .iter()
            .map(|action| AcceptedActionCommitment {
                action_key: &action.action_key,
                capability_id: &action.capability_id,
                input_contract: &action.input_contract,
                input_hash: &action.input_hash,
                sealed_input_retained: action.sealed_input.is_some(),
            })
            .collect::<Vec<_>>();
        Ok(ActiveRunCheckpoint {
            schema_version: ACTIVE_RUN_CHECKPOINT_SCHEMA_VERSION,
            interpreter: self.interpreter.checkpoint()?,
            state_trace: self.state_trace.clone(),
            direct_answer_retry_requested: self.direct_answer_retry_requested,
            usage: self.usage.clone(),
            capability_calls: self.capability_calls.clone(),
            completed_capabilities: self.completed_capabilities.clone(),
            logical_action_keys: self.logical_action_keys.clone(),
            conversation_hash: ContentHash::sha256(serde_jcs::to_vec(&self.messages)?),
            evidence_ledger_hash: ContentHash::sha256(serde_jcs::to_vec(&self.ledger)?),
            presentation_packs_hash: ContentHash::sha256(serde_jcs::to_vec(
                &self.presentation_packs,
            )?),
            composed_sections_hash: ContentHash::sha256(serde_jcs::to_vec(
                &self.composed_sections,
            )?),
            calculations_hash: ContentHash::sha256(serde_jcs::to_vec(&self.calculations)?),
            action_cache_hash: ContentHash::sha256(serde_jcs::to_vec(&action_cache_hashes)?),
            accepted_actions_hash: ContentHash::sha256(serde_jcs::to_vec(
                &accepted_action_commitments,
            )?),
            tool_schema_hash: self.tool_schema_hash.clone(),
            prompt_receipt_hashes: self.prompt_receipt_hashes.clone(),
            compaction_receipts: self.compaction_receipts.clone(),
            last_provider_episode_hash: self.last_provider_episode_hash.clone(),
            compacted_context_hash: self
                .compacted_context
                .as_ref()
                .map(|context| context.context_hash.clone()),
            research_planner_hash: self.research_planner.checkpoint_hash()?,
            derived_ticker_scope_hash: self
                .derived_ticker_scope
                .as_ref()
                .map(|scope| serde_jcs::to_vec(scope).map(ContentHash::sha256))
                .transpose()?,
            session_memory_hash: self
                .session_memory
                .as_ref()
                .map(|memory| memory.payload_hash.clone()),
        })
    }

    pub(crate) fn checkpoint_bytes(&self) -> Result<Vec<u8>, EngineError> {
        Ok(serde_jcs::to_vec(&self.checkpoint_value()?)?)
    }
}

/// The context-plan capabilities whose append proposals are bounded by the
/// root SearchPlan clause cap. Both discovery lanes stop the same way: the
/// company lane's `ontology.query_context` and the free door's
/// `ontology.query_context_universe`. The universe variant was originally
/// missing from the hardcoded check (2026-09-04 live postmortem: question-only
/// runs died terminally on append rejections instead of taking the
/// bounded-answer stop edge), so the accepted set is explicit and tested.
pub(crate) fn is_append_context_plan_provider_tool(function_name: &str) -> bool {
    [
        provider_tool_name("ontology.query_context"),
        provider_tool_name("ontology.query_context_universe"),
    ]
    .iter()
    .any(|accepted| accepted.as_str() == function_name)
}
