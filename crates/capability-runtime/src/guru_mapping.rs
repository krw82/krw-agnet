//! Closed mappings and compact run-local authorization for the default sealed
//! Guru workflow.

use krw_agent_contracts::{
    KRW_GURU_COMPANY_BRIEF_INPUT_V1, KRW_GURU_COMPANY_BRIEF_RESULT_V1,
    KRW_GURU_EVIDENCE_REVIEW_INPUT_V1, KRW_GURU_EVIDENCE_REVIEW_RESULT_V1,
    KRW_GURU_INPUT_CORRECTION_V1, KRW_GURU_QUERY_CONTEXT_INPUT_V1,
    KRW_GURU_QUERY_CONTEXT_RESULT_V1, NORMALIZED_CAPABILITY_RESULT_V1,
    build_company_research_context, build_evidence_review_input,
    enrich_guru_query_input_with_result_context, research_pack_identity,
    validate_company_brief_input_identity, validate_company_brief_result_linkage,
    validate_evidence_review_exchange, validate_evidence_review_input_origin,
    validate_guru_company_search_plan, validate_guru_query_exchange, validate_value,
};
use krw_agent_execution_contracts::DependencyFailure;
use serde_json::{Value, json};

use super::reject;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GuruMapping {
    QueryContext,
    CompanyBrief,
    EvidenceReview,
}

impl GuruMapping {
    pub(crate) const fn input_contract(self) -> &'static str {
        match self {
            Self::QueryContext => KRW_GURU_QUERY_CONTEXT_INPUT_V1,
            Self::CompanyBrief => KRW_GURU_COMPANY_BRIEF_INPUT_V1,
            Self::EvidenceReview => KRW_GURU_EVIDENCE_REVIEW_INPUT_V1,
        }
    }

    pub(crate) const fn success_contract(self) -> &'static str {
        match self {
            Self::QueryContext => KRW_GURU_QUERY_CONTEXT_RESULT_V1,
            Self::CompanyBrief => KRW_GURU_COMPANY_BRIEF_RESULT_V1,
            Self::EvidenceReview => KRW_GURU_EVIDENCE_REVIEW_RESULT_V1,
        }
    }

    pub(crate) const fn output_contracts(self) -> &'static [&'static str] {
        match self {
            Self::QueryContext => &[
                KRW_GURU_QUERY_CONTEXT_RESULT_V1,
                NORMALIZED_CAPABILITY_RESULT_V1,
            ],
            Self::CompanyBrief => &[
                KRW_GURU_COMPANY_BRIEF_RESULT_V1,
                KRW_GURU_INPUT_CORRECTION_V1,
                NORMALIZED_CAPABILITY_RESULT_V1,
            ],
            Self::EvidenceReview => &[
                KRW_GURU_EVIDENCE_REVIEW_RESULT_V1,
                KRW_GURU_INPUT_CORRECTION_V1,
                NORMALIZED_CAPABILITY_RESULT_V1,
            ],
        }
    }
}

#[derive(Debug, Clone)]
struct CommittedGuruQuery {
    /// The physical request is enriched only after the trusted Guru result
    /// supplies the neutral light company context. It retains neither a
    /// model-authored context nor the potentially 2 MiB `ResearchPack`.
    input: Value,
    research_pack_identity: String,
}

#[derive(Debug, Default)]
pub(crate) struct GuruRunState {
    query: Option<CommittedGuruQuery>,
    sealed_brief: Option<Value>,
    company_research_context: Option<Value>,
    review_complete: bool,
}

impl GuruRunState {
    pub(crate) fn authorize(
        &self,
        mapping: GuruMapping,
        arguments: &Value,
    ) -> Result<(), DependencyFailure> {
        match mapping {
            GuruMapping::QueryContext => {
                if self.query.is_some() || self.sealed_brief.is_some() {
                    return Err(reject(
                        "guru_query_already_committed",
                        "Guru retrieval may be committed only once per run",
                    ));
                }
                validate_value(KRW_GURU_QUERY_CONTEXT_INPUT_V1, arguments)
                    .map_err(|error| reject("guru_query_input_invalid", format!("{error:?}")))
            }
            GuruMapping::CompanyBrief => {
                if self.sealed_brief.is_some() {
                    return Err(reject(
                        "guru_brief_already_sealed",
                        "the immutable Guru brief is already sealed",
                    ));
                }
                let query = self.query.as_ref().ok_or_else(|| {
                    reject(
                        "guru_query_required",
                        "Guru retrieval must commit before company-brief sealing",
                    )
                })?;
                validate_company_brief_input_identity(
                    &query.input,
                    &query.research_pack_identity,
                    arguments,
                )
                .map_err(|error| reject("guru_brief_input_unlinked", format!("{error:?}")))
            }
            GuruMapping::EvidenceReview => {
                if self.review_complete {
                    return Err(reject(
                        "guru_review_already_committed",
                        "the sealed Guru evidence review is already committed",
                    ));
                }
                let brief = self.sealed_brief.as_ref().ok_or_else(|| {
                    reject(
                        "guru_sealed_brief_required",
                        "a committed sealed brief must precede evidence review",
                    )
                })?;
                let context = self.company_research_context.as_ref().ok_or_else(|| {
                    reject(
                        "guru_company_evidence_required",
                        "committed filing evidence must precede Guru review",
                    )
                })?;
                let query = self.query.as_ref().ok_or_else(|| {
                    reject(
                        "guru_query_required",
                        "the committed fixed-author retrieval identity disappeared",
                    )
                })?;
                validate_evidence_review_input_origin(&query.input, arguments)
                    .map_err(|error| reject("guru_review_input_unlinked", format!("{error:?}")))?;
                let analysis = arguments.get("agent_analysis").ok_or_else(|| {
                    reject(
                        "guru_review_analysis_missing",
                        "the physical review input omitted its typed analysis",
                    )
                })?;
                let expected = build_evidence_review_input(&query.input, brief, context, analysis)
                    .map_err(|error| reject("guru_review_input_unlinked", format!("{error:?}")))?;
                if expected == *arguments {
                    Ok(())
                } else {
                    Err(reject(
                        "guru_review_input_unlinked",
                        "the physical review input differs from committed artifacts",
                    ))
                }
            }
        }
    }

    /// Authorize one kernel-assembled physical invocation. The adapter does
    /// not synthesize private envelopes from model-authored input.
    pub(crate) fn prepare_invocation(
        &self,
        mapping: GuruMapping,
        arguments: &Value,
    ) -> Result<Value, DependencyFailure> {
        self.authorize(mapping, arguments)?;
        Ok(arguments.clone())
    }

    #[cfg(test)]
    pub(crate) fn physical_arguments(
        &self,
        mapping: GuruMapping,
        arguments: &Value,
    ) -> Result<Value, DependencyFailure> {
        self.prepare_invocation(mapping, arguments)
    }

    pub(crate) fn authorize_company_search_plan(
        &self,
        arguments: &Value,
    ) -> Result<(), DependencyFailure> {
        self.authorize_company_evidence()?;
        let brief = self
            .sealed_brief
            .as_ref()
            .ok_or_else(|| reject("guru_sealed_brief_required", "sealed brief disappeared"))?;
        validate_guru_company_search_plan(brief, arguments)
            .map_err(|error| reject("guru_search_plan_unlinked", format!("{error:?}")))
    }

    pub(crate) fn authorize_company_evidence(&self) -> Result<(), DependencyFailure> {
        if self.sealed_brief.is_none() {
            return Err(reject(
                "guru_sealed_brief_required",
                "company filing research cannot run before the Guru brief is sealed",
            ));
        }
        if self.review_complete {
            return Err(reject(
                "guru_review_already_committed",
                "company filing research cannot continue after sealed review",
            ));
        }
        Ok(())
    }

    pub(crate) fn validate_success(
        &self,
        mapping: GuruMapping,
        arguments: &Value,
        payload: &Value,
    ) -> Result<(), DependencyFailure> {
        validate_value(mapping.success_contract(), payload)
            .map_err(|error| reject("guru_success_contract_invalid", format!("{error:?}")))?;
        match mapping {
            GuruMapping::QueryContext => validate_guru_query_exchange(arguments, payload)
                .map_err(|error| reject("guru_query_exchange_invalid", format!("{error:?}"))),
            GuruMapping::CompanyBrief => {
                let physical = self.prepare_invocation(mapping, arguments)?;
                validate_company_brief_result_linkage(&physical, payload)
                    .map_err(|error| reject("guru_brief_exchange_invalid", format!("{error:?}")))
            }
            GuruMapping::EvidenceReview => {
                let brief = self.sealed_brief.as_ref().ok_or_else(|| {
                    reject("guru_sealed_brief_required", "sealed brief disappeared")
                })?;
                let context = self.company_research_context.as_ref().ok_or_else(|| {
                    reject(
                        "guru_company_evidence_required",
                        "filing context disappeared",
                    )
                })?;
                let physical = self.prepare_invocation(mapping, arguments)?;
                validate_evidence_review_exchange(brief, context, &physical, payload)
                    .map_err(|error| reject("guru_review_exchange_invalid", format!("{error:?}")))
            }
        }
    }

    pub(crate) fn apply_committed(
        &mut self,
        mapping: GuruMapping,
        expected_mcp_tool_name: &str,
        arguments: &Value,
        payload: &Value,
    ) -> Result<(), DependencyFailure> {
        if is_correction(payload) {
            validate_correction_for_mapping(mapping, expected_mcp_tool_name, payload)?;
            return Ok(());
        }
        self.validate_success(mapping, arguments, payload)?;
        match mapping {
            GuruMapping::QueryContext => {
                let pack = payload.get("research_pack").ok_or_else(|| {
                    reject("guru_pack_missing", "query result has no ResearchPack")
                })?;
                let identity = research_pack_identity(pack)
                    .map_err(|error| reject("guru_pack_invalid", format!("{error:?}")))?;
                let enriched_input = enrich_guru_query_input_with_result_context(
                    arguments, payload,
                )
                .map_err(|error| reject("guru_query_context_invalid", format!("{error:?}")))?;
                self.query = Some(CommittedGuruQuery {
                    input: enriched_input,
                    research_pack_identity: identity,
                });
            }
            GuruMapping::CompanyBrief => {
                let brief = payload
                    .get("investigation_brief")
                    .filter(|value| !value.is_null())
                    .ok_or_else(|| {
                        reject(
                            "guru_brief_missing",
                            "successful default result has no seal",
                        )
                    })?;
                self.sealed_brief = Some(brief.clone());
            }
            GuruMapping::EvidenceReview => {
                self.review_complete = true;
            }
        }
        Ok(())
    }

    pub(crate) fn observe_company_evidence(
        &mut self,
        payload: &Value,
    ) -> Result<(), DependencyFailure> {
        self.authorize_company_evidence()?;
        let brief = self
            .sealed_brief
            .as_ref()
            .ok_or_else(|| reject("guru_sealed_brief_required", "sealed brief disappeared"))?;
        let Ok(newest) = build_company_research_context(brief, std::slice::from_ref(payload))
        else {
            // A successful filing call may contain no matching evidence. It
            // does not erase previously committed context.
            return Ok(());
        };
        let Some(previous) = self.company_research_context.as_ref() else {
            self.company_research_context = Some(newest);
            return Ok(());
        };
        let older_result = json!({
            "contract_version": "research-state/v2",
            "evidence_units": previous["evidence_units"],
        });
        let newer_result = json!({
            "contract_version": "research-state/v2",
            "evidence_units": newest["evidence_units"],
        });
        self.company_research_context = Some(
            build_company_research_context(brief, &[older_result, newer_result])
                .map_err(|error| reject("guru_company_context_invalid", format!("{error:?}")))?,
        );
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn company_research_context(&self) -> Option<&Value> {
        self.company_research_context.as_ref()
    }
}

pub(crate) fn is_correction(payload: &Value) -> bool {
    payload.get("status").and_then(Value::as_str) == Some("input_correction_required")
}

pub(crate) fn validate_correction_for_mapping(
    mapping: GuruMapping,
    expected_mcp_tool_name: &str,
    payload: &Value,
) -> Result<(), DependencyFailure> {
    if mapping == GuruMapping::QueryContext {
        return Err(reject(
            "guru_query_correction_not_declared",
            "Guru query context has no typed correction result",
        ));
    }
    validate_value(KRW_GURU_INPUT_CORRECTION_V1, payload)
        .map_err(|error| reject("guru_correction_invalid", format!("{error:?}")))?;
    let exact_next_tool = payload
        .get("allowed_next_tools")
        .and_then(Value::as_array)
        .is_some_and(|tools| tools.len() == 1 && tools[0].as_str() == Some(expected_mcp_tool_name));
    if exact_next_tool {
        Ok(())
    } else {
        Err(reject(
            "guru_correction_mapping_mismatch",
            "Guru correction is not bound to the invoked capability",
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use krw_agent_contracts::{
        KRW_GURU_COMPANY_BRIEF_INPUT_V1, KRW_GURU_COMPANY_BRIEF_RESULT_V1,
        KRW_GURU_COMPANY_RESEARCH_CONTEXT_V1, KRW_GURU_EVIDENCE_REVIEW_INPUT_V1,
        KRW_GURU_EVIDENCE_REVIEW_RESULT_V1, KRW_GURU_INPUT_CORRECTION_V1,
        KRW_GURU_QUERY_CONTEXT_INPUT_V1, KRW_GURU_QUERY_CONTEXT_RESULT_V1,
    };

    use super::*;

    fn root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    fn vector(contract_id: &str) -> Value {
        let vectors: Value = serde_json::from_slice(
            &fs::read(root().join("contracts/krw-guru/v1/conformance-vectors.json"))
                .expect("Guru conformance vectors"),
        )
        .expect("Guru conformance JSON");
        vectors["vectors"]
            .as_array()
            .expect("Guru vector array")
            .iter()
            .find(|candidate| {
                candidate["contract_id"] == contract_id
                    && candidate["name"] == "minimal_valid"
                    && candidate["valid"] == true
            })
            .expect("minimal valid Guru vector")["value"]
            .clone()
    }

    fn observed_filing_result() -> Value {
        let context = vector(KRW_GURU_COMPANY_RESEARCH_CONTEXT_V1);
        json!({
            "contract_version": "research-state/v2",
            "evidence_units": context["evidence_units"],
        })
    }

    #[test]
    fn sealed_workflow_is_authorized_only_in_committed_order() {
        let query_input = vector(KRW_GURU_QUERY_CONTEXT_INPUT_V1);
        let query_result = vector(KRW_GURU_QUERY_CONTEXT_RESULT_V1);
        let brief_input = vector(KRW_GURU_COMPANY_BRIEF_INPUT_V1);
        let brief_result = vector(KRW_GURU_COMPANY_BRIEF_RESULT_V1);
        let review_input = vector(KRW_GURU_EVIDENCE_REVIEW_INPUT_V1);
        let review_result = vector(KRW_GURU_EVIDENCE_REVIEW_RESULT_V1);
        let expected_context = vector(KRW_GURU_COMPANY_RESEARCH_CONTEXT_V1);
        let mut state = GuruRunState::default();

        assert_eq!(
            state
                .authorize_company_evidence()
                .expect_err("filing lookup before seal")
                .code,
            "guru_sealed_brief_required"
        );
        assert_eq!(
            state
                .authorize(GuruMapping::CompanyBrief, &brief_input)
                .expect_err("brief before retrieval commit")
                .code,
            "guru_query_required"
        );

        state
            .authorize(GuruMapping::QueryContext, &query_input)
            .expect("one fixed-author retrieval");
        state
            .apply_committed(
                GuruMapping::QueryContext,
                "krw_guru_query_context",
                &query_input,
                &query_result,
            )
            .expect("commit exact retrieval exchange");
        assert_eq!(
            state
                .authorize(GuruMapping::QueryContext, &query_input)
                .expect_err("second retrieval")
                .code,
            "guru_query_already_committed"
        );

        state
            .authorize(GuruMapping::CompanyBrief, &brief_input)
            .expect("brief carries exact committed pack identity");
        state
            .apply_committed(
                GuruMapping::CompanyBrief,
                "krw_guru_company_brief",
                &brief_input,
                &brief_result,
            )
            .expect("commit immutable brief seal");
        let sealed_plan = json!({
            "question": "Does Services support resilience beyond replacement demand?",
            "intent": "company_research",
            "tickers": ["AAPL"],
            "clauses": [
                {
                    "clause_id": "segment_disclosure",
                    "retrieval_query": "AAPL segment disclosure",
                    "required_concepts": ["segment disclosure"],
                    "required": true,
                    "tickers": ["AAPL"]
                },
                {
                    "clause_id": "revenue_driver",
                    "retrieval_query": "AAPL revenue driver discussion",
                    "required_concepts": ["revenue driver discussion"],
                    "required": true,
                    "tickers": ["AAPL"]
                }
            ]
        });
        state
            .authorize_company_search_plan(&sealed_plan)
            .expect("one sealed question becomes a bounded filing plan");
        let mut unrelated_plan = sealed_plan;
        unrelated_plan["question"] = json!("Research an unrelated company topic");
        assert_eq!(
            state
                .authorize_company_search_plan(&unrelated_plan)
                .expect_err("unsealed research plan")
                .code,
            "guru_search_plan_unlinked"
        );
        state
            .authorize_company_evidence()
            .expect("filing lookup after seal");
        state
            .observe_company_evidence(&observed_filing_result())
            .expect("derive context from committed filing result");
        assert_eq!(state.company_research_context(), Some(&expected_context));

        state
            .authorize(GuruMapping::EvidenceReview, &review_input)
            .expect("review exact sealed artifacts");
        state
            .apply_committed(
                GuruMapping::EvidenceReview,
                "krw_guru_review_company_evidence",
                &review_input,
                &review_result,
            )
            .expect("commit exact review exchange");
        assert_eq!(
            state
                .authorize_company_evidence()
                .expect_err("filing lookup after review")
                .code,
            "guru_review_already_committed"
        );
        assert_eq!(
            state
                .authorize(GuruMapping::EvidenceReview, &review_input)
                .expect_err("second review")
                .code,
            "guru_review_already_committed"
        );
    }

    #[test]
    fn physical_inputs_must_exactly_match_kernel_sealed_artifacts() {
        let query_input = vector(KRW_GURU_QUERY_CONTEXT_INPUT_V1);
        let query_result = vector(KRW_GURU_QUERY_CONTEXT_RESULT_V1);
        let brief_input = vector(KRW_GURU_COMPANY_BRIEF_INPUT_V1);
        let brief_result = vector(KRW_GURU_COMPANY_BRIEF_RESULT_V1);
        let mut state = GuruRunState::default();
        state
            .apply_committed(
                GuruMapping::QueryContext,
                "krw_guru_query_context",
                &query_input,
                &query_result,
            )
            .expect("commit query");

        let physical_brief = state
            .physical_arguments(GuruMapping::CompanyBrief, &brief_input)
            .expect("kernel-sealed brief request");
        assert_eq!(physical_brief, brief_input);
        assert_eq!(physical_brief["author_keys"], json!(["buffett"]));
        assert_eq!(physical_brief["ticker"], json!("AAPL"));
        let mut echoed_pack = brief_input.clone();
        echoed_pack["guru_query_context"]["pack_meta"]["pack_id"] = json!("foreign-pack");
        assert_eq!(
            state
                .prepare_invocation(GuruMapping::CompanyBrief, &echoed_pack)
                .expect_err("the kernel envelope cannot replace the committed pack")
                .code,
            "guru_brief_input_unlinked"
        );
        let mut foreign_principle = brief_input.clone();
        foreign_principle["investigation_questions"][0]["guru_principle_ids"] =
            json!(["guru:marks:invented"]);
        assert_eq!(
            state
                .prepare_invocation(GuruMapping::CompanyBrief, &foreign_principle)
                .expect_err("foreign principle")
                .code,
            "guru_brief_input_unlinked"
        );

        state
            .apply_committed(
                GuruMapping::CompanyBrief,
                "krw_guru_company_brief",
                &brief_input,
                &brief_result,
            )
            .expect("commit seal");
        state
            .observe_company_evidence(&observed_filing_result())
            .expect("observe filing result");
        let canonical_review = vector(KRW_GURU_EVIDENCE_REVIEW_INPUT_V1);
        let physical_review = state
            .physical_arguments(GuruMapping::EvidenceReview, &canonical_review)
            .expect("kernel-sealed review request");
        assert_eq!(physical_review, canonical_review);

        let mut echoed_brief = canonical_review.clone();
        echoed_brief["investigation_brief"]["ticker"] = json!("MSFT");
        assert_eq!(
            state
                .prepare_invocation(GuruMapping::EvidenceReview, &echoed_brief)
                .expect_err("the kernel envelope cannot replace the sealed brief")
                .code,
            "guru_review_input_unlinked"
        );

        let mut invented_evidence = canonical_review;
        invented_evidence["agent_analysis"]["assessments"][0]["evidence_object_ids"] =
            json!(["claim:AAPL:invented"]);
        assert_eq!(
            state
                .prepare_invocation(GuruMapping::EvidenceReview, &invented_evidence)
                .expect_err("invented evidence")
                .code,
            "guru_review_input_unlinked"
        );
    }

    #[test]
    fn corrections_are_typed_and_bound_to_the_exact_capability() {
        let review_correction = vector(KRW_GURU_INPUT_CORRECTION_V1);
        validate_correction_for_mapping(
            GuruMapping::EvidenceReview,
            "krw_guru_review_company_evidence",
            &review_correction,
        )
        .expect("review correction");
        assert_eq!(
            validate_correction_for_mapping(
                GuruMapping::CompanyBrief,
                "krw_guru_company_brief",
                &review_correction,
            )
            .expect_err("review correction on brief capability")
            .code,
            "guru_correction_mapping_mismatch"
        );
        assert_eq!(
            validate_correction_for_mapping(
                GuruMapping::QueryContext,
                "krw_guru_query_context",
                &review_correction,
            )
            .expect_err("query has no correction contract")
            .code,
            "guru_query_correction_not_declared"
        );
    }
}
