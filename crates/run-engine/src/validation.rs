//! integrity validation: contract guards, run-scope and ticker binding
//! checks, admission rules, durable receipt/episode/calculation validation,
//! canonical mutation ids, and bounded-input guards — presentation/quality
//! concerns deliberately do not live here

use super::*;

#[derive(Debug, Default)]
pub struct StructuralContractGuard;

impl ContractGuard for StructuralContractGuard {
    fn is_canonical(&self) -> bool {
        false
    }

    fn validate_arguments(
        &self,
        _capability: &CapabilitySpec,
        _binding: &CapabilityBinding,
        arguments: &Value,
    ) -> Result<(), DependencyFailure> {
        if arguments.is_object() {
            Ok(())
        } else {
            Err(DependencyFailure::redacted(
                "arguments_not_object",
                "capability arguments must be an object",
                false,
                DeliveryCertainty::NotDispatched,
            ))
        }
    }

    fn validate_result(
        &self,
        _capability: &CapabilitySpec,
        _binding: &CapabilityBinding,
        result: &CapabilityResult,
    ) -> Result<(), DependencyFailure> {
        if result.provider_content.is_null() {
            Err(DependencyFailure::redacted(
                "null_capability_result",
                "capability provider content was null",
                false,
                DeliveryCertainty::NotDispatched,
            ))
        } else {
            Ok(())
        }
    }
}

#[derive(Debug)]
pub struct CanonicalContractGuard {
    pins: BTreeMap<String, ContentHash>,
}

impl CanonicalContractGuard {
    pub fn new(image: &AgentImageManifest) -> Result<Self, CanonicalGuardError> {
        verify_registry()?;
        let mut pins = BTreeMap::new();
        for contract in &image.body.contracts {
            verify_pin(&contract.id, &contract.content_hash)?;
            if pins
                .insert(contract.id.clone(), contract.content_hash.clone())
                .is_some()
            {
                return Err(CanonicalGuardError::DuplicateContract(contract.id.clone()));
            }
        }
        for capability in &image.body.capabilities {
            let resolved = image.resolve_capability_contracts(capability)?;
            verify_pin(&resolved.input.id, &resolved.input.content_hash)?;
            let model_input = image.resolve_capability_model_input_contract(capability)?;
            verify_pin(&model_input.id, &model_input.content_hash)?;
            for output in &resolved.outputs {
                verify_pin(&output.id, &output.content_hash)?;
            }
            if !resolved
                .outputs
                .iter()
                .any(|contract| contract.id == NORMALIZED_CAPABILITY_RESULT_V1)
            {
                return Err(CanonicalGuardError::MissingNormalizedOutput(
                    capability.id.clone(),
                ));
            }
        }
        Ok(Self { pins })
    }

    fn verify_capability_contracts(
        &self,
        capability: &CapabilitySpec,
    ) -> Result<(), DependencyFailure> {
        let ids = std::iter::once(&capability.input_contract)
            .chain(capability.model_input_contract.iter())
            .chain(capability.output_contracts.iter());
        for id in ids {
            let Some(hash) = self.pins.get(id) else {
                return Err(contract_failure("contract_pin_missing", id));
            };
            if verify_pin(id, hash).is_err() {
                return Err(contract_failure("contract_pin_mismatch", id));
            }
        }
        Ok(())
    }
}

impl ContractGuard for CanonicalContractGuard {
    fn is_canonical(&self) -> bool {
        true
    }

    fn validate_arguments(
        &self,
        capability: &CapabilitySpec,
        _binding: &CapabilityBinding,
        arguments: &Value,
    ) -> Result<(), DependencyFailure> {
        self.verify_capability_contracts(capability)?;
        validate_canonical_value(&capability.input_contract, arguments)
            .map_err(|error| canonical_value_failure("canonical_input", &error))
    }

    fn validate_result(
        &self,
        capability: &CapabilitySpec,
        _binding: &CapabilityBinding,
        result: &CapabilityResult,
    ) -> Result<(), DependencyFailure> {
        self.verify_capability_contracts(capability)?;
        if result.provider_content.is_null()
            || result.evidence.len() > 256
            || result.calculations.len() > 64
        {
            return Err(contract_failure(
                "normalized_result_bounds",
                "normalized result is null or exceeds fixed item bounds",
            ));
        }
        let mut normalized_result = result.clone();
        normalized_result.presentation = None;
        let normalized = serde_json::to_value(&normalized_result).map_err(|error| {
            contract_failure("normalized_result_serialization", format!("{error:?}"))
        })?;
        validate_canonical_value(NORMALIZED_CAPABILITY_RESULT_V1, &normalized)
            .map_err(|error| canonical_value_failure("normalized_result", &error))?;

        if matches!(
            capability.research_action.as_ref(),
            Some(ResearchActionPolicy {
                kind: ImageResearchActionKind::Context,
                ..
            })
        ) {
            let raw_contract = if result.provider_content.get("violations").is_some()
                || result
                    .provider_content
                    .get("status")
                    .and_then(Value::as_str)
                    == Some("input_correction_required")
            {
                QUERY_CONTEXT_INPUT_CORRECTION_V1
            } else {
                RESEARCH_STATE_V2
            };
            validate_canonical_value(raw_contract, &result.provider_content)
                .map_err(|error| canonical_value_failure("canonical_remote_output", &error))?;
            if raw_contract == RESEARCH_STATE_V2 {
                let bytes = serde_json::to_vec(&result.provider_content).map_err(|error| {
                    contract_failure("research_state_serialization", format!("{error:?}"))
                })?;
                krw_ontology_adapter::parse_research_state(&bytes).map_err(|error| {
                    contract_failure("research_state_typed_invalid", format!("{error:?}"))
                })?;
            }
        }

        if let Some(raw_contract) = front_success_contract(capability)? {
            validate_canonical_value(raw_contract, &result.provider_content)
                .map_err(|error| canonical_value_failure("canonical_front_output", &error))?;
            validate_front_projection(raw_contract, capability, result)?;
        }

        let mut ledger =
            EvidenceLedger::from_records(result.evidence.clone()).map_err(|error| {
                contract_failure("normalized_evidence_invalid", format!("{error:?}"))
            })?;
        for calculation in &result.calculations {
            ledger
                .append_calculation(calculation.clone())
                .map_err(|error| {
                    contract_failure("normalized_calculation_invalid", format!("{error:?}"))
                })?;
        }
        Ok(())
    }
}

fn front_success_contract(capability: &CapabilitySpec) -> Result<Option<&str>, DependencyFailure> {
    let raw_contracts = capability
        .output_contracts
        .iter()
        .map(String::as_str)
        .filter(|contract| *contract != NORMALIZED_CAPABILITY_RESULT_V1)
        .collect::<Vec<_>>();
    let front_contracts = raw_contracts
        .iter()
        .copied()
        .filter(|contract| {
            matches!(
                *contract,
                KRW_FEED_LIST_ITEMS_RESULT_V1
                    | KRW_FEED_GET_ITEMS_RESULT_V1
                    | KRW_FEED_CONTEXT_V2
                    | KRW_FILING_SEARCH_RESULT_V1
                    | KRW_FILING_METADATA_V1
                    | KRW_FILING_BRIEF_RESULT_V1
                    | KRW_FILING_SECTIONS_RESULT_V1
                    | KRW_FILING_READ_SECTION_RESULT_V1
                    | KRW_FILING_DOCUMENTS_RESULT_V1
                    | KRW_FILING_READ_DOCUMENT_RESULT_V1
                    | KRW_FORM4_TRANSACTIONS_RESULT_V1
            )
        })
        .collect::<Vec<_>>();
    if front_contracts.is_empty() {
        return Ok(None);
    }
    if raw_contracts.len() != 1 || front_contracts.len() != 1 {
        return Err(contract_failure(
            "front_output_contract_set_invalid",
            "front capability must have exactly one canonical success output",
        ));
    }
    Ok(front_contracts.first().copied())
}

fn validate_front_projection(
    raw_contract: &str,
    capability: &CapabilitySpec,
    result: &CapabilityResult,
) -> Result<(), DependencyFailure> {
    // The company-research supplemental ladder reuses the front physical
    // contracts but projects evidence through the ontology adapter's
    // supplemental mappers: filing events are issuer-disclosed fact and map to
    // direct/strong records, feed issues stay related/unverified. The feed
    // deployment's non-evidence catalog policy keys on the same output
    // contracts, so the projection class must follow the image-declared
    // result ingest, not the contract id alone.
    match capability.result_ingest {
        CapabilityResultIngest::FilingEventSearchV1
        | CapabilityResultIngest::FilingEventBriefV1 => {
            // Same closed projection the verified filing reads enforce: only
            // direct strong qualitative records, and an empty-but-valid
            // catalog carries no evidence and stays qualified-only.
            let expected_answerability = if result.evidence.is_empty() {
                Answerability::QualifiedOnly
            } else {
                Answerability::StrongAllowed
            };
            if !result.calculations.is_empty()
                || result.answerability != Some(expected_answerability)
                || result.evidence.iter().any(|record| {
                    record.directness != Directness::Direct
                        || record.grade != EvidenceGrade::Strong
                        || !record.strong_claim_allowed
                })
            {
                return Err(contract_failure(
                    "front_filing_ladder_projection_invalid",
                    "ladder filing events must map only to direct strong qualitative evidence",
                ));
            }
        }
        CapabilityResultIngest::FeedIssueListV1 | CapabilityResultIngest::FeedIssueContextV1 => {
            validate_front_feed_projection(result)?;
        }
        _ => match raw_contract {
            KRW_FEED_LIST_ITEMS_RESULT_V1 | KRW_FEED_GET_ITEMS_RESULT_V1 | KRW_FEED_CONTEXT_V2 => {
                validate_front_feed_projection(result)?;
            }
            KRW_FILING_SEARCH_RESULT_V1
            | KRW_FILING_METADATA_V1
            | KRW_FILING_BRIEF_RESULT_V1
            | KRW_FILING_SECTIONS_RESULT_V1
            | KRW_FILING_DOCUMENTS_RESULT_V1 => {
                if !result.evidence.is_empty()
                    || !result.calculations.is_empty()
                    || result.answerability.is_some()
                {
                    return Err(contract_failure(
                        "front_filing_catalog_became_evidence",
                        "filing catalog, metadata, brief, and list outputs are non-evidence",
                    ));
                }
            }
            KRW_FILING_READ_SECTION_RESULT_V1
            | KRW_FILING_READ_DOCUMENT_RESULT_V1
            | KRW_FORM4_TRANSACTIONS_RESULT_V1 => {
                let expected_answerability = if result.evidence.is_empty() {
                    Answerability::QualifiedOnly
                } else {
                    Answerability::StrongAllowed
                };
                if !result.calculations.is_empty()
                    || result.answerability != Some(expected_answerability)
                    || result.evidence.iter().any(|record| {
                        record.directness != Directness::Direct
                            || record.grade != EvidenceGrade::Strong
                            || !record.strong_claim_allowed
                    })
                {
                    return Err(contract_failure(
                        "front_filing_direct_projection_invalid",
                        "verified filing content must map only to direct strong qualitative evidence",
                    ));
                }
            }
            _ => {
                return Err(contract_failure(
                    "unknown_front_projection",
                    "front success contract has no closed projection policy",
                ));
            }
        },
    }
    Ok(())
}

/// Feed output may only produce related/unverified qualified evidence.
fn validate_front_feed_projection(result: &CapabilityResult) -> Result<(), DependencyFailure> {
    if !result.calculations.is_empty()
        || result.answerability != Some(Answerability::QualifiedOnly)
        || result.evidence.iter().any(|record| {
            record.strong_claim_allowed
                || record.directness > Directness::Related
                || record.grade > EvidenceGrade::Medium
        })
    {
        return Err(contract_failure(
            "front_feed_projection_unsafe",
            "feed output may only produce related/unverified qualified evidence",
        ));
    }
    Ok(())
}

fn contract_failure(code: &str, diagnostic: impl AsRef<[u8]>) -> DependencyFailure {
    DependencyFailure::redacted(code, diagnostic, false, DeliveryCertainty::NotDispatched)
}

/// Expose a stable validation category while retaining the detailed contract
/// diagnostic only as a hash. Model arguments and capability payloads can
/// contain user or retrieved text and are never placed in ordinary logs.
fn canonical_value_failure(
    prefix: &str,
    error: &krw_agent_contracts::ContractValueError,
) -> DependencyFailure {
    let category = match error {
        krw_agent_contracts::ContractValueError::UnknownContract(id) => {
            tracing::warn!(contract_id = %id, prefix, "canonical_input_unknown_contract");
            "unknown_contract"
        }
        krw_agent_contracts::ContractValueError::Shape(_) => "shape_invalid",
        krw_agent_contracts::ContractValueError::Semantic(_) => "semantic_invalid",
        krw_agent_contracts::ContractValueError::Limit(_) => "limit_exceeded",
        krw_agent_contracts::ContractValueError::Json(_) => "serialization_invalid",
    };
    contract_failure(&format!("{prefix}_{category}"), format!("{error:?}"))
}

#[derive(Debug, Error)]
pub enum CanonicalGuardError {
    #[error(transparent)]
    Registry(#[from] krw_agent_contracts::ContractArtifactError),
    #[error(transparent)]
    Image(#[from] krw_agent_image::ImageError),
    #[error("duplicate contract in image: {0}")]
    DuplicateContract(String),
    #[error("capability lacks normalized output contract: {0}")]
    MissingNormalizedOutput(String),
}

pub(crate) fn validate_capability_run_scope(
    entrypoint: &EntrypointSpec,
    context: &RunContextV1,
    derived_ticker_scope: Option<&DerivedTickerScope>,
    capability: &CapabilitySpec,
    arguments: &Value,
) -> Result<(), EngineError> {
    match (&capability.scope_binding, context) {
        (
            CapabilityScopeBinding::TrustedTickerSet {
                ticker_references,
                require_any_of,
                reject_non_null_pointers,
            },
            RunContextV1::CompanyTickerSet { .. } | RunContextV1::ResearchNotebook { .. },
        ) => validate_trusted_ticker_binding(
            ticker_references,
            require_any_of,
            reject_non_null_pointers,
            arguments,
            context.trusted_tickers(),
        ),
        (
            CapabilityScopeBinding::TrustedTickerSet {
                ticker_references,
                require_any_of,
                reject_non_null_pointers,
            },
            RunContextV1::SelectedFeedItems { .. },
        ) => {
            let scope = derived_ticker_scope.ok_or(EngineError::DerivedFeedScopeUnavailable)?;
            validate_trusted_ticker_binding(
                ticker_references,
                require_any_of,
                reject_non_null_pointers,
                arguments,
                &scope.tickers,
            )
        }
        (
            CapabilityScopeBinding::CoveredUniverse {
                ticker_references,
                required_string_values,
                bounded_integer_pointer,
            },
            RunContextV1::CoveredUniverse { .. },
        ) => validate_covered_universe_binding(
            entrypoint,
            ticker_references,
            required_string_values,
            bounded_integer_pointer.as_deref(),
            arguments,
        ),
        (
            CapabilityScopeBinding::SelectedFeedItems {
                issue_ids_pointer,
                ticker_references,
                forbid_ticker_references,
            },
            RunContextV1::SelectedFeedItems { feed_item_ids },
        ) => validate_selected_feed_binding(
            feed_item_ids,
            issue_ids_pointer,
            ticker_references,
            *forbid_ticker_references,
            arguments,
        ),
        (
            CapabilityScopeBinding::SourceFiling {
                filing_event_id_pointer,
            },
            RunContextV1::SourceFiling { filing_event_id },
        ) => validate_source_filing_binding(arguments, filing_event_id_pointer, filing_event_id),
        // A follow-up read whose identifiers the capability-runtime observed
        // in a prior committed producer result of this run. Admitting the
        // dispatch into the trusted ticker context does not widen anything:
        // the producer ran under the same validated ticker scope, the
        // observed-id guard rejects unobserved identifiers fail-closed, and
        // the inherited scope is the producer's trusted ticker.
        (
            CapabilityScopeBinding::ObservedResultIds { .. },
            RunContextV1::CompanyTickerSet { .. } | RunContextV1::ResearchNotebook { .. },
        ) => Ok(()),
        (
            _,
            RunContextV1::QuestionOnly {}
            | RunContextV1::RoutingRequest { .. }
            | RunContextV1::ExistingAnswer { .. },
        ) => Err(EngineError::RunScopeViolation(
            "no-scope context cannot authorize capability dispatch",
        )),
        _ => Err(EngineError::RunScopeViolation(
            "capability scope binding does not authorize this run context",
        )),
    }
}

fn validate_trusted_ticker_binding(
    ticker_references: &[TickerReferenceSpec],
    require_any_of: &[String],
    reject_non_null_pointers: &[String],
    arguments: &Value,
    trusted_tickers: &[String],
) -> Result<(), EngineError> {
    if trusted_tickers.is_empty() {
        return Err(EngineError::RunScopeViolation(
            "ticker-bound capability has no trusted ticker scope",
        ));
    }
    let trusted = trusted_tickers
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let observed = validate_ticker_reference_values(arguments, ticker_references, &trusted)?;
    if !require_any_of
        .iter()
        .any(|reference_id| observed.get(reference_id).copied().unwrap_or_default() > 0)
    {
        return Err(EngineError::RunScopeViolation(
            "capability omitted its required trusted ticker reference",
        ));
    }
    for pointer in reject_non_null_pointers {
        if arguments
            .pointer(pointer)
            .is_some_and(|value| !value.is_null())
        {
            return Err(EngineError::RunScopeViolation(
                "capability attempted to widen a trusted ticker scope",
            ));
        }
    }
    Ok(())
}

fn validate_covered_universe_binding(
    entrypoint: &EntrypointSpec,
    ticker_references: &[TickerReferenceSpec],
    required_string_values: &[krw_agent_image::RequiredStringValue],
    bounded_integer_pointer: Option<&str>,
    arguments: &Value,
) -> Result<(), EngineError> {
    let empty = BTreeSet::new();
    validate_ticker_reference_values(arguments, ticker_references, &empty)?;
    for requirement in required_string_values {
        if arguments
            .pointer(&requirement.pointer)
            .and_then(Value::as_str)
            != Some(requirement.value.as_str())
        {
            return Err(EngineError::RunScopeViolation(
                "covered-universe capability omitted a required scope marker",
            ));
        }
    }
    if let Some(pointer) = bounded_integer_pointer {
        let limit = arguments.pointer(pointer).and_then(Value::as_u64).ok_or(
            EngineError::RunScopeViolation(
                "covered-universe capability omitted its bounded discovery limit",
            ),
        )?;
        if limit == 0 || limit > u64::from(entrypoint.scope.cardinality.value()) {
            return Err(EngineError::RunScopeViolation(
                "covered-universe capability exceeded the entrypoint discovery limit",
            ));
        }
    }
    Ok(())
}

fn validate_selected_feed_binding(
    trusted_ids: &[String],
    issue_ids_pointer: &str,
    ticker_references: &[TickerReferenceSpec],
    forbid_ticker_references: bool,
    arguments: &Value,
) -> Result<(), EngineError> {
    let supplied = arguments
        .pointer(issue_ids_pointer)
        .and_then(Value::as_array)
        .ok_or(EngineError::RunScopeViolation(
            "selected-feed read omitted issue_ids",
        ))?;
    if supplied.is_empty() {
        return Err(EngineError::RunScopeViolation(
            "selected-feed read omitted issue_ids",
        ));
    }
    let trusted = trusted_ids
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let mut seen = BTreeSet::new();
    for value in supplied {
        let value = value.as_str().ok_or(EngineError::RunScopeViolation(
            "selected-feed read used a non-string issue ID",
        ))?;
        if !trusted.contains(value) || !seen.insert(value) {
            return Err(EngineError::RunScopeViolation(
                "selected-feed read widened or duplicated its immutable issue IDs",
            ));
        }
    }
    if forbid_ticker_references {
        let empty = BTreeSet::new();
        validate_ticker_reference_values(arguments, ticker_references, &empty)?;
    }
    Ok(())
}

fn validate_source_filing_binding(
    arguments: &Value,
    filing_event_id_pointer: &str,
    expected_filing_event_id: &str,
) -> Result<(), EngineError> {
    let supplied = arguments
        .pointer(filing_event_id_pointer)
        .and_then(Value::as_str)
        .ok_or(EngineError::RunScopeViolation(
            "source-filing capability omitted its immutable filing event ID",
        ))?;
    if supplied != expected_filing_event_id {
        return Err(EngineError::RunScopeViolation(
            "source-filing capability substituted its immutable filing event ID",
        ));
    }
    Ok(())
}

const MAX_SCOPE_POINTER_MATCHES: usize = 4_096;

fn validate_ticker_reference_values(
    arguments: &Value,
    references: &[TickerReferenceSpec],
    trusted: &BTreeSet<&str>,
) -> Result<BTreeMap<String, usize>, EngineError> {
    let mut observed = BTreeMap::new();
    for reference in references {
        let values = resolve_scope_pointer_pattern(arguments, &reference.pointer_pattern)?;
        let mut count = 0usize;
        for value in values {
            match reference.value_kind {
                TickerReferenceValueKind::String => {
                    if value.is_null() {
                        continue;
                    }
                    let ticker = value.as_str().ok_or(EngineError::RunScopeViolation(
                        "ticker scope reference must resolve to a string",
                    ))?;
                    validate_scope_ticker(ticker, trusted)?;
                    count = count.saturating_add(1);
                }
                TickerReferenceValueKind::StringArray => {
                    if value.is_null() {
                        continue;
                    }
                    let values = value.as_array().ok_or(EngineError::RunScopeViolation(
                        "ticker scope reference must resolve to an array",
                    ))?;
                    let mut local_seen = BTreeSet::new();
                    for value in values {
                        let ticker = value.as_str().ok_or(EngineError::RunScopeViolation(
                            "ticker scope array contains a non-string value",
                        ))?;
                        validate_scope_ticker(ticker, trusted)?;
                        if !local_seen.insert(ticker) {
                            return Err(EngineError::RunScopeViolation(
                                "ticker scope array contains duplicates",
                            ));
                        }
                        count = count.saturating_add(1);
                    }
                }
            }
        }
        observed.insert(reference.id.clone(), count);
    }
    Ok(observed)
}

fn validate_scope_ticker(ticker: &str, trusted: &BTreeSet<&str>) -> Result<(), EngineError> {
    if !is_canonical_ticker(ticker) || !trusted.contains(ticker) {
        return Err(EngineError::RunScopeViolation(
            "capability ticker is outside the immutable run scope",
        ));
    }
    Ok(())
}

fn resolve_scope_pointer_pattern<'a>(
    root: &'a Value,
    pointer_pattern: &str,
) -> Result<Vec<&'a Value>, EngineError> {
    let mut frontier = vec![root];
    for raw_segment in pointer_pattern.split('/').skip(1) {
        let mut next = Vec::new();
        if raw_segment == "*" {
            for value in frontier {
                let values = value.as_array().ok_or(EngineError::RunScopeViolation(
                    "ticker scope wildcard did not resolve to an array",
                ))?;
                next.extend(values.iter());
            }
        } else {
            let segment = decode_scope_pointer_segment(raw_segment)?;
            for value in frontier {
                match value {
                    Value::Object(object) => {
                        if let Some(value) = object.get(&segment) {
                            next.push(value);
                        }
                    }
                    Value::Array(values) => {
                        let index = segment.parse::<usize>().map_err(|_| {
                            EngineError::RunScopeViolation(
                                "ticker scope pointer used a non-array index",
                            )
                        })?;
                        if let Some(value) = values.get(index) {
                            next.push(value);
                        }
                    }
                    _ => {
                        return Err(EngineError::RunScopeViolation(
                            "ticker scope pointer crossed a non-container value",
                        ));
                    }
                }
            }
        }
        if next.len() > MAX_SCOPE_POINTER_MATCHES {
            return Err(EngineError::RunScopeViolation(
                "ticker scope pointer exceeded its bounded expansion",
            ));
        }
        frontier = next;
    }
    Ok(frontier)
}

fn decode_scope_pointer_segment(segment: &str) -> Result<String, EngineError> {
    let mut decoded = String::with_capacity(segment.len());
    let mut characters = segment.chars();
    while let Some(character) = characters.next() {
        if character != '~' {
            decoded.push(character);
            continue;
        }
        match characters.next() {
            Some('0') => decoded.push('~'),
            Some('1') => decoded.push('/'),
            _ => {
                return Err(EngineError::RunScopeViolation(
                    "ticker scope pointer contains an invalid escape",
                ));
            }
        }
    }
    Ok(decoded)
}

pub(crate) fn validate_fixed_guru_author_payload(
    entrypoint: &EntrypointSpec,
    payload: &Value,
) -> Result<(), EngineError> {
    let Some(author) = entrypoint.constants.fixed_guru_author else {
        return Ok(());
    };
    validate_author_fields(payload, author.as_str())
}

fn validate_author_fields(value: &Value, expected: &str) -> Result<(), EngineError> {
    match value {
        Value::Array(values) => {
            for value in values {
                validate_author_fields(value, expected)?;
            }
        }
        Value::Object(values) => {
            for (key, value) in values {
                match key.as_str() {
                    "author_key" | "fixed_guru_author" => {
                        if value.as_str() != Some(expected) {
                            return Err(EngineError::GuruAuthorMismatch);
                        }
                    }
                    "author_keys" | "selected_author_keys" | "guru_keys" => {
                        let authors = value.as_array().ok_or(EngineError::GuruAuthorMismatch)?;
                        if authors.len() != 1 || authors[0].as_str() != Some(expected) {
                            return Err(EngineError::GuruAuthorMismatch);
                        }
                    }
                    _ => {}
                }
                validate_author_fields(value, expected)?;
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
    Ok(())
}

/// A model transition is an edge selection, not an untrusted state update.
/// Facts attached to its durable state artifact are generated from the pinned
/// execution contract. If a future workflow needs model-authored transition
/// data, it must add an explicit event payload contract rather than reopening
/// an arbitrary JSON object here.
pub(crate) fn kernel_workflow_facts(
    image: &AgentImageManifest,
    request: &RunRequest,
    event: &str,
) -> Result<Value, EngineError> {
    let entrypoint = selected_entrypoint(image, request)?;
    let mut pinned = serde_json::json!({"event": event});
    if let Some(author) = entrypoint.constants.fixed_guru_author {
        pinned
            .as_object_mut()
            .ok_or(EngineError::InvalidWorkflowControl)?
            .insert("author_key".into(), Value::String(author.as_str().into()));
    }
    Ok(pinned)
}

pub(crate) fn evaluate_admission_rules(
    image: &AgentImageManifest,
    request: &RunRequest,
) -> Result<(), EngineError> {
    if let Some(typed_input) = product_context_value(request) {
        return evaluate_rules(image, RulePhase::Admission, typed_input, None);
    }
    let entrypoint = selected_entrypoint(image, request)?;
    let mut input = serde_json::Map::new();
    input.insert("question".into(), Value::String(request.question.clone()));
    input.insert("run_kind".into(), Value::String(request.run_kind.clone()));
    input.insert("locale".into(), Value::String(request.locale.clone()));
    match &request.context {
        RunContextV1::CompanyTickerSet { tickers } => {
            input.insert("tickers".into(), serde_json::to_value(tickers)?);
            if let [ticker] = tickers.as_slice() {
                input.insert("ticker".into(), Value::String(ticker.clone()));
            }
        }
        RunContextV1::CoveredUniverse { universe } => {
            input.insert("universe".into(), serde_json::to_value(universe)?);
        }
        RunContextV1::SelectedFeedItems { feed_item_ids } => {
            input.insert("feed_item_ids".into(), serde_json::to_value(feed_item_ids)?);
        }
        RunContextV1::SourceFiling { filing_event_id } => {
            input.insert(
                "filing_event_id".into(),
                Value::String(filing_event_id.clone()),
            );
        }
        RunContextV1::ResearchNotebook { ticker, .. } => {
            input.insert("ticker".into(), Value::String(ticker.clone()));
        }
        RunContextV1::ExistingAnswer { .. }
        | RunContextV1::RoutingRequest { .. }
        | RunContextV1::QuestionOnly {} => {}
    }
    if let Some(author) = entrypoint.constants.fixed_guru_author {
        input.insert("author_key".into(), Value::String(author.as_str().into()));
    }
    evaluate_rules(image, RulePhase::Admission, &Value::Object(input), None)
}

pub(crate) fn trusted_scope_payload(entrypoint: &EntrypointSpec, context: &RunContextV1) -> Value {
    let scope = match context {
        RunContextV1::CompanyTickerSet { tickers } => {
            serde_json::json!({"kind":"company_ticker_set","tickers":tickers})
        }
        RunContextV1::CoveredUniverse { universe } => {
            serde_json::json!({"kind":"covered_universe","universe":universe})
        }
        RunContextV1::SelectedFeedItems { feed_item_ids } => {
            serde_json::json!({"kind":"selected_feed_items","feed_item_ids":feed_item_ids})
        }
        RunContextV1::SourceFiling { filing_event_id } => {
            serde_json::json!({"kind":"source_filing","filing_event_id":filing_event_id})
        }
        RunContextV1::ResearchNotebook {
            ticker, input_hash, ..
        } => serde_json::json!({
            "kind":"research_notebook",
            "ticker":ticker,
            "input_hash":input_hash,
        }),
        RunContextV1::ExistingAnswer { committed_source } => serde_json::json!({
            "kind":"existing_answer",
            "content_authority":"claim_pinned_committed_answer",
            "final_receipt_hash":committed_source.final_receipt_hash,
            "answer_bundle_hash":committed_source.answer_bundle_hash,
            "answer_ir_hash":committed_source.answer_ir_hash,
            "canonical_source_hash":committed_source.canonical_source_hash,
        }),
        RunContextV1::RoutingRequest { input_hash, .. } => {
            serde_json::json!({"kind":"routing_request","input_hash":input_hash})
        }
        RunContextV1::QuestionOnly {} => serde_json::json!({"kind":"question_only"}),
    };
    serde_json::json!({
        "entrypoint_constants": {
            "fixed_guru_author": entrypoint.constants.fixed_guru_author,
        },
        "scope": scope,
    })
}

pub(crate) fn untrusted_task_payload(request: &RunRequest) -> Value {
    let task_data = match &request.context {
        RunContextV1::ResearchNotebook { typed_input, .. } => Some(serde_json::json!({
            "contract":"notebook-transform-input/v1",
            "value":typed_input,
        })),
        RunContextV1::ExistingAnswer { committed_source } => Some(serde_json::json!({
            "contract":"canonical-display-source/v1",
            "value":committed_source.canonical_source,
        })),
        RunContextV1::RoutingRequest { typed_input, .. } => Some(serde_json::json!({
            "contract":"routing-request/v1",
            "value":typed_input,
        })),
        _ => None,
    };
    serde_json::json!({
        "locale": request.locale,
        "question": request.question,
        "run_kind": request.run_kind,
        "task_data": task_data,
    })
}

fn product_context_value(request: &RunRequest) -> Option<&Value> {
    match &request.context {
        RunContextV1::ResearchNotebook { typed_input, .. }
        | RunContextV1::RoutingRequest { typed_input, .. } => Some(typed_input),
        RunContextV1::ExistingAnswer { committed_source } => {
            Some(&committed_source.canonical_source)
        }
        _ => None,
    }
}

pub(crate) fn validate_product_context(
    request: &RunRequest,
    output_contract: &str,
) -> Result<(), EngineError> {
    match output_contract {
        ROUTING_DECISION_V2 => {
            let _ = routing_input(request)?;
        }
        NOTEBOOK_TRANSFORM_V2 => {
            let _ = notebook_input(request)?;
        }
        DISPLAY_PLAN_V2 => {
            let _ = committed_display_source(request)?;
        }
        _ if product_context_value(request).is_some() => {
            return Err(EngineError::ProductContextMismatch(
                "typed product context is paired with the wrong output contract",
            ));
        }
        _ => {}
    }
    Ok(())
}

pub(crate) fn action_rule_input(
    payload: &Value,
    state_trace: &[String],
    capability_calls: &BTreeMap<String, u16>,
) -> Result<Value, EngineError> {
    let mut input = match payload {
        Value::Object(input) => input.clone(),
        _ => serde_json::Map::from_iter([("output".into(), payload.clone())]),
    };
    // Canonical capability contracts may legitimately own fields named
    // `usage` or `state_trace` (the Guru query-context result does). Kernel
    // policy inputs remain unspoofable by overwriting those names only in this
    // ephemeral validator projection; the committed typed payload is never
    // mutated.
    input.insert("state_trace".into(), serde_json::to_value(state_trace)?);
    input.insert(
        "usage".into(),
        serde_json::json!({"capability_calls": capability_calls}),
    );
    Ok(Value::Object(input))
}

pub(crate) fn evaluate_rules(
    image: &AgentImageManifest,
    phase: RulePhase,
    input: &Value,
    result_ingest: Option<CapabilityResultIngest>,
) -> Result<(), EngineError> {
    let mut violations = Vec::new();
    for program in image.body.validators.iter().filter(|program| {
        program.phase == phase
            && (program.result_ingest_scope.is_empty()
                || result_ingest
                    .is_some_and(|ingest| program.result_ingest_scope.contains(&ingest)))
    }) {
        violations.extend(
            evaluate_rule_program(program, input)?
                .violations
                .into_iter()
                .map(|violation| format!("{}:{}", program.id, violation.code)),
        );
    }
    if violations.is_empty() {
        Ok(())
    } else {
        Err(EngineError::PhaseRuleViolations { phase, violations })
    }
}

pub(crate) fn validate_input(
    input: &RunInput<'_>,
    config: &EngineConfig,
) -> Result<(), EngineError> {
    validate_bounded_run_request(input.request)?;
    let entrypoint = selected_entrypoint(input.image, input.request)?;
    entrypoint
        .validate_run_context(&input.request.context)
        .map_err(|_| {
            EngineError::RunScopeViolation(
                "run context violates the selected AgentImage entrypoint policy",
            )
        })?;
    validate_product_context(
        input.request,
        &input.image.body.answer_policy.internal_format,
    )?;
    if input.request.run_id != input.snapshot.run_id {
        return Err(EngineError::InvalidInput("run_id mismatch"));
    }
    if input.snapshot.protocol_version != PROTOCOL_VERSION {
        return Err(EngineError::InvalidInput("protocol version mismatch"));
    }
    if input.image.content_hash != input.snapshot.agent_image_hash {
        return Err(EngineError::InvalidInput("agent image hash mismatch"));
    }
    if !ALLOWED_MODEL_IDS.contains(&input.request.requested_model.as_str())
        || !ALLOWED_MODEL_IDS.contains(&input.snapshot.requested_model.as_str())
        || !ALLOWED_MODEL_IDS.contains(&input.snapshot.resolved_model.as_str())
        || input.request.requested_model != input.snapshot.requested_model
        || input.snapshot.requested_model != input.snapshot.resolved_model
        || input.request.model_profile != input.snapshot.model_profile
        || entrypoint.required_model_profile != input.snapshot.model_profile
    {
        return Err(EngineError::InvalidInput("model identity mismatch"));
    }
    if input.snapshot.provider_api_version != "anthropic-messages-v1"
        || !matches!(
            (input.snapshot.thinking, input.snapshot.reasoning_effort),
            (ThinkingMode::Enabled, Some(_)) | (ThinkingMode::Disabled, None)
        )
    {
        return Err(EngineError::InvalidInput(
            "provider execution profile mismatch",
        ));
    }
    if input.request.budget != input.snapshot.budget {
        return Err(EngineError::InvalidInput("budget snapshot mismatch"));
    }
    if let Some(reserve) = input
        .image
        .effective_final_output_reserve_tokens(&entrypoint.workflow)?
    {
        let minimum_research_turn = input
            .image
            .body
            .answer_policy
            .minimum_research_turn_tokens
            .ok_or(EngineError::InvalidInput(
                "final output reserve has no minimum research turn",
            ))?;
        let threshold = reserve
            .checked_add(minimum_research_turn)
            .ok_or(EngineError::InvalidInput("final output reserve overflow"))?;
        if input.request.budget.max_output_tokens <= threshold {
            return Err(EngineError::InvalidInput(
                "final output reservation exceeds output budget",
            ));
        }
    }
    if input.resolved_deployment_binding_hash != &input.snapshot.deployment_binding_hash {
        return Err(EngineError::InvalidInput(
            "deployment binding hash mismatch",
        ));
    }
    build_tool_definitions(input.image, input.request)?;
    if input.request.budget.deadline_ms == 0 {
        return Err(EngineError::InvalidInput("deadline must be non-zero"));
    }
    if config.max_episode_bytes == 0
        || config.max_capability_result_bytes == 0
        || config.max_model_output_bytes == 0
        || config.max_conversation_bytes == 0
        || !(MIN_COMPACTED_CONTEXT_BYTES..=MAX_COMPACTED_CONTEXT_BYTES)
            .contains(&config.max_compacted_context_bytes)
        || config.max_compacted_context_bytes >= config.max_conversation_bytes
        || config.max_tool_calls_per_episode == 0
        || config.safety_write_timeout.is_zero()
    {
        return Err(EngineError::InvalidInput("engine bounds must be non-zero"));
    }
    if config.require_canonical_contracts && !config.contract_guard.is_canonical() {
        return Err(EngineError::CanonicalContractGuardRequired);
    }
    Ok(())
}

pub(crate) fn prepare_session_memory(
    request: &RunRequest,
) -> Result<Option<PreparedSessionMemory>, EngineError> {
    let Some(carrier) = request.session_memory.as_ref() else {
        return Ok(None);
    };
    carrier.validate_carrier()?;
    let canonical_bytes = serde_jcs::to_vec(&carrier.canonical_view)?;
    let view: SessionMemoryViewV3 = serde_json::from_slice(&canonical_bytes)
        .map_err(|_| EngineError::InvalidInput("session memory schema mismatch"))?;
    view.validate(MAX_SESSION_MEMORY_VIEW_BYTES)
        .map_err(|_| EngineError::InvalidInput("session memory validation failed"))?;
    if view.session_id_hash != ContentHash::sha256(&request.session_id)
        || view.source_frontier_hash != carrier.source_frontier_hash
        || view.source_revision != carrier.source_revision
        || ContentHash::sha256(&canonical_bytes) != carrier.view_hash
    {
        return Err(EngineError::InvalidInput(
            "session memory ownership or hash mismatch",
        ));
    }
    let semantic_view_hash = view.view_hash.clone();
    let canonical = String::from_utf8(canonical_bytes)
        .map_err(|_| EngineError::InvalidInput("session memory was not UTF-8"))?;
    Ok(Some(PreparedSessionMemory {
        canonical,
        payload_hash: carrier.view_hash.clone(),
        semantic_view_hash,
        source_frontier_hash: carrier.source_frontier_hash.clone(),
        source_revision: carrier.source_revision,
    }))
}

/// Session continuity must retain the actual company scope of a completed
/// research turn. Fixed company contexts already carry that scope; discovery
/// contexts do not, so use only the committed typed result rather than trying
/// to infer a ticker from the user's prose.
const MAX_MEMORY_TICKERS: usize = 32;

pub(crate) fn memory_tickers(
    context: &RunContextV1,
    derived_ticker_scope: Option<&DerivedTickerScope>,
    ledger: &EvidenceLedger,
) -> Vec<String> {
    match context {
        RunContextV1::CompanyTickerSet { tickers } => tickers.clone(),
        RunContextV1::ResearchNotebook { ticker, .. } => vec![ticker.clone()],
        RunContextV1::CoveredUniverse { .. }
        | RunContextV1::SelectedFeedItems { .. }
        | RunContextV1::SourceFiling { .. } => {
            discovered_memory_tickers(derived_ticker_scope, ledger)
        }
        RunContextV1::RoutingRequest { .. }
        | RunContextV1::ExistingAnswer { .. }
        | RunContextV1::QuestionOnly {} => Vec::new(),
    }
}

fn discovered_memory_tickers(
    derived_ticker_scope: Option<&DerivedTickerScope>,
    ledger: &EvidenceLedger,
) -> Vec<String> {
    let projected = derived_ticker_scope
        .map(|scope| canonical_memory_ticker_set(scope.tickers.iter().cloned()))
        .unwrap_or_default();
    if !projected.is_empty() {
        return projected;
    }

    // Orientation/snapshot-only records are useful navigation context but do
    // not establish that a company was researched. Persist entities only when
    // an actual read produced at least related evidence.
    canonical_memory_ticker_set(
        ledger
            .iter()
            .filter(|(_id, record)| record.directness != Directness::Unverified)
            .filter_map(|(_id, record)| record.entity.clone()),
    )
}

fn canonical_memory_ticker_set(tickers: impl IntoIterator<Item = String>) -> Vec<String> {
    tickers
        .into_iter()
        .filter(|ticker| is_canonical_ticker(ticker))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .take(MAX_MEMORY_TICKERS)
        .collect()
}

fn validate_bounded_run_request(request: &RunRequest) -> Result<(), EngineError> {
    const MAX_ID_BYTES: usize = 256;
    const MAX_KIND_BYTES: usize = 128;
    const MAX_LOCALE_BYTES: usize = 32;
    const MAX_QUESTION_BYTES: usize = 64 * 1024;

    let bounded_nonempty = |value: &str, limit: usize| {
        !value.is_empty() && value.len() <= limit && !value.contains('\0')
    };
    if !bounded_nonempty(&request.run_id, MAX_ID_BYTES)
        || !bounded_nonempty(&request.session_id, MAX_ID_BYTES)
        || !bounded_nonempty(&request.tenant_id, MAX_ID_BYTES)
        || !bounded_nonempty(&request.principal_id, MAX_ID_BYTES)
        || !bounded_nonempty(&request.run_kind, MAX_KIND_BYTES)
        || !bounded_nonempty(&request.locale, MAX_LOCALE_BYTES)
        || !bounded_nonempty(&request.question, MAX_QUESTION_BYTES)
        || !ALLOWED_MODEL_IDS.contains(&request.requested_model.as_str())
        || !bounded_nonempty(&request.model_profile, MAX_ID_BYTES)
    {
        return Err(EngineError::InvalidInput(
            "run request exceeds fixed bounds",
        ));
    }
    request.context.validate()?;
    if let Some(memory) = &request.session_memory {
        memory.validate_carrier()?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn validate_episode(
    episode: &ProviderEpisodeV1,
    expected_request_hash: &ContentHash,
    image: &AgentImageManifest,
    tool_schema_hash: &ContentHash,
    model: &str,
    api_version: &str,
    provider_wire_capabilities: ProviderWireCapabilities,
    request_thinking: ThinkingMode,
) -> Result<(), EngineError> {
    episode.verify_model_identity()?;
    episode.verify_tool_call_structure()?;
    let requires_thinking_tool_replay = request_thinking == ThinkingMode::Enabled;
    episode.verify_tool_call_requirements(
        requires_thinking_tool_replay
            && provider_wire_capabilities.requires_assistant_content_for_tool_calls,
        requires_thinking_tool_replay && provider_wire_capabilities.requires_thinking_block_replay,
    )?;
    if !ALLOWED_MODEL_IDS.contains(&model)
        || episode.schema_version != 1
        || episode.request_hash != *expected_request_hash
        || episode.agent_image_hash != image.content_hash
        || episode.tool_schema_hash != *tool_schema_hash
        || episode.requested_model != model
        || episode.api_version != api_version
        || !episode.tool_results.is_empty()
    {
        return Err(EngineError::InvalidProviderEpisode(
            "provider episode contract mismatch",
        ));
    }
    if episode.calculate_replay_hash()? != episode.replay_hash {
        return Err(EngineError::InvalidProviderEpisode(
            "provider replay hash mismatch",
        ));
    }
    Ok(())
}

pub(crate) fn validate_action_receipt(
    receipt: &ActionReceipt,
    call: &PreparedCall,
) -> Result<(), EngineError> {
    if receipt.action_key != call.action_key
        || receipt.request_hash != call.request_hash
        || !receipt.retryable_read
    {
        return Err(EngineError::InvalidActionReceipt(
            "begin action receipt mismatch",
        ));
    }
    Ok(())
}

pub(crate) fn validate_observed_receipt(
    receipt: &ActionReceipt,
    call: &PreparedCall,
    result_hash: &ContentHash,
) -> Result<(), EngineError> {
    if receipt.action_key != call.action_key
        || receipt.request_hash != call.request_hash
        || receipt.result_hash.as_ref() != Some(result_hash)
        || receipt.stage != ActionStage::Observed
    {
        return Err(EngineError::InvalidActionReceipt(
            "observed action receipt mismatch",
        ));
    }
    Ok(())
}

pub(crate) fn validate_finalized_receipt(
    receipt: &ActionFinalizationReceipt,
    call: &PreparedCall,
    result_hash: &ContentHash,
    disposition: ActionDisposition,
    validation_receipt_hash: &ContentHash,
    policy_receipt_hash: &ContentHash,
) -> Result<(), EngineError> {
    if receipt.action.action_key != call.action_key
        || receipt.action.request_hash != call.request_hash
        || receipt.action.result_hash.as_ref() != Some(result_hash)
        || receipt.action.stage != disposition.stage()
        || receipt.disposition != disposition
        || receipt.validation_receipt_hash != *validation_receipt_hash
        || receipt.policy_receipt_hash != *policy_receipt_hash
    {
        return Err(EngineError::InvalidActionReceipt(
            "finalized action receipt mismatch",
        ));
    }
    Ok(())
}

pub(crate) fn validate_capability_result(
    call: &PreparedCall,
    result: &CapabilityResult,
) -> Result<(), EngineError> {
    for evidence in &result.evidence {
        if evidence.source.capability_id != call.capability.id
            || evidence.source.action_key != call.action_key
            || evidence.source.server_build != call.binding.server_build
            || evidence.source.normalized_contract_hash != call.normalized_output_contract_hash
            || evidence.source.server_schema_bundle_hash != call.binding.server_schema_bundle_hash
            || evidence.source.data_release_hash != call.binding.data_release_hash
        {
            return Err(EngineError::InvalidCapabilityResult(
                "evidence provenance does not match the pinned invocation",
            ));
        }
    }
    Ok(())
}

pub(crate) fn accepted_action_receipt_hash(
    call: &PreparedCall,
    result: &CapabilityResult,
) -> Result<ContentHash, EngineError> {
    #[derive(Serialize)]
    #[serde(deny_unknown_fields)]
    struct AcceptedReceiptHashInput<'a> {
        disposition: &'static str,
        action_key: &'a str,
        request_hash: &'a ContentHash,
        result_hash: ContentHash,
    }
    let result_hash = ContentHash::sha256(serde_jcs::to_vec(result)?);
    Ok(ContentHash::sha256(serde_jcs::to_vec(
        &AcceptedReceiptHashInput {
            disposition: "accepted",
            action_key: &call.action_key,
            request_hash: &call.request_hash,
            result_hash,
        },
    )?))
}

pub(crate) fn validate_calculations(
    answer: &AnswerIr,
    committed: &BTreeMap<String, Calculation>,
) -> Result<(), EngineError> {
    for calculation in &answer.calculations {
        if committed.get(&calculation.calculation_id) != Some(calculation) {
            return Err(EngineError::UncommittedCalculation(
                calculation.calculation_id.clone(),
            ));
        }
    }
    Ok(())
}

pub(crate) fn answer_policy(image: &AgentImageManifest) -> AnswerPolicy {
    AnswerPolicy {
        forbidden_terms: image.body.answer_policy.forbidden_user_terms.clone(),
        // The locale contract comes from the image's declared answer locale
        // (Korean images keep ko-KR; the English image declares en-US).
        expected_locale: image.body.answer_policy.locale.clone(),
        require_direct_strong_claims: image
            .body
            .evidence_policy
            .require_load_bearing_direct_premise,
        require_period_for_numbers: true,
        require_unit_for_numbers: true,
        require_counter_signal_for_interpretation: image
            .body
            .evidence_policy
            .require_counter_signal_for_inference,
        exact_follow_up_count: usize::from(image.body.answer_policy.exact_follow_up_count),
    }
}

pub(crate) fn issue_codes(issues: &[ValidationIssue]) -> Vec<String> {
    issues.iter().map(|issue| issue.code.into()).collect()
}

pub(crate) fn mutation_id(operation: &str, run_id: &str, payload: &ContentHash) -> String {
    ContentHash::sha256(format!("mutation/v1\0{operation}\0{run_id}\0{payload}")).to_string()
}

pub(crate) fn action_mutation_id(
    operation: &str,
    run_id: &str,
    action_key: &str,
    payload: &ContentHash,
) -> String {
    ContentHash::sha256(format!(
        "mutation/v1\0{operation}\0{run_id}\0{action_key}\0{payload}"
    ))
    .to_string()
}

pub(crate) fn ensure_before(deadline: Instant, phase: &'static str) -> Result<(), EngineError> {
    if Instant::now() >= deadline {
        Err(EngineError::DeadlineExceeded(phase))
    } else {
        Ok(())
    }
}

pub(crate) fn ensure_size(
    observed: usize,
    limit: usize,
    resource: &'static str,
) -> Result<(), EngineError> {
    if observed > limit {
        Err(EngineError::SizeLimit {
            resource,
            observed,
            limit,
        })
    } else {
        Ok(())
    }
}

pub(crate) fn scrub_json(value: &mut Value) {
    match value {
        Value::String(value) => value.zeroize(),
        Value::Array(values) => {
            for value in values {
                scrub_json(value);
            }
        }
        Value::Object(values) => {
            for value in values.values_mut() {
                scrub_json(value);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}
