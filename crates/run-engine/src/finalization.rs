//! Final answer policy: parse, integrity-validate, sanitize, render, and
//! atomically commit the core answer.  Presentation packs are compiled here
//! but are optional and never fail the text answer.

use super::*;
use krw_agent_evidence::{
    ClaimKind, ClaimStrength, MAX_ANSWER_CALCULATIONS, MAX_ANSWER_CLAIMS, MAX_ANSWER_SECTIONS,
    MAX_CLAIMS_PER_SECTION,
};
use krw_agent_planning::GoalStatus;
use krw_agent_research_planner::IntentPlanningProjection;

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

/// Issue classes that protect ledger/contract integrity rather than answer
/// presentation. These map to the policy's `IntegrityFailure` list and remain
/// hard failures after sanitization:
///
/// - `untrusted_calculation`, `calculation_mismatch`,
///   `calculation_unknown_evidence`: the answer's calculation array claims
///   lineage the kernel never committed ("uncommitted calculation lineage").
/// - `number_not_equal_to_calculation` **with at least one referenced
///   calculation resolving in the ledger**: the numeric assertion diverges
///   from a committed calculation output (a forged derived value). The same
///   code fires vacuously for numeric claims with no lineage at all (the
///   validator's `any` over zero referenced calculations), which is the
///   policy's missing-lineage quality class and downgrades to a fact.
/// - `unsupported_answer_schema`: the payload is not the pinned AnswerIR
///   contract version (a contract-pin-level violation; the canonical schema
///   makes this unreachable in practice).
///
/// Everything else `validate_answer` reports is a presentation/quality
/// defect: it must degrade the answer (`AcceptedWithWarnings`), never produce
/// `run.failed`.
fn issue_is_integrity(issue: &ValidationIssue, answer: &AnswerIr, ledger: &EvidenceLedger) -> bool {
    match issue.code {
        "unsupported_answer_schema"
        | "untrusted_calculation"
        | "calculation_mismatch"
        | "calculation_unknown_evidence"
        // Process leaks and wrong-script answers disclose internals or
        // break the locale contract; both require a bounded compose repair
        // (measured 2026-08-31 GLM smoke) rather than a silent downgrade.
        | "answer_control_payload_leak"
        | "answer_language_mismatch" => true,
        "number_not_equal_to_calculation" => issue
            .claim_id
            .as_deref()
            .and_then(|claim_id| {
                answer
                    .claims
                    .iter()
                    .find(|claim| claim.claim_id == claim_id)
            })
            .is_some_and(|claim| {
                claim
                    .calculation_ids
                    .iter()
                    .any(|calculation_id| ledger.calculation(calculation_id).is_some())
            }),
        _ => false,
    }
}

/// Answer-always finalization gate.
///
/// Policy mapping (doc/refetoring/03-runtime-failure-policy.md, sanitizer
/// steps 1-8; step 9's ledger fallback is intentionally NOT implemented
/// here):
///
/// 1. Keep only parseable sections/claims — enforced upstream by the pinned
///    canonical contract plus `serde` deserialization; this function only
///    ever sees a structurally parseable `AnswerIr`. Bounded-cardinality
///    overflows (`too_many_*`) are truncated to the validator's own limits.
/// 2. Claims referencing evidence outside the current ledger
///    (`unknown_evidence`, `unknown_counter_evidence`) lose the reference;
///    a grounded claim left with no evidence at all is dropped.
/// 3. Numeric claims without usable calculation lineage
///    (`number_without_*`, `unknown_calculation`,
///    `calculation_evidence_not_cited`) are downgraded from `number` to a
///    grounded `fact` claim: the numeric assertion is removed, nothing is
///    re-derived.
/// 4. Interpretations without a counter-signal are downgraded to `fact`
///    (`interpretation_without_counter_signal`).
/// 5. Unsupported strength bindings (`*_strong_claim_not_allowed`,
///    `strong_claim_without_required_directness`) are recomputed to the
///    evidence's actual support: `strong` softens to `qualified`. Kernel
///    goal bindings stay hard-validated in `validate_kernel_goal_bindings`.
/// 6. Sections that lose all claims (and empty sections generally) are
///    dropped, as are claims no longer rendered by any section
///    (`unrendered_claim`).
/// 7. Locale and headings are presentation metadata and are normalized
///    (`answer_locale_mismatch`, `invalid_section_heading`).
/// 8. Follow-up questions keep only valid ones, truncated to the policy
///    count when too many; a shortfall is committed as-is because the
///    sanitizer never invents questions.
///
/// The sanitizer never invents evidence and never promotes a claim: it only
/// drops or weakens model-authored material.
pub(crate) fn sanitize_answer(
    answer: &AnswerIr,
    ledger: &EvidenceLedger,
    policy: &AnswerPolicy,
) -> Result<(AnswerIr, ResearchCompletion), Vec<ValidationIssue>> {
    let issues = match validate_answer(answer, ledger, policy) {
        Ok(()) => return Ok((answer.clone(), ResearchCompletion::Accepted)),
        Err(issues) => issues,
    };
    // Integrity violations keep today's terminal failure path. The full issue
    // list is returned so the repair-feedback taxonomy is unchanged.
    if issues
        .iter()
        .any(|issue| issue_is_integrity(issue, answer, ledger))
    {
        return Err(issues);
    }
    let sanitized = sanitize_answer_ir(answer, ledger, policy, &issues);
    // Policy step 9 (deterministic ledger fallback) is the NEXT task: when
    // sanitization leaves no answerable material at all, keep the existing
    // failure path instead of committing an empty answer.
    if sanitized.sections.is_empty() || sanitized.claims.is_empty() {
        return Err(issues);
    }
    // Re-validate the degraded answer. Only integrity classes may still fail
    // the commit; residual quality issues (e.g. a follow-up count below the
    // exact policy count, which cannot be padded without inventing
    // questions) are committed together with the warning completion class.
    if let Err(residual) = validate_answer(&sanitized, ledger, policy)
        && residual
            .iter()
            .any(|issue| issue_is_integrity(issue, &sanitized, ledger))
    {
        return Err(residual);
    }
    Ok((sanitized, ResearchCompletion::AcceptedWithWarnings))
}

const SANITIZER_HEADING_FALLBACKS: [&str; 4] = ["결론", "근거", "반대 신호", "확인 조건"];

fn sanitize_answer_ir(
    answer: &AnswerIr,
    ledger: &EvidenceLedger,
    policy: &AnswerPolicy,
    issues: &[ValidationIssue],
) -> AnswerIr {
    let mut sanitized = answer.clone();
    let codes_for_claim = |claim_id: &str| {
        issues
            .iter()
            .filter(|issue| issue.claim_id.as_deref() == Some(claim_id))
            .map(|issue| issue.code)
            .collect::<Vec<_>>()
    };

    // Step 7: locale is presentation metadata pinned by the contract.
    if issues
        .iter()
        .any(|issue| issue.code == "answer_locale_mismatch")
    {
        sanitized.locale = "ko-KR".to_owned();
    }

    // Step 1 (bounded cardinality): retain a valid, unique, bounded
    // calculation prefix. Integrity-class calculation defects already failed
    // above, so this only trims cardinality/shape defects.
    if issues.iter().any(|issue| {
        matches!(
            issue.code,
            "too_many_calculations" | "empty_calculation_id" | "duplicate_calculation_id"
        )
    }) {
        let mut seen = BTreeSet::new();
        sanitized.calculations = sanitized
            .calculations
            .iter()
            .filter(|calculation| {
                !calculation.calculation_id.trim().is_empty()
                    && seen.insert(calculation.calculation_id.clone())
            })
            .take(MAX_ANSWER_CALCULATIONS)
            .cloned()
            .collect();
    }
    let retained_calculation_ids = sanitized
        .calculations
        .iter()
        .map(|calculation| calculation.calculation_id.clone())
        .collect::<BTreeSet<_>>();

    // Steps 2-5: per-claim drop / reference-trim / soften / downgrade.
    let mut dropped_claims = BTreeSet::new();
    for claim in &mut sanitized.claims {
        let codes = codes_for_claim(&claim.claim_id);
        // Claims whose identifier, grounding, or public text is unusable are
        // dropped whole. A duplicated claim_id drops every copy (the issue
        // cannot distinguish the first occurrence, and keeping an ambiguous
        // identifier would risk cross-claim citation confusion).
        if codes.iter().any(|code| {
            matches!(
                *code,
                "empty_claim_id"
                    | "duplicate_claim_id"
                    | "invalid_claim_shape"
                    | "claim_has_no_evidence"
                    | "internal_term_exposed"
                    | "invalid_claim_text"
                    | "invalid_citation_text"
            )
        }) {
            dropped_claims.insert(claim.claim_id.clone());
            continue;
        }
        // Step 2: drop references to evidence that is not in the current
        // ledger; mirror the same rule for counter-evidence.
        claim
            .evidence_ids
            .retain(|evidence_id| ledger.active(evidence_id).is_some());
        claim
            .counter_evidence_ids
            .retain(|evidence_id| ledger.active(evidence_id).is_some());
        if claim.kind != ClaimKind::Uncertainty && claim.evidence_ids.is_empty() {
            dropped_claims.insert(claim.claim_id.clone());
            continue;
        }
        // Step 5: unsupported strength bindings recompute to actual support.
        if codes.iter().any(|code| {
            matches!(
                *code,
                "global_strong_claim_not_allowed"
                    | "strong_claim_not_allowed"
                    | "strong_claim_without_required_directness"
            )
        }) {
            claim.strength = ClaimStrength::Qualified;
        }
        // Step 3: numeric assertions without usable committed lineage lose
        // the numeric form; step 4: interpretations without a counter-signal
        // downgrade to facts. `number_not_equal_to_calculation` reaches this
        // list only in its vacuous form (no referenced calculation resolved
        // in the ledger): the diverging-lineage form already failed as an
        // integrity violation above.
        let numeric_without_lineage = codes.iter().any(|code| {
            matches!(
                *code,
                "number_without_value"
                    | "number_without_identity"
                    | "number_without_unit"
                    | "number_without_period"
                    | "number_without_calculation"
                    | "unknown_calculation"
                    | "calculation_evidence_not_cited"
                    | "number_not_equal_to_calculation"
            )
        }) || claim
            .calculation_ids
            .iter()
            .any(|calculation_id| !retained_calculation_ids.contains(calculation_id));
        if numeric_without_lineage && claim.kind == ClaimKind::Number {
            claim.kind = ClaimKind::Fact;
            claim.calculation_ids.clear();
        }
        // Step 4: once counter-evidence references have been trimmed, an
        // interpretation without any surviving counter-signal downgrades to
        // a fact. The condition is state-based: the validator's issue code
        // only exists for the pre-trim shape, and the engine-side
        // normalizer already applies the same rule before validation.
        if policy.require_counter_signal_for_interpretation
            && claim.kind == ClaimKind::Interpretation
            && claim.counter_evidence_ids.is_empty()
        {
            claim.kind = ClaimKind::Fact;
        }
    }
    if issues.iter().any(|issue| issue.code == "too_many_claims") {
        for claim in sanitized.claims.iter().skip(MAX_ANSWER_CLAIMS) {
            dropped_claims.insert(claim.claim_id.clone());
        }
        sanitized.claims.truncate(MAX_ANSWER_CLAIMS);
    }
    sanitized
        .claims
        .retain(|claim| !dropped_claims.contains(&claim.claim_id));

    // Steps 6-7 plus section-level shape repairs. Uncertainty must be a
    // typed claim, so free-text disclosures are cleared wherever they
    // appear (the issue carries no stable section identifier).
    let clear_uncertainty_text = issues
        .iter()
        .any(|issue| issue.code == "free_uncertainty_text_forbidden");
    let mut seen_section_ids = BTreeSet::new();
    let mut sections = Vec::new();
    for (index, section) in sanitized.sections.iter_mut().enumerate() {
        if issues
            .iter()
            .any(|issue| issue.code == "invalid_section_id")
            && (section.section_id.trim().is_empty()
                || !section_identifier_is_valid(&section.section_id)
                || !seen_section_ids.insert(section.section_id.clone()))
        {
            continue;
        }
        seen_section_ids.insert(section.section_id.clone());
        if clear_uncertainty_text {
            section.disclosed_uncertainty = None;
        }
        if issues
            .iter()
            .any(|issue| issue.code == "invalid_section_heading")
        {
            section.heading = SANITIZER_HEADING_FALLBACKS.get(index).map_or_else(
                || format!("핵심 판단 {}", index + 1),
                |value| (*value).to_owned(),
            );
        }
        // Drop references to claims that no longer exist (including claims
        // dropped above) and dedupe within the section, mirroring
        // `duplicate_section_claim` / `section_unknown_claim`.
        let mut local = BTreeSet::new();
        section.claim_ids.retain(|claim_id| {
            sanitized
                .claims
                .iter()
                .any(|claim| &claim.claim_id == claim_id)
                && local.insert(claim_id.clone())
        });
        if issues
            .iter()
            .any(|issue| issue.code == "too_many_section_claims")
        {
            section.claim_ids.truncate(MAX_CLAIMS_PER_SECTION);
        }
        // Step 6: a section with nothing left to say is empty presentation.
        if !section.claim_ids.is_empty() {
            sections.push(section.clone());
        }
    }
    sanitized.sections = sections;
    if issues.iter().any(|issue| issue.code == "too_many_sections") {
        sanitized.sections.truncate(MAX_ANSWER_SECTIONS);
    }

    // Step 6 (reverse direction): claims no longer rendered by any section
    // are not answer material.
    let rendered = sanitized
        .sections
        .iter()
        .flat_map(|section| section.claim_ids.iter().cloned())
        .collect::<BTreeSet<_>>();
    sanitized
        .claims
        .retain(|claim| rendered.contains(&claim.claim_id));

    // Step 8: keep only valid follow-up questions, bounded by the policy
    // count when too many. A shortfall is left as-is; the sanitizer never
    // invents questions.
    if issues
        .iter()
        .any(|issue| issue.code == "invalid_follow_up_question")
    {
        sanitized
            .follow_up_questions
            .retain(|question| follow_up_is_presentable(question, policy));
    }
    if issues
        .iter()
        .any(|issue| issue.code == "follow_up_count_mismatch")
        && sanitized.follow_up_questions.len() > policy.exact_follow_up_count
    {
        sanitized
            .follow_up_questions
            .truncate(policy.exact_follow_up_count);
    }
    sanitized
}

/// Mirror of the evidence crate's public-text rule for follow-up questions
/// (shape, bounded length, terminal question mark, no internal terms).
fn follow_up_is_presentable(question: &str, policy: &AnswerPolicy) -> bool {
    let trimmed = question.trim();
    !trimmed.is_empty()
        && question.len() <= 300
        && trimmed.ends_with('?')
        && !question.chars().any(|character| {
            character == '\0'
                || character == '\n'
                || character == '\r'
                || (character.is_control() && character != '\t')
        })
        && !policy
            .forbidden_terms
            .iter()
            .any(|term| question.to_lowercase().contains(&term.to_lowercase()))
}

/// Mirror of the evidence crate's bounded identifier rule for section ids.
fn section_identifier_is_valid(section_id: &str) -> bool {
    !section_id.is_empty()
        && section_id.len() <= 128
        && section_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
}

// ---------------------------------------------------------------------------
// Deterministic content gates for the direct Markdown lane.
// ---------------------------------------------------------------------------

/// Phrases that, appearing in the opening sentence with no directional or
/// judgment wording alongside, mean the answer framed itself as an
/// unavailability instead of taking an analyst position.
const UNAVAILABILITY_OPENING_PHRASES: [&str; 21] = [
    "확인할 수 없",
    "확인할수없",
    "확인이 불가",
    "확인할 방법이 없",
    "파악할 수 없",
    "알 수 없",
    "알수없",
    "공개되지 않",
    "공개하고 있지 않",
    "공시되지 않",
    "공시는 없",
    "공시가 없",
    "제공되지 않",
    "밝히지 않",
    "나와 있지 않",
    "단정할 수 없",
    "내리기 어렵",
    "불가능합니다",
    "cannot be determined",
    "could not be determined",
    "unable to determine",
];

/// Directional/judgment wording whose presence in the opening sentence means
/// a limitation phrase there is subordinate to a stated position, not the
/// answer's frame.
const OPENING_JUDGMENT_MARKERS: [&str; 34] = [
    "가능성",
    "판단",
    "보입",
    "보여",
    "전망",
    "추정",
    "방향",
    "상승",
    "하락",
    "증가",
    "감소",
    "성장",
    "견고",
    "부진",
    "개선",
    "악화",
    "늘어",
    "줄어",
    "커",
    "작아",
    "높아",
    "낮아",
    "강세",
    "약세",
    "우려",
    "기대",
    "likely",
    "suggest",
    "expect",
    "estimate",
    "growth",
    "decline",
    "increase",
    "decrease",
];

/// The first user-facing sentence: heading-only lines are skipped, decimals
/// ("6.56B") do not end a sentence, and the scan is bounded so a wall of
/// unpunctuated text cannot bypass the gate.
fn first_markdown_sentence(content: &str) -> String {
    let mut paragraph: Option<&str> = None;
    for line in content.trim_start().lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with('#') && trimmed.trim_start_matches('#').starts_with(' ') {
            continue;
        }
        paragraph = Some(trimmed);
        break;
    }
    let paragraph = paragraph.unwrap_or_default();
    let mut sentence = String::new();
    let mut chars = paragraph.chars().peekable();
    while let Some(character) = chars.next() {
        sentence.push(character);
        let ends_sentence = matches!(character, '。' | '！' | '？' | '!' | '?' | '\n')
            || (character == '.' && !chars.peek().is_some_and(|next| next.is_ascii_digit()));
        if ends_sentence || sentence.chars().count() >= 200 {
            break;
        }
    }
    sentence.trim().to_owned()
}

/// ASCII identifiers match on word boundaries ("chain" must not fire on
/// "supply chain"); non-ASCII terms match as substrings.
fn markdown_contains_forbidden_term(lower: &str, term: &str) -> bool {
    let word_term = !term.is_empty()
        && term
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_');
    if !word_term {
        return lower.contains(term);
    }
    let mut search_from = 0usize;
    while let Some(found) = lower[search_from..].find(term) {
        let start = search_from + found;
        let end = start + term.len();
        let bounded_before = lower[..start]
            .chars()
            .next_back()
            .is_none_or(|character| !(character.is_ascii_alphanumeric() || character == '_'));
        let bounded_after = lower[end..]
            .chars()
            .next()
            .is_none_or(|character| !(character.is_ascii_alphanumeric() || character == '_'));
        if bounded_before && bounded_after {
            return true;
        }
        search_from = end;
    }
    false
}

/// Retry feedback text when the direct Markdown draft fails a deterministic
/// content gate, or `None` when it passes. The gate is deliberately narrow:
/// internal vocabulary anywhere, and an unavailability phrase in the opening
/// sentence without a direction stated alongside. Limitations elsewhere in
/// the answer are required behavior and never gated.
/// Short lowercase tool names ("chain" = `ontology.chain`, "trace") are also
/// common English words. The leak this list guards against is the TOOL
/// reference, so only tool-usage patterns gate (`ontology.chain`, "chain
/// 호출", "chain call", "chain(") while plain English usage such as "supply
/// chain" passes.
fn markdown_contains_tool_reference(lower: &str, term: &str) -> bool {
    if lower.contains(&format!("ontology.{term}")) {
        return true;
    }
    [
        " 호출", " 콜", " call", " tool", " 도구", " 사용", " 결과", " 쿼리", " query", "(",
    ]
    .iter()
    .any(|suffix| lower.contains(&format!("{term}{suffix}")))
}

pub(crate) fn markdown_content_gate_feedback(
    content: &str,
    forbidden_terms: &[String],
) -> Option<String> {
    let lower = content.to_lowercase();
    for term in forbidden_terms {
        let term_lower = term.trim().to_lowercase();
        if term_lower.is_empty() {
            continue;
        }
        // Short lowercase alphabetic terms are ambiguous between tool names
        // and common English words; only their tool-reference forms gate.
        let internal_shape = term
            .chars()
            .any(|character| character.is_uppercase() || character == '_')
            || !term
                .chars()
                .all(|character| character.is_ascii_alphabetic() || character == ' ');
        if !internal_shape && term.len() < 6 {
            if markdown_contains_tool_reference(&lower, &term_lower) {
                return Some(format!(
                    "internal tool name \"{term}\" appeared in user-facing prose."
                ));
            }
            continue;
        }
        if markdown_contains_forbidden_term(&lower, &term_lower) {
            return Some(format!(
                "internal term \"{term}\" appeared in user-facing prose."
            ));
        }
    }
    let opening = first_markdown_sentence(content);
    if opening.is_empty() {
        return None;
    }
    let opening_lower = opening.to_lowercase();
    let limitation = UNAVAILABILITY_OPENING_PHRASES
        .iter()
        .find(|phrase| opening_lower.contains(*phrase));
    if let Some(phrase) = limitation {
        let has_direction = OPENING_JUDGMENT_MARKERS
            .iter()
            .any(|marker| opening_lower.contains(marker));
        if !has_direction {
            return Some(format!(
                "the opening sentence frames the answer as unavailability (\"{phrase}\") without stating a direction."
            ));
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Ledger fallback (answer-always finalization, policy steps 9 and the
// `UnavailableButAnswerable` completion class).
// ---------------------------------------------------------------------------

/// Deterministic reason codes for the no-evidence ledger fallback. The policy
/// also names `not_disclosed`, `not_indexed`, and `out_of_scope`; those need a
/// server-side coverage verdict the engine cannot derive without a working
/// dependency, so the renderer never invents them.
pub(crate) const LEDGER_FALLBACK_DEPENDENCY_UNAVAILABLE: &str = "dependency_unavailable";
pub(crate) const LEDGER_FALLBACK_RETRIEVAL_EMPTY: &str = "retrieval_empty";
/// The run's own output-token budget ran out before an answer turn could
/// complete. Distinct from `dependency_unavailable` because nothing external
/// failed: telling the user "provider/MCP 문제" for a budget exhaustion is a
/// false diagnosis.
pub(crate) const LEDGER_FALLBACK_OUTPUT_BUDGET_EXHAUSTED: &str = "output_budget_exhausted";

const LEDGER_FALLBACK_MAX_RECORDS: usize = 16;
const LEDGER_FALLBACK_MAX_FACTS_PER_RECORD: usize = 8;
const LEDGER_FALLBACK_MAX_CALCULATIONS: usize = 8;
const LEDGER_FALLBACK_MAX_GOALS: usize = 8;

/// Deterministic ledger-fallback answer: the Markdown bytes are a pure
/// function of the admitted ledger, committed calculations, user-linked
/// intent projection, and the reason classification. No LLM turn, no clock,
/// no run identity — nothing that could vary between two identical runs.
pub(crate) struct LedgerFallbackAnswer {
    pub(crate) markdown: String,
    /// Ledger evidence ids actually cited by the rendering; they become the
    /// bundle's audit index, so the renderer owns the exact set.
    pub(crate) cited_evidence_ids: Vec<String>,
    pub(crate) reason_code: &'static str,
}

/// Classify why no evidence exists: a dependency that never delivered
/// (`dependency_unavailable`) versus retrievals that ran and committed
/// results without admitting any evidence (`retrieval_empty`).
fn ledger_fallback_reason_code(
    ledger: &EvidenceLedger,
    accepted_capability_results: usize,
) -> &'static str {
    if ledger.is_empty() && accepted_capability_results > 0 {
        LEDGER_FALLBACK_RETRIEVAL_EMPTY
    } else {
        LEDGER_FALLBACK_DEPENDENCY_UNAVAILABLE
    }
}

/// Sanitize a string for the public fallback Markdown. Unlike the evidence
/// crate's strict inline writer this is total: control characters are dropped
/// instead of failing the deterministic render.
fn fallback_public_inline(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for character in value.trim().chars() {
        if character.is_control() {
            continue;
        }
        if matches!(
            character,
            '\\' | '`' | '*' | '_' | '[' | ']' | '<' | '>' | '#' | '|'
        ) {
            output.push('\\');
        }
        output.push(character);
    }
    output
}

fn fallback_json_value(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_owned())
}

struct FallbackCitations {
    order: Vec<String>,
    numbers: BTreeMap<String, usize>,
}

impl FallbackCitations {
    fn new() -> Self {
        Self {
            order: Vec::new(),
            numbers: BTreeMap::new(),
        }
    }

    fn mark(&mut self, evidence_id: &str) -> Option<String> {
        if evidence_id.trim().is_empty() {
            return None;
        }
        if let Some(number) = self.numbers.get(evidence_id) {
            return Some(format!("[^{number}]"));
        }
        let number = self.order.len() + 1;
        self.order.push(evidence_id.to_owned());
        self.numbers.insert(evidence_id.to_owned(), number);
        Some(format!("[^{number}]"))
    }
}

fn fallback_fact_line(
    record: &krw_agent_evidence::EvidenceRecord,
    fact: &krw_agent_evidence::NormalizedFact,
    citations: &mut FallbackCitations,
) -> Option<String> {
    let subject = fallback_public_inline(&fact.subject);
    let predicate = fallback_public_inline(&fact.predicate);
    if subject.is_empty() || predicate.is_empty() {
        return None;
    }
    let period = fact
        .period
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .or(record.period.as_deref());
    let unit = fact
        .unit
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .map(fallback_public_inline);
    let mut line = format!(
        "- {subject}: {predicate} = {}",
        fallback_json_value(&fact.value)
    );
    if let Some(unit) = unit {
        line.push(' ');
        line.push_str(&unit);
    }
    if let Some(period) = period {
        let period = fallback_public_inline(period);
        if !period.is_empty() {
            line.push_str(&format!(" · {period}"));
        }
    }
    if let Some(mark) = citations.mark(&record.evidence_id) {
        line.push(' ');
        line.push_str(&mark);
    }
    Some(line)
}

/// Render the deterministic ledger fallback answer (Korean, matching the
/// product answer locale). Sections with no material are omitted so an empty
/// ledger produces only the unavailability notice plus the question scope.
pub(crate) fn fallback_answer_from_ledger(
    question: &str,
    tickers: &[String],
    ledger: &EvidenceLedger,
    calculations: &BTreeMap<String, Calculation>,
    intent: Option<&IntentPlanningProjection>,
    accepted_capability_results: usize,
    output_budget_exhausted: bool,
) -> LedgerFallbackAnswer {
    let reason_code = if output_budget_exhausted {
        LEDGER_FALLBACK_OUTPUT_BUDGET_EXHAUSTED
    } else {
        ledger_fallback_reason_code(ledger, accepted_capability_results)
    };
    let mut citations = FallbackCitations::new();
    let mut markdown = String::new();

    markdown.push_str("## 안내\n\n");
    if output_budget_exhausted {
        markdown.push_str(
            "요청하신 연구가 응답 예산(출력 토큰) 소진으로 완료되지 못했습니다. \
이미 검증된 자료만으로 제한적 요약을 남깁니다.\n\n",
        );
    } else {
        markdown.push_str(
            "요청하신 연구가 기한 내 완료되지 못했습니다. 외부 연결(provider/MCP) 문제가 복구 기한 안에 해결되지 않아, \
정상 답변 대신 이미 검증된 자료만으로 제한적 요약을 남깁니다.\n\n",
        );
    }
    markdown.push_str(&format!("- 사유 코드: {reason_code}\n"));
    if output_budget_exhausted {
        markdown.push_str("- 이는 시스템 응답 예산 문제이며 기업의 공시 범위와 무관합니다.\n");
    } else {
        markdown.push_str("- 이는 시스템 의존성 문제이며 기업의 공시 범위와 무관합니다.\n");
    }

    markdown.push_str("\n## 질문 범위\n\n");
    markdown.push_str(&format!("- 질문: {}\n", fallback_public_inline(question)));
    let tickers_inline = tickers
        .iter()
        .map(|ticker| fallback_public_inline(ticker))
        .filter(|ticker| !ticker.is_empty())
        .collect::<Vec<_>>()
        .join(", ");
    if !tickers_inline.is_empty() {
        markdown.push_str(&format!("- 대상 종목: {tickers_inline}\n"));
    }

    let direct_records = ledger
        .iter()
        .filter(|(_, record)| {
            matches!(
                record.directness,
                krw_agent_evidence::Directness::Direct
                    | krw_agent_evidence::Directness::MetricLineage
            )
        })
        .take(LEDGER_FALLBACK_MAX_RECORDS)
        .collect::<Vec<_>>();
    let related_records = ledger
        .iter()
        .filter(|(_, record)| record.directness == krw_agent_evidence::Directness::Related)
        .take(LEDGER_FALLBACK_MAX_RECORDS)
        .collect::<Vec<_>>();

    let mut direct_lines = Vec::new();
    for (_, record) in &direct_records {
        for fact in record
            .facts
            .iter()
            .take(LEDGER_FALLBACK_MAX_FACTS_PER_RECORD)
        {
            if let Some(line) = fallback_fact_line(record, fact, &mut citations) {
                direct_lines.push(line);
            }
        }
    }
    if !direct_lines.is_empty() {
        markdown.push_str("\n## 확보된 핵심 사실 (직접 근거)\n\n");
        for line in direct_lines {
            markdown.push_str(&line);
            markdown.push('\n');
        }
    }

    let mut related_lines = Vec::new();
    for (_, record) in &related_records {
        for fact in record
            .facts
            .iter()
            .take(LEDGER_FALLBACK_MAX_FACTS_PER_RECORD)
        {
            if let Some(mut line) = fallback_fact_line(record, fact, &mut citations) {
                line.push_str(" · 간접 근거이므로 참고 수준으로만 반영");
                related_lines.push(line);
            }
        }
    }
    if !related_lines.is_empty() {
        markdown.push_str("\n## 제한적 시사점 (간접 근거)\n\n");
        for line in related_lines {
            markdown.push_str(&line);
            markdown.push('\n');
        }
    }

    let mut calculation_lines = Vec::new();
    for (_, calculation) in calculations.iter().take(LEDGER_FALLBACK_MAX_CALCULATIONS) {
        let metric = calculation
            .label
            .as_deref()
            .map(fallback_public_inline)
            .filter(|label| !label.is_empty())
            .or_else(|| {
                calculation
                    .metric
                    .as_deref()
                    .map(fallback_public_inline)
                    .filter(|metric| !metric.is_empty())
            })
            .unwrap_or_else(|| fallback_public_inline(&calculation.calculation_id));
        let mut line = format!("- {metric}");
        if let Some(subject) = calculation
            .subject
            .as_deref()
            .map(fallback_public_inline)
            .filter(|subject| !subject.is_empty())
        {
            line.push_str(&format!(" ({subject}"));
            if let Some(period) = calculation
                .period
                .as_deref()
                .map(fallback_public_inline)
                .filter(|period| !period.is_empty())
            {
                line.push_str(&format!(", {period}"));
            }
            line.push(')');
        }
        line.push_str(&format!(": {}", fallback_json_value(&calculation.output)));
        if let Some(unit) = calculation
            .unit
            .as_deref()
            .map(fallback_public_inline)
            .filter(|unit| !unit.is_empty())
        {
            line.push(' ');
            line.push_str(&unit);
        }
        if let Some(input) = calculation
            .input_evidence_ids
            .first()
            .and_then(|id| ledger.active(id))
            .and_then(|record| citations.mark(&record.evidence_id))
        {
            line.push(' ');
            line.push_str(&input);
        }
        calculation_lines.push(line);
    }
    if !calculation_lines.is_empty() {
        markdown.push_str("\n## 계산 지표 (검증된 계산)\n\n");
        for line in calculation_lines {
            markdown.push_str(&line);
            markdown.push('\n');
        }
    }

    if let Some(intent) = intent {
        let uncovered = intent
            .graph
            .goals()
            .filter(|goal| goal.status != GoalStatus::Satisfied)
            .take(LEDGER_FALLBACK_MAX_GOALS)
            .map(|goal| fallback_public_inline(&goal.goal_id))
            .filter(|goal_id| !goal_id.is_empty())
            .collect::<Vec<_>>();
        if !uncovered.is_empty() {
            markdown.push_str("\n## 다루지 못한 목표\n\n");
            for goal_id in uncovered {
                markdown.push_str(&format!(
                    "- 목표 {goal_id} 는 이번 실행에서 충족되지 못했습니다.\n"
                ));
            }
        }
    }

    markdown.push_str("\n## 제약 및 조회 상태\n\n");
    markdown.push_str(&format!(
        "- 이번 실행에서 확보한 근거: {}건, 완료된 외부 조회: {}건\n",
        ledger.len(),
        accepted_capability_results
    ));
    markdown.push_str(
        "- 연구가 완료되지 않았으므로 위 내용은 부분 자료이며, 완전한 답변이 아닙니다.\n",
    );

    let periods = ledger
        .iter()
        .filter_map(|(_, record)| record.period.as_deref())
        .filter(|period| !period.trim().is_empty())
        .map(fallback_public_inline)
        .collect::<BTreeSet<_>>();
    let as_of = ledger
        .iter()
        .filter_map(|(_, record)| record.as_of.as_deref())
        .filter(|as_of| !as_of.trim().is_empty())
        .map(fallback_public_inline)
        .collect::<BTreeSet<_>>();
    if !periods.is_empty() || !as_of.is_empty() {
        markdown.push_str("\n## 자료 시점\n\n");
        if !periods.is_empty() {
            markdown.push_str(&format!(
                "- 근거 기간: {}\n",
                periods.into_iter().collect::<Vec<_>>().join(", ")
            ));
        }
        if !as_of.is_empty() {
            markdown.push_str(&format!(
                "- 자료 기준일: {}\n",
                as_of.into_iter().collect::<Vec<_>>().join(", ")
            ));
        }
        markdown.push_str("- 나열된 기간 이후 상황은 반영되지 않았습니다.\n");
    }

    if !citations.order.is_empty() {
        markdown.push_str("\n### 출처\n\n");
        for (index, evidence_id) in citations.order.iter().enumerate() {
            let Some(record) = ledger.active(evidence_id) else {
                continue;
            };
            let mut line = format!(
                "[^{}]: {}",
                index + 1,
                fallback_public_inline(&record.citation.title)
            );
            if let Some(document_type) = record
                .citation
                .document_type
                .as_deref()
                .map(fallback_public_inline)
                .filter(|document_type| !document_type.is_empty())
            {
                line.push_str(&format!(" · {document_type}"));
            }
            if let Some(period) = record
                .citation
                .period
                .as_deref()
                .map(fallback_public_inline)
                .filter(|period| !period.is_empty())
            {
                line.push_str(&format!(" · {period}"));
            }
            markdown.push_str(&line);
            markdown.push('\n');
        }
    }

    LedgerFallbackAnswer {
        markdown,
        cited_evidence_ids: citations.order,
        reason_code,
    }
}

/// Terminal error classes that may take the deterministic ledger fallback
/// instead of failing the run: provider/MCP/dependency outages and budget
/// exhaustion — including the global budget classes enforced by
/// `BudgetUsage::ensure_within`, which escape as
/// `EngineError::Contract(ContractError::BudgetExceeded)`. Everything else —
/// integrity violations, cancellation, stale fences, ambiguous commits, and
/// provider-output contract violations — keeps
/// today's terminal failure. A dependency failure on `persistence.commit_final`
/// itself is deliberately excluded: the real answer composition already
/// succeeded there, and replacing it with a notice commit would destroy a
/// better answer over a transient storage fault.
pub(crate) fn error_allows_ledger_fallback(error: &EngineError) -> bool {
    match error {
        EngineError::Dependency { component, .. } => *component != "persistence.commit_final",
        EngineError::DeadlineExceeded(_) => true,
        EngineError::NoRemainingOutputBudget
        | EngineError::FinalOutputReserveReached
        | EngineError::CapabilityBudgetExceeded { .. } => true,
        EngineError::Contract(krw_agent_protocol::ContractError::BudgetExceeded { .. }) => true,
        _ => false,
    }
}

/// Whether the ledger fallback was triggered by the run's own output budget
/// running dry rather than an external dependency fault. The user-facing
/// notice must not blame the provider/MCP transport for a budget exhaustion.
pub(crate) fn error_is_output_budget_exhaustion(error: &EngineError) -> bool {
    matches!(
        error,
        EngineError::NoRemainingOutputBudget
            | EngineError::FinalOutputReserveReached
            | EngineError::CapabilityBudgetExceeded { .. }
            | EngineError::Contract(krw_agent_protocol::ContractError::BudgetExceeded { .. })
    )
}

/// Drop unknown keys from typed answer elements before the strict parse.
/// Composers occasionally add one extra descriptive field to a claim or
/// section (`interpretation`, `note`, …); the strict `deny_unknown_fields`
/// parse would burn a repair turn on each such invention. Required fields
/// still fail loudly — only additive keys are removed (2026-09-02 EN loop:
/// two live failures, `answer_ir` wrapper then a claim `interpretation`).
fn strip_unknown_typed_answer_fields(output: &mut Value) {
    const CLAIM_KEYS: [&str; 14] = [
        "claim_id", "kind", "strength", "text", "goal_ids", "evidence_ids",
        "counter_evidence_ids", "calculation_ids", "subject", "predicate",
        "value", "unit", "period", "comparison_basis",
    ];
    const SECTION_KEYS: [&str; 5] = [
        "section_id", "heading", "intent", "claim_ids", "disclosed_uncertainty",
    ];
    // Option<String> fields: a wrongly-typed value (map, array) is dropped
    // rather than failing the parse — the claim's required `text` carries the
    // substance (2026-09-02 EN loop: third shape drift, a map in a string
    // field).
    const CLAIM_OPTIONAL_STRINGS: [&str; 6] = [
        "subject", "predicate", "unit", "period", "comparison_basis", "strength_label",
    ];
    let Some(root) = output.as_object_mut() else { return };
    let mut seen_section_ids: std::collections::BTreeSet<String> =
        std::collections::BTreeSet::new();
    // Root scalars: composers occasionally echo the contract name into
    // `schema_version` ("answer_ir/v1") or omit `locale`; both normalize
    // deterministically (2026-09-02 EN loop, sixth drift).
    let version_ok = root
        .get("schema_version")
        .is_some_and(Value::is_u64);
    if !version_ok {
        root.insert("schema_version".to_owned(), Value::from(1_u64));
    }
    // Locale is run metadata, not model output: bind it to the request so a
    // composer echoing the wrong example never trips the locale gate
    // (2026-09-02 EN loop: answer_locale_mismatch on an otherwise valid IR).
    let _ = root.remove("locale");
    for (field, allowed) in [("claims", &CLAIM_KEYS[..]), ("sections", &SECTION_KEYS[..])] {
        let Some(rows) = root.get_mut(field).and_then(Value::as_array_mut) else {
            continue;
        };
        rows.retain(|row| {
            let Some(object) = row.as_object() else { return true };
            if field != "claims" {
                return true;
            }
            // A claim without any prose field carries no substance: drop it
            // rather than failing the whole answer (the alias pass below
            // first rescues the common renames).
            object.contains_key("text")
                || object.contains_key("claim")
                || object.contains_key("statement")
                || object.contains_key("content")
        });
        for (index, row) in rows.iter_mut().enumerate() {
            let Some(object) = row.as_object_mut() else { continue };
            // Sections require `intent`; composers rarely emit it. A
            // missing or wrongly-typed intent defaults to a bounded label
            // (2026-09-02 EN loop, seventh drift). `heading` likewise.
            if field == "sections" {
                let heading_ok = object
                    .get("heading")
                    .is_some_and(Value::is_string);
                if !heading_ok
                    && let Some(heading) = object.remove("title")
                        .or_else(|| object.remove("name"))
                        .filter(Value::is_string)
                {
                    object.insert("heading".to_owned(), heading);
                }
                if !object
                    .get("heading")
                    .is_some_and(Value::is_string)
                {
                    object.insert(
                        "heading".to_owned(),
                        Value::String("Analysis".to_owned()),
                    );
                }
                let intent_ok = object
                    .get("intent")
                    .is_some_and(Value::is_string);
                if !intent_ok
                    && let Some(intent) = object.remove("purpose")
                        .or_else(|| object.remove("summary"))
                        .filter(Value::is_string)
                {
                    object.insert("intent".to_owned(), intent);
                }
                if !object
                    .get("intent")
                    .is_some_and(Value::is_string)
                {
                    object.insert(
                        "intent".to_owned(),
                        Value::String("orientation".to_owned()),
                    );
                }
                // section_id must satisfy the bounded identifier rule
                // (lowercase alphanumerics plus ._-:) AND stay unique across
                // sections. Sanitize whatever the composer produced; mint a
                // positional id when absent or colliding (2026-09-02 EN
                // loop: invalid_section_id x4 — duplicates after
                // sanitization).
                let mut sanitized = object
                    .get("section_id")
                    .and_then(Value::as_str)
                    .map(|id| {
                        id.chars()
                            .map(|c| {
                                if c.is_ascii_lowercase()
                                    || c.is_ascii_digit()
                                    || matches!(c, '.' | '_' | ':' | '-')
                                {
                                    c
                                } else if c.is_ascii_uppercase() {
                                    c.to_ascii_lowercase()
                                } else {
                                    '-'
                                }
                            })
                            .collect::<String>()
                    })
                    .filter(|id| !id.is_empty() && id.len() <= 128)
                    .unwrap_or_default();
                if sanitized.is_empty() {
                    sanitized = format!("section-{index}");
                }
                if !seen_section_ids.insert(sanitized.clone()) {
                    sanitized = format!("section-{index}");
                    seen_section_ids.insert(sanitized.clone());
                }
                object.insert("section_id".to_owned(), Value::String(sanitized));
                // claim_ids must be an array of strings; a missing or
                // wrongly-typed value defaults to empty (the section simply
                // carries no claim bindings — 2026-09-02 EN loop, ninth
                // drift).
                let ids_ok = object
                    .get("claim_ids")
                    .is_some_and(|ids| ids.as_array().is_some_and(
                        |rows| rows.iter().all(Value::is_string),
                    ));
                if !ids_ok
                    && let Some(ids) = object.remove("claims")
                        .filter(|ids| ids.as_array().is_some_and(
                            |rows| rows.iter().all(Value::is_string),
                        ))
                {
                    object.insert("claim_ids".to_owned(), ids);
                } else if !object
                    .get("claim_ids")
                    .is_some_and(|ids| ids.as_array().is_some_and(
                        |rows| rows.iter().all(Value::is_string),
                    ))
                {
                    object.insert("claim_ids".to_owned(), Value::Array(Vec::new()));
                }
            }
            if field == "claims" && !object.contains_key("text") {
                for alias in ["claim", "statement", "content"] {
                    if let Some(value) = object.remove(alias)
                        && value.is_string()
                    {
                        object.insert("text".to_owned(), value);
                        break;
                    }
                }
            }
            // The single most common id rename (`id` for `claim_id` /
            // `section_id`) is aliased deterministically (2026-09-02 EN
            // loop: fourth shape drift, a missing `claim_id`).
            let id_field = if field == "claims" { "claim_id" } else { "section_id" };
            if !object.contains_key(id_field) {
                for alias in ["id", "claim", "cid", "claimId", "sectionId"] {
                    if let Some(id) = object.remove(alias).filter(Value::is_string) {
                        object.insert(id_field.to_owned(), id);
                        break;
                    }
                }
            }
            // Still missing after aliases: mint a deterministic id from the
            // row's position so a claim never dies on its identifier alone
            // (2026-09-02 EN loop, eighth drift: `claimId` camelCase).
            if !object
                .get(id_field)
                .is_some_and(Value::is_string)
            {
                let minted = format!(
                    "{}-{}",
                    if field == "claims" { "claim" } else { "section" },
                    object.len()
                );
                object.insert(id_field.to_owned(), Value::String(minted));
            }
            object.retain(|key, _| allowed.contains(&key.as_str()));
            // Non-substantive enum labels the sanitizer re-derives anyway:
            // default them when the composer omits the key entirely (the
            // provider cannot run constrained JSON-schema output, so shape
            // drift on labels must not burn the answer budget).
            if field == "claims" {
                // Valid variants only: kind ∈ fact|number|interpretation|
                // uncertainty, strength ∈ qualified|strong. An unknown or
                // missing label falls back to the safe pair (the sanitizer
                // re-derives grading from the ledger regardless).
                let kind_ok = object
                    .get("kind")
                    .and_then(Value::as_str)
                    .is_some_and(|kind| {
                        matches!(
                            kind,
                            "fact" | "number" | "interpretation" | "uncertainty"
                        )
                    });
                if !kind_ok {
                    object.insert("kind".to_owned(), Value::String("fact".into()));
                }
                let strength_ok = object
                    .get("strength")
                    .and_then(Value::as_str)
                    .is_some_and(|strength| matches!(strength, "qualified" | "strong"));
                if !strength_ok {
                    object.insert(
                        "strength".to_owned(),
                        Value::String("qualified".into()),
                    );
                }
            }
            if field == "claims" {
                for key in ["goal_ids", "evidence_ids", "counter_evidence_ids", "calculation_ids"] {
                    let ok = object.get(key).is_some_and(|ids| ids.as_array().is_some_and(
                        |rows| rows.iter().all(Value::is_string),
                    ));
                    if !ok {
                        object.insert(key.to_owned(), Value::Array(Vec::new()));
                    }
                }
            }
            for key in CLAIM_OPTIONAL_STRINGS {
                if let Some(value) = object.get(key)
                    && !value.is_string()
                    && !value.is_null()
                {
                    object.remove(key);
                }
            }
        }
    }
    if let Some(questions) = root
        .get_mut("follow_up_questions")
        .and_then(Value::as_array_mut)
    {
        questions.retain(Value::is_string);
        // The public-text gate requires each question to end with '?' and
        // carry no control characters or numbered prefixes; composers often
        // emit "1. …" lists (2026-09-02 EN loop: x3). Normalize in place.
        for question in questions.iter_mut() {
            let Some(text) = question.as_str().map(str::to_owned) else {
                continue;
            };
            let cleaned = text
                .trim()
                .trim_start_matches(|c: char| c.is_ascii_digit())
                .trim_start_matches(|c: char| c == '.' || c == ')' || c.is_whitespace())
                .trim()
                .replace(['\n', '\r'], " ");
            let mut cleaned = cleaned;
            if !cleaned.is_empty() && !cleaned.ends_with('?') {
                cleaned.push('?');
            }
            if !cleaned.is_empty() && cleaned.len() <= 300 {
                *question = Value::String(cleaned);
            }
        }
        questions.retain(|question| {
            question
                .as_str()
                .is_some_and(|text| !text.trim().is_empty() && text.len() <= 300)
        });
    }
    // Every claim must be referenced by a section (unrendered_claim fails
    // verification). When the composer omits section bindings, attach the
    // orphaned claims to the first section — deterministic, and the renderer
    // places them under that heading (2026-09-02 EN loop: x9).
    let orphan_ids: Vec<String> = {
        let sections = root
            .get("sections")
            .and_then(Value::as_array);
        let claims = root
            .get("claims")
            .and_then(Value::as_array);
        match (sections, claims) {
            (Some(sections), Some(claims)) => {
                let bound: std::collections::BTreeSet<String> = sections
                    .iter()
                    .filter_map(|section| section.get("claim_ids"))
                    .filter_map(Value::as_array)
                    .flat_map(|ids| ids.iter().filter_map(Value::as_str))
                    .map(str::to_owned)
                    .collect();
                claims
                    .iter()
                    .filter_map(|claim| claim.get("claim_id"))
                    .filter_map(Value::as_str)
                    .filter(|id| !bound.contains(*id))
                    .map(str::to_owned)
                    .collect()
            }
            _ => Vec::new(),
        }
    };
    if !orphan_ids.is_empty()
        && let Some(ids) = root
            .get_mut("sections")
            .and_then(Value::as_array_mut)
            .and_then(|sections| sections.first_mut())
            .and_then(|first| first.get_mut("claim_ids"))
            .and_then(Value::as_array_mut)
    {
        ids.extend(orphan_ids.into_iter().map(Value::String));
    }
    // Calculations carry their own closed key set; strip additive keys and
    // coerce array-of-string fields (2026-09-02 EN loop, tenth drift: an
    // `evidence_ids` key appeared on a calculation).
    const CALCULATION_KEYS: [&str; 11] = [
        "calculation_id", "expression", "label", "input_evidence_ids",
        "output", "unit", "rounding", "subject", "metric", "period", "currency",
    ];
    if let Some(rows) = root
        .get_mut("calculations")
        .and_then(Value::as_array_mut)
    {
        for (index, row) in rows.iter_mut().enumerate() {
            let Some(object) = row.as_object_mut() else { continue };
            object.retain(|key, _| CALCULATION_KEYS.contains(&key.as_str()));
            let ids_ok = object
                .get("input_evidence_ids")
                .is_some_and(|ids| ids.as_array().is_some_and(
                    |values| values.iter().all(Value::is_string),
                ));
            if !ids_ok {
                object.insert(
                    "input_evidence_ids".to_owned(),
                    Value::Array(Vec::new()),
                );
            }
            for key in ["label", "unit", "rounding", "subject", "metric", "period", "currency"] {
                if let Some(value) = object.get(key)
                    && !value.is_string()
                    && !value.is_null()
                {
                    object.remove(key);
                }
            }
        }
    }
}

fn validate_typed_output(
    input: &RunInput<'_>,
    state: &ActiveRun,
    contract: &ContractPin,
    output: &Value,
) -> Result<Option<(AnswerIr, ResearchCompletion)>, EngineError> {
    verify_pin(&contract.id, &contract.content_hash)
        .map_err(|error| EngineError::CanonicalRegistry(format!("{error:?}")))?;
    validate_fixed_guru_author_payload(selected_entrypoint(input.image, input.request)?, output)?;

    // answer-ir/v2 is structurally identical to v1 — only the typed caps
    // differ — so both share one typed parse/sanitize pipeline.
    let answer_ir = if contract.id == ANSWER_IR_V1 || contract.id == ANSWER_IR_V2 {
        // First live English run (2026-09-02): the composer prompt says
        // "Produce AnswerIR v1", and GLM wrapped the object in a single
        // `answer_ir` key. Unwrap exactly one such envelope before the typed
        // parse — a bounded, deterministic tolerance for a one-key wrapper,
        // never a deep or repeated unwrap.
        let mut typed_output = match output.as_object() {
            Some(object)
                if object.len() == 1 && object.contains_key("answer_ir") =>
            {
                object["answer_ir"].clone()
            }
            _ => output.clone(),
        };
        strip_unknown_typed_answer_fields(&mut typed_output);
        if let Some(object) = typed_output.as_object_mut() {
            object.insert(
                "locale".to_owned(),
                Value::String(input.request.locale.clone()),
            );
        }
        // Normalization may drop every claim (prose-less rows); committing
        // an empty shell as the final answer is worse than one bounded
        // repair: surface it through the answer-validation lane so the
        // composer retries (2026-09-02 EN live: a 468-char all-headings
        // final).
        let claims_empty = typed_output
            .get("claims")
            .and_then(Value::as_array)
            .is_none_or(Vec::is_empty);
        if claims_empty {
            return Err(EngineError::AnswerValidation(vec![
                "answer_no_surviving_claims".to_owned(),
            ]));
        }
        let mut answer_ir: AnswerIr = serde_json::from_value(typed_output)?;
        let policy = answer_policy(input.image);
        bind_kernel_goal_ids(&mut answer_ir, state);
        normalize_answer_calculation_lineage(&mut answer_ir, state);
        normalize_answer_section_headings(&mut answer_ir, &policy);
        validate_calculations(&answer_ir, &state.calculations)?;
        // Answer-always policy: presentation/quality defects degrade the
        // answer to `AcceptedWithWarnings`; only ledger-integrity classes
        // still fail here (and keep the repair/terminal taxonomy unchanged).
        let (answer_ir, completion) = sanitize_answer(&answer_ir, &state.ledger, &policy)
            .map_err(|issues| EngineError::AnswerValidation(issue_codes(&issues)))?;
        validate_kernel_goal_bindings(&answer_ir, state)?;
        Some((answer_ir, completion))
    } else if contract.id == REPORT_SECTIONS_V1 {
        // E1 sectioned compose: a batch is never the committed answer. The
        // batch grammar itself was already validated canonically; here the
        // accumulated-so-far assembly is checked with the sanitizer's own
        // integrity classifier so a batch that would corrupt the claim↔ledger
        // binding rides the ordinary bounded repair lanes before retention.
        validate_section_batch_ledger_binding(input, state, output)?;
        None
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

/// E1 sectioned compose: validate one report-sections/v1 batch against the
/// evidence doctrine before it is retained. The check assembles every batch
/// accumulated so far plus the candidate and runs the ordinary
/// `validate_answer`; only the sanitizer's integrity classes fail here —
/// quality defects (heading shape, follow-up counts, strength bindings) stay
/// with the sanitizer at final assembly, exactly as for a single-shot typed
/// answer.
fn validate_section_batch_ledger_binding(
    input: &RunInput<'_>,
    state: &ActiveRun,
    batch: &Value,
) -> Result<(), EngineError> {
    let policy = answer_policy(input.image);
    let mut batches = state.composed_sections.clone();
    batches.push(batch.clone());
    let assembled = ActiveRun::assemble_answer_ir_from(&batches, &policy)?;
    let issues = match validate_answer(&assembled, &state.ledger, &policy) {
        Ok(()) => return Ok(()),
        Err(issues) => issues,
    };
    if issues
        .iter()
        .any(|issue| issue_is_integrity(issue, &assembled, &state.ledger))
    {
        return Err(EngineError::AnswerValidation(issue_codes(&issues)));
    }
    Ok(())
}

/// E1 engine-owned section accumulation loop: retain one validated
/// report-sections/v1 batch, walk the `section_submitted` edge into the
/// verification builtin, and decide loop continuation.
///
/// Returns `Ok(Some(assembled))` when the loop ended — the writer said
/// `report_done`, or the engine's own reserve-floor check
/// (`section_loop_may_continue`) stopped it, which is answer-always: the
/// already-retained sections are assembled and flow through the ordinary
/// verify→render path. Returns `Ok(None)` when exactly one more section turn
/// was admitted (batch retained, continuation ack appended); the orchestrator
/// checkpoints and dispatches the next compose turn.
#[allow(clippy::too_many_arguments)]
pub(crate) fn retain_section_batch(
    max_conversation_bytes: usize,
    input: &RunInput<'_>,
    state: &mut ActiveRun,
    episode: &ProviderEpisodeV1,
    section_pin: &ContractPin,
    final_contract: &ContractPin,
    batch: &Value,
) -> Result<Option<(Value, AnswerIr, ResearchCompletion, String)>, EngineError> {
    // Validate BEFORE any state change, and retain only after every fallible
    // step of the turn succeeded: a post-push failure (transition admission,
    // the acknowledgement's conversation limit, final assembly) would strand
    // the retained batch and send the repair retry to a misleading
    // `duplicate_section_id` death instead of a clean re-emit.
    state.validate_composed_section(batch)?;
    let episode_hash = ContentHash::sha256(serde_jcs::to_vec(episode)?);
    // `section_submitted`: compose → verification builtin, with the batch
    // itself as the model artifact sealed under the section grammar.
    let submitted_event = state.program.unique_transition_event(
        state.interpreter.current_state(),
        batch,
        |candidate| {
            matches!(
                candidate.operation,
                StateOperation::Builtin {
                    handler: BuiltinHandler::ValidateArtifact | BuiltinHandler::VerifyOutput,
                    ..
                }
            )
        },
        "section submitted",
    )?;
    state.apply_model_artifact(&submitted_event, section_pin.clone(), batch, episode_hash)?;
    let report_done = batch.get("continuation").and_then(Value::as_str) == Some("report_done");
    if !report_done && state.section_loop_may_continue(input.image)? {
        let facts = serde_json::json!({
            "sections_retained": state.composed_sections_len_with(batch),
        });
        let handler = match state.interpreter.current_operation()? {
            StateOperation::Builtin { handler, .. }
                if matches!(
                    handler,
                    BuiltinHandler::ValidateArtifact | BuiltinHandler::VerifyOutput
                ) =>
            {
                handler
            }
            _ => {
                return Err(EngineError::WorkflowResolution {
                    outcome: "section verifier",
                })
            }
        };
        let more_event = state.program.unique_transition_event(
            state.interpreter.current_state(),
            &facts,
            |candidate| matches!(candidate.operation, StateOperation::ModelDecision { .. }),
            "more sections required",
        )?;
        state.apply_builtin_artifact(
            *handler,
            &more_event,
            ContractPin::canonical(STATE_FACTS_V1)?,
            &facts,
        )?;
        state.require_model_state()?;
        state.append_assistant(episode);
        state.append_section_batch_ack(state.composed_sections_len_with(batch));
        state.check_conversation_limit(max_conversation_bytes)?;
        state.retain_composed_section(batch)?;
        return Ok(None);
    }
    // Loop ended: assemble the final and hand it to the ordinary
    // verify→render path — the same bind/normalize/validate/sanitize chain a
    // single-shot typed answer takes, so the evidence doctrine and the
    // claim↔ledger binding invariants are unchanged. Assembly is read-only
    // over the retained batches plus this one; the push happens only after
    // the assembled final proved committable.
    let policy = answer_policy(input.image);
    let mut batches = state.composed_sections.clone();
    batches.push(batch.clone());
    let mut answer_ir = ActiveRun::assemble_answer_ir_from(&batches, &policy)?;
    bind_kernel_goal_ids(&mut answer_ir, state);
    normalize_answer_calculation_lineage(&mut answer_ir, state);
    normalize_answer_section_headings(&mut answer_ir, &policy);
    validate_calculations(&answer_ir, &state.calculations)?;
    let (answer_ir, completion) = sanitize_answer(&answer_ir, &state.ledger, &policy)
        .map_err(|issues| EngineError::AnswerValidation(issue_codes(&issues)))?;
    validate_kernel_goal_bindings(&answer_ir, state)?;
    let rendered_content = render_markdown(&answer_ir, &state.ledger)?;
    state.retain_composed_section(batch)?;
    let final_output = if final_contract.id == ANSWER_IR_V1 || final_contract.id == ANSWER_IR_V2 {
        serde_json::to_value(&answer_ir)?
    } else {
        Value::String(rendered_content.clone())
    };
    Ok(Some((final_output, answer_ir, completion, rendered_content)))
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
    if contract.id == ANSWER_IR_V1 || contract.id == ANSWER_IR_V2 {
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
                && !state.current_operation_emits_answer(input.image)?
                && !state
                    .decision_retry_requested
                    .load(std::sync::atomic::Ordering::SeqCst)
            {
                // One bounded thinking-off retry for a decision turn that
                // truncated mid-reasoning (see ActiveRun::decision_retry_requested).
                state
                    .decision_retry_requested
                    .store(true, std::sync::atomic::Ordering::SeqCst);
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
                if state.section_output_contract()?.is_some() {
                    // A sectioned compose turn must retry in the SAME batch
                    // grammar (the retry disables private thinking so the
                    // batch fits the cap); the Markdown-language retry
                    // feedback would ask for the wrong output shape.
                    state.append_repair_feedback(REPORT_SECTIONS_V1, "answer_output_truncated");
                } else {
                    state.append_direct_answer_retry_feedback("answer_output_truncated");
                }
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
                if state.section_output_contract()?.is_some() {
                    state.append_repair_feedback(REPORT_SECTIONS_V1, "final_output_missing");
                } else {
                    state.append_direct_answer_retry_feedback("final_output_missing");
                }
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
        // The fifth tuple element is the raw report-sections/v1 batch when a
        // sectioned compose state produced this turn (its `section_submitted`
        // model artifact carries the batch, not the assembled final); `None`
        // for every ordinary output whose model artifact is the final itself.
        let (output, answer_ir, completion, rendered_content, section_batch) = match final_output_mode {
            ModelOutputMode::Markdown => {
                let output = Value::String(content.to_owned());
                validate_canonical_value(&output_contract.id, &output)
                    .map_err(|error| EngineError::CanonicalRegistry(format!("{error:?}")))?;
                // Direct Markdown carries no typed AnswerIR to sanitize, so
                // two deterministic content gates stand in for it: internal
                // vocabulary must not surface in user-facing prose, and the
                // answer may not open by framing itself as an unavailability.
                // One bounded retry; a second identically framed draft is
                // accepted rather than looping the run over prose judgment.
                if !state.direct_answer_retry_requested() {
                    if let Some(feedback) = markdown_content_gate_feedback(
                        content,
                        &input.image.body.answer_policy.forbidden_user_terms,
                    ) {
                        state.request_direct_answer_retry();
                        state.append_answer_content_gate_feedback(&feedback);
                        state.check_conversation_limit(self.config.max_conversation_bytes)?;
                        return Ok(None);
                    }
                }
                // Direct Markdown has no typed AnswerIR to sanitize; the
                // canonical contract is its own validation.
                (
                    output,
                    None,
                    ResearchCompletion::Accepted,
                    content.to_owned(),
                    None,
                )
            }
            ModelOutputMode::TypedJson => {
                let output: Value = match parse_typed_json_content(content) {
                    Ok(output) => output,
                    Err(error)
                        if constraint_mode != ProviderConstraintMode::JsonSchema
                            && state.reserve_repair()? =>
                    {
                        let code = answer_error_code(&EngineError::Json(error));
                        tracing::warn!(%code, "typed answer repair: content parse");
                        state.append_assistant(episode);
                        state.append_repair_feedback(&output_contract.id, code);
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
                // E1: a sectioned compose state declares report-sections/v1
                // as its own output grammar; the image-level internal format
                // stays the committed final-output contract. Canonical
                // validation always runs against the grammar the state
                // actually declares.
                let section_contract = state.section_output_contract()?;
                let validation_contract = section_contract
                    .clone()
                    .unwrap_or_else(|| output_contract.clone());
                if let Err(error) = validate_canonical_value(&validation_contract.id, &output) {
                    if constraint_mode == ProviderConstraintMode::JsonSchema {
                        return Err(EngineError::ProviderConstrainedOutputViolation(
                            "canonical schema",
                        ));
                    }
                    if state.reserve_repair()? {
                        let code = answer_error_code(&EngineError::CanonicalRegistry(format!(
                            "{error:?}"
                        )));
                        tracing::warn!(%code, "typed answer repair: canonical validation");
                        state.append_assistant(episode);
                        state.append_repair_feedback(&validation_contract.id, code);
                        state.check_conversation_limit(self.config.max_conversation_bytes)?;
                        return Ok(None);
                    }
                    return Err(EngineError::CanonicalRegistry(format!("{error:?}")));
                }
                if let Err(error) =
                    validate_product_output_linkage(input.request, &validation_contract, &output)
                {
                    if state.reserve_repair()? {
                        state.append_assistant(episode);
                        state.append_repair_feedback(
                            &validation_contract.id,
                            answer_error_code(&error),
                        );
                        state.check_conversation_limit(self.config.max_conversation_bytes)?;
                        return Ok(None);
                    }
                    return Err(error);
                }

                let candidate =
                    validate_typed_output(input, state, &validation_contract, &output);
                if let Some(section_pin) = section_contract {
                    // E1 engine-owned section accumulation loop: retain the
                    // validated batch, walk `section_submitted`, and either
                    // admit exactly one more section turn or assemble the
                    // final and continue through the ordinary verify→render
                    // path. Batch retention and assembly failures ride the
                    // same bounded repair lanes as any typed answer.
                    let assembled = candidate.and_then(|_| {
                        retain_section_batch(
                            self.config.max_conversation_bytes,
                            input,
                            state,
                            episode,
                            &section_pin,
                            &output_contract,
                            &output,
                        )
                    });
                    match assembled {
                        Ok(Some((final_output, answer_ir, completion, rendered_content))) => {
                            // The compose→verify artifact carries the batch
                            // itself; the assembled final below is the
                            // committed output.
                            (
                                final_output,
                                Some(answer_ir),
                                completion,
                                rendered_content,
                                Some(output),
                            )
                        }
                        Ok(None) => return Ok(None),
                        Err(error) if state.apply_answer_repair(answer_error_code(&error))? => {
                            let code = answer_error_code(&error);
                            tracing::warn!(%code, "section batch repair: answer validation");
                            state.append_assistant(episode);
                            state.append_repair_feedback(&section_pin.id, code);
                            state.check_conversation_limit(self.config.max_conversation_bytes)?;
                            return Ok(None);
                        }
                        Err(error) if state.reserve_repair()? => {
                            state.append_assistant(episode);
                            state
                                .append_repair_feedback(&section_pin.id, answer_error_code(&error));
                            state.check_conversation_limit(self.config.max_conversation_bytes)?;
                            return Ok(None);
                        }
                        Err(error) => return Err(error),
                    }
                } else {
                    let (answer_ir, completion) = match candidate {
                        Ok(Some((answer_ir, completion))) => (Some(answer_ir), completion),
                        // Product outputs (routing, notebook, display planning)
                        // intentionally produce no AnswerIR and no research
                        // completion class beyond the default.
                        Ok(None) => (None, ResearchCompletion::Accepted),
                        Err(error) if state.apply_answer_repair(answer_error_code(&error))? => {
                            let code = answer_error_code(&error);
                            tracing::warn!(%code, "typed answer repair: answer validation");
                            state.append_assistant(episode);
                            state.append_repair_feedback(&output_contract.id, code);
                            state.check_conversation_limit(self.config.max_conversation_bytes)?;
                            return Ok(None);
                        }
                        // A contract-shaped mistake inside the compose state has
                        // no verify-state repair transition; give it the same
                        // bounded conversational retry used for canonical
                        // violations instead of failing the run terminally.
                        Err(error) if state.reserve_repair()? => {
                            state.append_assistant(episode);
                            state.append_repair_feedback(
                                &output_contract.id,
                                answer_error_code(&error),
                            );
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
                    (normalized_output, answer_ir, completion, rendered_content, None)
                }
            }
            _ => {
                return Err(EngineError::WorkflowResolution {
                    outcome: "final output mode",
                });
            }
        };

        // The sectioned compose flow already walked compose→verify inside
        // `retain_section_batch` (its `section_submitted` model artifact
        // carries the retained batch); every ordinary output applies its own
        // compose→verify model artifact with the final payload here. Both
        // then share the same verification→render→commit chain below.
        if section_batch.is_none() {
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
        }

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
            schema_version: 5,
            output_contract: output_contract.clone(),
            output: output.clone(),
            evidence_ledger_hash,
            evidence_ids,
            answer_ir,
            sections: state.composed_sections.clone(),
            rendered_content: rendered_content.clone(),
            rendered_markdown: rendered_content,
            visualizations,
            completion,
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
        Ok(Some(run_outcome_after_commit(
            state,
            execution,
            &output,
            output_contract,
            answer_bundle,
            answer_bundle_hash,
            final_status,
        )))
    }

    /// Deterministic ledger fallback (policy `UnavailableButAnswerable`).
    ///
    /// Called only when the run loop terminated with a dependency/budget-class
    /// escape after admission (see [`error_allows_ledger_fallback`]). It never
    /// issues another provider turn: the answer is rendered from the
    /// already-admitted ledger and committed through the same durable-final
    /// machinery as an ordinary answer. Cancellation, fencing, and ownership
    /// checks still gate the commit, and a failed or ambiguous fallback commit
    /// keeps the run failed rather than papering over a storage fault.
    pub(crate) async fn commit_ledger_fallback(
        &self,
        input: &RunInput<'_>,
        identity: &RunIdentity,
        state: &mut ActiveRun,
        error: EngineError,
        deadline: Instant,
    ) -> Result<RunOutcome, EngineError> {
        // The budget deadline is often the very reason the loop terminated, so
        // the terminal commit gets a small grace window — but never beyond the
        // lease deadline that still bounds this execution.
        let grace_cap = Instant::now()
            .checked_add(LEDGER_FALLBACK_COMMIT_GRACE)
            .ok_or(EngineError::DeadlineExceeded("ledger_fallback"))?;
        let commit_deadline = input.hard_deadline.min(deadline.max(grace_cap));
        self.guard_control(identity, commit_deadline).await?;

        state.merge_runtime_timings();
        let tickers = memory_tickers(
            &input.request.context,
            state.derived_ticker_scope.as_ref(),
            &state.ledger,
        );
        let fallback = fallback_answer_from_ledger(
            &input.request.question,
            &tickers,
            &state.ledger,
            &state.calculations,
            state.research_planner.intent_projection(),
            state.accepted_actions.len(),
            error_is_output_budget_exhaustion(&error),
        );
        tracing::warn!(
            code = fallback.reason_code,
            trigger = ?error,
            "terminal dependency/budget escape; committing deterministic ledger fallback final"
        );
        let output = Value::String(fallback.markdown.clone());
        let output_contract = ContractPin::canonical(FINAL_MARKDOWN_V1)?;
        validate_canonical_value(FINAL_MARKDOWN_V1, &output)
            .map_err(|error| EngineError::CanonicalRegistry(format!("{error:?}")))?;
        let evidence_ledger_hash = ContentHash::sha256(serde_jcs::to_vec(&state.ledger)?);
        let answer_bundle = AnswerBundle {
            schema_version: 5,
            output_contract: output_contract.clone(),
            output: output.clone(),
            evidence_ledger_hash,
            evidence_ids: fallback.cited_evidence_ids,
            answer_ir: None,
            // The ledger fallback is answer-always: a sectioned run that
            // escaped terminally still carries every batch it had committed.
            sections: state.composed_sections.clone(),
            rendered_content: fallback.markdown.clone(),
            rendered_markdown: fallback.markdown,
            visualizations: Vec::new(),
            completion: ResearchCompletion::UnavailableButAnswerable,
            usage: state.usage.clone(),
            agent_image_hash: input.image.content_hash.clone(),
        };
        let bundle_value = serde_json::to_value(&answer_bundle)?;
        let bundle_bytes = serde_jcs::to_vec(&bundle_value)?;
        let answer_bundle_hash = ContentHash::sha256(&bundle_bytes);
        let final_output_hash = ContentHash::sha256(serde_jcs::to_vec(&answer_bundle.output)?);
        let rendered_message_hash = ContentHash::sha256(&answer_bundle.rendered_markdown);
        let commit_envelope_hash = ContentHash::sha256(serde_jcs::to_vec(&serde_json::json!({
            "schema_version": 2,
            "answer_bundle_hash": answer_bundle_hash,
            "session_memory_delta_hash": Option::<ContentHash>::None,
            "next_memory_frontier_hash": Option::<ContentHash>::None,
        }))?);
        let durable_final = DurableFinal {
            mutation: FinalCommitMutation {
                run_id: identity.run_id.clone(),
                fencing_token: identity.fencing_token,
                expected_cancel_generation: identity.expected_cancel_generation,
                mutation_id: mutation_id("commit_final", &identity.run_id, &commit_envelope_hash),
                answer_bundle_hash: answer_bundle_hash.clone(),
                session_memory_delta_hash: None,
            },
            final_output_hash,
            rendered_message_hash,
            answer_bundle: bundle_value,
            usage: state.usage.clone(),
            session_memory_delta: None,
            next_memory_frontier_hash: None,
        };
        let final_status = match await_until(
            commit_deadline,
            self.persistence.commit_final(&durable_final),
        )
        .await
        {
            Ok(Ok(status)) => status,
            Ok(Err(failure)) if failure.delivery == DeliveryCertainty::MayHaveDispatched => {
                return Err(EngineError::FinalCommitAmbiguous(answer_bundle_hash));
            }
            // The fallback commit deterministically did not dispatch:
            // surface the original dependency cause, not the notice.
            Ok(Err(_)) => return Err(error),
            Err(()) => return Err(EngineError::FinalCommitAmbiguous(answer_bundle_hash)),
        };
        if final_status == FinalStatus::Cancelled {
            return Err(EngineError::Cancelled);
        }
        Ok(RunOutcome {
            answer_bundle,
            answer_bundle_hash,
            final_status,
            logical_action_keys: state.logical_action_keys.iter().cloned().collect(),
            evidence_count: state.ledger.len(),
        })
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

/// Wall-clock grace window for committing a ledger-fallback final after the
/// run budget deadline has already fired. Bounded by the lease deadline.
const LEDGER_FALLBACK_COMMIT_GRACE: Duration = Duration::from_secs(2);

/// Walk the statechart to its terminal edge after the durable final commit.
///
/// Every failure here happens strictly after the core final is durably
/// committed, so it is an auxiliary workflow-edge defect: the policy row
/// "DB commit 뒤 workflow edge 오류가 API final을 뒤집지 않음" requires the
/// caller to log-and-continue rather than fail the run.
fn finalize_post_commit_workflow(
    state: &mut ActiveRun,
    execution: ExecutionState,
    output: &Value,
    output_contract: ContractPin,
) -> Result<(), EngineError> {
    let terminal_event = state.program.unique_transition_event(
        state.interpreter.current_state(),
        output,
        |candidate| matches!(candidate.operation, StateOperation::Terminal { .. }),
        "atomic final committed",
    )?;
    state.apply_builtin_artifact(
        BuiltinHandler::CommitOutput,
        &terminal_event,
        output_contract,
        output,
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
    Ok(())
}

/// Build the run outcome for an already-durably-committed final. The core
/// final (identity, Markdown, hashes, evidence refs, completion class) is
/// committed; the post-commit workflow edge is auxiliary and infallible at
/// this boundary: its failures are logged and never flip the API final.
pub(crate) fn run_outcome_after_commit(
    state: &mut ActiveRun,
    execution: ExecutionState,
    output: &Value,
    output_contract: ContractPin,
    answer_bundle: AnswerBundle,
    answer_bundle_hash: ContentHash,
    final_status: FinalStatus,
) -> RunOutcome {
    if let Err(error) = finalize_post_commit_workflow(state, execution, output, output_contract) {
        tracing::warn!(
            error = ?error,
            "workflow edge failed after the durable final commit; committed answer stays final"
        );
    }
    RunOutcome {
        answer_bundle,
        answer_bundle_hash,
        final_status,
        logical_action_keys: state.logical_action_keys.iter().cloned().collect(),
        evidence_count: state.ledger.len(),
    }
}

#[cfg(test)]
mod typed_answer_tolerance_tests {
    use serde_json::json;

    use super::strip_unknown_typed_answer_fields;

    #[test]
    fn unknown_claim_and_section_keys_are_stripped_but_required_kept() {
        // 2026-09-02 EN live: composers added `interpretation` to claims and
        // once wrapped the whole IR in `answer_ir`. Only additive keys are
        // dropped; a missing required field still fails the strict parse.
        let mut output = json!({
            "schema_version": 1,
            "locale": "en-US",
            "sections": [{
                "section_id": "s1", "heading": "Read", "intent": "open",
                "claim_ids": ["c1"], "disclosed_uncertainty": null, "note": "x",
            }],
            "claims": [{
                "claim_id": "c1", "kind": "statement", "strength": "medium",
                "text": "Revenue grew.", "interpretation": "bullish",
                "evidence_ids": ["e1"],
            }],
            "calculations": [],
            "follow_up_questions": [],
        });
        let drift = json!({
            "id": "c2", "claim": "Margins improved.", "evidence_ids": ["e2"],
        });
        output["claims"].as_array_mut().unwrap().push(json!({
            "claimId": "c4", "claim": "Services margin expanded.",
            "evidence_ids": ["e3"],
        }));
        output["claims"].as_array_mut().unwrap().push(drift);
        output["schema_version"] = json!("answer_ir/v1");
        output["claims"][0]["unit"] = json!({"label": "USD"});
        output["sections"][0].as_object_mut().unwrap().remove("claim_ids");
        output["claims"][1]["evidence_ids"] = json!("e2");
        output["schema_version"] = json!("ir");
        strip_unknown_typed_answer_fields(&mut output);
        assert_eq!(
            output["schema_version"], 1,
            "a contract-name echo normalizes to version 1"
        );
        assert!(
            output["sections"][0]["intent"].is_string(),
            "missing intent defaults"
        );
        let claims = output["claims"].as_array().unwrap();
        assert!(
            claims.iter().any(|claim| claim["claim_id"] == "c2"),
            "an `id` key is aliased to claim_id"
        );
        assert!(
            claims.iter().all(|claim| claim["kind"] == "fact"),
            "missing labels default to a valid variant"
        );
        assert!(
            claims.iter().all(|claim| claim["strength"] == "qualified"),
            "missing labels default"
        );
        assert!(
            claims
                .iter()
                .any(|claim| claim["text"] == "Margins improved."),
            "a claim alias rescues prose"
        );
        assert!(
            claims.iter().all(|claim| claim.get("text").is_some()),
            "prose-less claims are dropped, not fatal"
        );
        assert!(
            claims
                .iter()
                .all(|claim| claim.get("claim_id").map(serde_json::Value::is_string).unwrap_or(false)),
            "camelCase ids alias or mint — never fatal"
        );
        assert!(
            claims
                .iter()
                .all(|claim| claim["evidence_ids"].is_array()),
            "wrongly-typed array fields normalize to arrays"
        );
        assert!(
            output["sections"][0]["claim_ids"].is_array(),
            "missing section claim_ids default to an empty array"
        );
        output["follow_up_questions"] = json!([
            "1. What drives Services growth next quarter",
            "How does the 2s10s slope affect valuation?",
            ""
        ]);
        output["calculations"].as_array_mut().unwrap().push(json!({
            "calculation_id": "calc-x", "expression": "a+b",
            "input_evidence_ids": "e1", "evidence_ids": ["e1"], "output": 3,
        }));
        strip_unknown_typed_answer_fields(&mut output);
        let calcs = output["calculations"].as_array().unwrap();
        assert!(calcs[0].get("evidence_ids").is_none(), "calculation unknown keys strip");
        let questions = output["follow_up_questions"].as_array().unwrap();
        assert!(
            questions.iter().all(|question| question
                .as_str()
                .is_some_and(|text| text.ends_with('?'))),
            "numbered or unterminated follow-ups normalize and empty ones drop"
        );
        assert_eq!(questions.len(), 2);
        assert!(calcs[0]["input_evidence_ids"].is_array(), "calculation ids coerce to arrays");
        assert!(output["claims"][0].get("interpretation").is_none());
        assert!(
            output["claims"][0].get("unit").is_none(),
            "a wrongly-typed optional string is dropped, not fatal"
        );
        assert_eq!(output["claims"][0]["claim_id"], "c1");
        assert!(output["sections"][0].get("note").is_none());
        assert_eq!(output["sections"][0]["heading"], "Read");
    }
}

#[cfg(test)]
mod markdown_gate_tests {
    use super::markdown_content_gate_feedback;

    fn terms() -> Vec<String> {
        [
            "ResearchState",
            "object_id",
            "chain",
            "도구 호출",
            "검색 계획",
        ]
        .iter()
        .map(|term| (*term).to_owned())
        .collect()
    }

    #[test]
    fn unavailability_opening_without_direction_is_gated() {
        let gated = markdown_content_gate_feedback(
            "COIN의 신규 상품 매출 기여는 확인할 수 없습니다.\n\n관련 근거를 정리하면 아래와 같습니다.",
            &terms(),
        );
        assert!(gated.is_some_and(|feedback| feedback.contains("unavailability")));

        let english = markdown_content_gate_feedback(
            "The contribution of the new product cannot be determined from the filings.\n\nRelated figures follow.",
            &terms(),
        );
        assert!(english.is_some_and(|feedback| feedback.contains("unavailability")));
    }

    #[test]
    fn judgment_first_partial_evidence_opening_passes() {
        let passes = markdown_content_gate_feedback(
            "**신규 상품이 아직 매출을 의미 있게 끌고 있을 가능성은 낮아 보입니다(제 판단) — 직접 공시는 없지만, 전체 구독·서비스 매출이 전년 대비 14% 감소했고 '기타' 항목도 22% 줄었습니다.** 근거는 아래 표와 같습니다.",
            &terms(),
        );
        assert!(passes.is_none());
    }

    #[test]
    fn limitation_mid_answer_is_never_gated() {
        let passes = markdown_content_gate_feedback(
            "매출은 견고하게 성장했습니다. 다만 신제품 기여는 공개되지 않아 정확한 비중은 확인할 수 없습니다. 전체적으로 상승 방향입니다.",
            &terms(),
        );
        assert!(passes.is_none());
    }

    #[test]
    fn internal_terms_are_gated_with_word_boundaries() {
        let gated =
            markdown_content_gate_feedback("매출이 증가했습니다. (object_id: xbrl:COIN)", &terms());
        assert!(gated.is_some_and(|feedback| feedback.contains("object_id")));

        // "chain" is a forbidden internal tool name, but an investor answer
        // about supply chains must not be retried for it.
        let passes = markdown_content_gate_feedback(
            "공급망(supply chain) 부담 때문에 마진이 악화되었습니다.",
            &terms(),
        );
        assert!(passes.is_none());

        let korean_gated = markdown_content_gate_feedback(
            "이 결과는 도구 호출 예산 소진 후 남은 자료로 판단했습니다.",
            &terms(),
        );
        assert!(korean_gated.is_some());
    }

    #[test]
    fn short_tool_names_gate_only_on_tool_references() {
        // "chain" in the forbidden list names the ontology.chain TOOL. Plain
        // English usage passes; tool references — including Korean particle
        // forms that explicitly discuss invoking it — gate.
        let supply_chain_passes = markdown_content_gate_feedback(
            "공급망(supply chain) 부담 때문에 마진이 악화되었습니다.",
            &terms(),
        );
        assert!(supply_chain_passes.is_none());

        let tool_call_gated = markdown_content_gate_feedback(
            "이 차이는 chain 호출 결과에서 확인할 수 있습니다.",
            &terms(),
        );
        assert!(tool_call_gated.is_some_and(|feedback| feedback.contains("chain")));

        let qualified_gated = markdown_content_gate_feedback(
            "매출 구조는 ontology.chain 조회로 추적했습니다.",
            &terms(),
        );
        assert!(qualified_gated.is_some());

        let tool_english_gated =
            markdown_content_gate_feedback("The chain tool traced the revenue movement.", &terms());
        assert!(tool_english_gated.is_some());
    }

    #[test]
    fn headings_are_skipped_and_decimals_do_not_end_the_opening() {
        let passes = markdown_content_gate_feedback(
            "# COIN 실적 분석\n\n매출 6.56B 달러는 전년 대비 9.4% 증가했습니다. 신규 기여는 공개되지 않았습니다.",
            &terms(),
        );
        assert!(passes.is_none());
    }
}
