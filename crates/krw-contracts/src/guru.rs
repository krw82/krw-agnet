//! Canonical contracts for the default sealed Guru workflow.
//!
//! Authority is split intentionally:
//!
//! - `krw-agent` owns host scope and the trusted neutral light-company
//!   projection, while the ontology runtime supplies its indexed vocabulary;
//! - `krw-ontology` owns `ResearchPack`, brief sealing, correction, and evidence
//!   review semantics.
//!
//! The generated artifacts pin both repositories.  This module validates the
//! closed default path only: brief -> company filing evidence -> review.  It
//! does not model the legacy company-pack path and does not invent an ontology
//! evidence-verifier call.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{ContractArtifactError, ContractDescriptor, ContractValueError, SEARCH_PLAN_V2};

include!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../contracts/krw-guru/v1/bindings.rs"
));

macro_rules! schema_bytes {
    ($name:ident, $file:literal) => {
        const $name: &[u8] = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../contracts/krw-guru/v1/schemas/",
            $file
        ));
    };
}

schema_bytes!(
    QUERY_CONTEXT_INPUT_BYTES,
    "krw-guru-query-context-input-v1.json"
);
schema_bytes!(
    QUERY_CONTEXT_RESULT_BYTES,
    "krw-guru-query-context-result-v1.json"
);
schema_bytes!(RESEARCH_PACK_BYTES, "krw-guru-research-pack-v1.json");
schema_bytes!(
    LIGHT_COMPANY_CONTEXT_BYTES,
    "krw-guru-light-company-context-v1.json"
);
schema_bytes!(
    INVESTIGATION_DRAFT_BYTES,
    "krw-guru-investigation-question-draft-v1.json"
);
schema_bytes!(
    COMPANY_BRIEF_INPUT_BYTES,
    "krw-guru-company-brief-input-v1.json"
);
schema_bytes!(
    INVESTIGATION_BRIEF_BYTES,
    "krw-guru-investigation-brief-v1.json"
);
schema_bytes!(
    COMPANY_BRIEF_RESULT_BYTES,
    "krw-guru-company-brief-result-v1.json"
);
schema_bytes!(INPUT_CORRECTION_BYTES, "krw-guru-input-correction-v1.json");
schema_bytes!(
    COMPANY_RESEARCH_CONTEXT_BYTES,
    "krw-guru-company-research-context-v1.json"
);
schema_bytes!(
    AGENT_EVIDENCE_ANALYSIS_BYTES,
    "krw-guru-agent-evidence-analysis-v1.json"
);
schema_bytes!(
    EVIDENCE_REVIEW_INPUT_BYTES,
    "krw-guru-evidence-review-input-v1.json"
);
schema_bytes!(
    VALIDATED_EVIDENCE_ANALYSIS_BYTES,
    "krw-guru-validated-evidence-analysis-v1.json"
);
schema_bytes!(
    EVIDENCE_REVIEW_RESULT_BYTES,
    "krw-guru-evidence-review-result-v1.json"
);

const MANIFEST_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../contracts/krw-guru/v1/manifest.json"
));
const BUNDLE_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../contracts/krw-guru/v1/schema-bundle.json"
));
const VECTOR_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../contracts/krw-guru/v1/conformance-vectors.json"
));

const CONTRACT_COUNT: usize = 14;
const MAX_GURU_METRIC_POINTS_PER_UNIT: usize = 128;
const GURU_FORMAT: &str = "krw-guru-workflow-contract-export/v1";
const CONFORMANCE_FORMAT: &str = "krw-guru-workflow-contract-conformance/v1";
const RESEARCH_PACK_FORMAT: &str = "krw-guru-research-pack/v1";
const LIGHT_CONTEXT_FORMAT: &str = "krw-guru-light-company-context/v1";
const INVESTIGATION_BRIEF_FORMAT: &str = "krw-guru-investigation-brief/v1";
const COMPANY_RESEARCH_CONTEXT_FORMAT: &str = "krw-guru-company-research-context/v1";
const VALIDATED_EVIDENCE_FORMAT: &str = "krw-guru-validated-evidence-analysis/v1";

macro_rules! descriptor {
    ($id:ident, $hash:ident, $bytes:ident) => {
        ContractDescriptor {
            id: $id,
            schema_sha256: $hash,
            schema: $bytes,
        }
    };
}

pub(super) fn contract(contract_id: &str) -> Option<ContractDescriptor> {
    Some(match contract_id {
        KRW_GURU_QUERY_CONTEXT_INPUT_V1 => descriptor!(
            KRW_GURU_QUERY_CONTEXT_INPUT_V1,
            KRW_GURU_QUERY_CONTEXT_INPUT_V1_SCHEMA_SHA256,
            QUERY_CONTEXT_INPUT_BYTES
        ),
        KRW_GURU_QUERY_CONTEXT_RESULT_V1 => descriptor!(
            KRW_GURU_QUERY_CONTEXT_RESULT_V1,
            KRW_GURU_QUERY_CONTEXT_RESULT_V1_SCHEMA_SHA256,
            QUERY_CONTEXT_RESULT_BYTES
        ),
        KRW_GURU_RESEARCH_PACK_V1 => descriptor!(
            KRW_GURU_RESEARCH_PACK_V1,
            KRW_GURU_RESEARCH_PACK_V1_SCHEMA_SHA256,
            RESEARCH_PACK_BYTES
        ),
        KRW_GURU_LIGHT_COMPANY_CONTEXT_V1 => descriptor!(
            KRW_GURU_LIGHT_COMPANY_CONTEXT_V1,
            KRW_GURU_LIGHT_COMPANY_CONTEXT_V1_SCHEMA_SHA256,
            LIGHT_COMPANY_CONTEXT_BYTES
        ),
        KRW_GURU_INVESTIGATION_QUESTION_DRAFT_V1 => descriptor!(
            KRW_GURU_INVESTIGATION_QUESTION_DRAFT_V1,
            KRW_GURU_INVESTIGATION_QUESTION_DRAFT_V1_SCHEMA_SHA256,
            INVESTIGATION_DRAFT_BYTES
        ),
        KRW_GURU_COMPANY_BRIEF_INPUT_V1 => descriptor!(
            KRW_GURU_COMPANY_BRIEF_INPUT_V1,
            KRW_GURU_COMPANY_BRIEF_INPUT_V1_SCHEMA_SHA256,
            COMPANY_BRIEF_INPUT_BYTES
        ),
        KRW_GURU_INVESTIGATION_BRIEF_V1 => descriptor!(
            KRW_GURU_INVESTIGATION_BRIEF_V1,
            KRW_GURU_INVESTIGATION_BRIEF_V1_SCHEMA_SHA256,
            INVESTIGATION_BRIEF_BYTES
        ),
        KRW_GURU_COMPANY_BRIEF_RESULT_V1 => descriptor!(
            KRW_GURU_COMPANY_BRIEF_RESULT_V1,
            KRW_GURU_COMPANY_BRIEF_RESULT_V1_SCHEMA_SHA256,
            COMPANY_BRIEF_RESULT_BYTES
        ),
        KRW_GURU_INPUT_CORRECTION_V1 => descriptor!(
            KRW_GURU_INPUT_CORRECTION_V1,
            KRW_GURU_INPUT_CORRECTION_V1_SCHEMA_SHA256,
            INPUT_CORRECTION_BYTES
        ),
        KRW_GURU_COMPANY_RESEARCH_CONTEXT_V1 => descriptor!(
            KRW_GURU_COMPANY_RESEARCH_CONTEXT_V1,
            KRW_GURU_COMPANY_RESEARCH_CONTEXT_V1_SCHEMA_SHA256,
            COMPANY_RESEARCH_CONTEXT_BYTES
        ),
        KRW_GURU_AGENT_EVIDENCE_ANALYSIS_V1 => descriptor!(
            KRW_GURU_AGENT_EVIDENCE_ANALYSIS_V1,
            KRW_GURU_AGENT_EVIDENCE_ANALYSIS_V1_SCHEMA_SHA256,
            AGENT_EVIDENCE_ANALYSIS_BYTES
        ),
        KRW_GURU_EVIDENCE_REVIEW_INPUT_V1 => descriptor!(
            KRW_GURU_EVIDENCE_REVIEW_INPUT_V1,
            KRW_GURU_EVIDENCE_REVIEW_INPUT_V1_SCHEMA_SHA256,
            EVIDENCE_REVIEW_INPUT_BYTES
        ),
        KRW_GURU_VALIDATED_EVIDENCE_ANALYSIS_V1 => descriptor!(
            KRW_GURU_VALIDATED_EVIDENCE_ANALYSIS_V1,
            KRW_GURU_VALIDATED_EVIDENCE_ANALYSIS_V1_SCHEMA_SHA256,
            VALIDATED_EVIDENCE_ANALYSIS_BYTES
        ),
        KRW_GURU_EVIDENCE_REVIEW_RESULT_V1 => descriptor!(
            KRW_GURU_EVIDENCE_REVIEW_RESULT_V1,
            KRW_GURU_EVIDENCE_REVIEW_RESULT_V1_SCHEMA_SHA256,
            EVIDENCE_REVIEW_RESULT_BYTES
        ),
        _ => return None,
    })
}

pub(super) fn descriptors() -> [ContractDescriptor; CONTRACT_COUNT] {
    [
        contract(KRW_GURU_QUERY_CONTEXT_INPUT_V1).expect("static contract"),
        contract(KRW_GURU_QUERY_CONTEXT_RESULT_V1).expect("static contract"),
        contract(KRW_GURU_RESEARCH_PACK_V1).expect("static contract"),
        contract(KRW_GURU_LIGHT_COMPANY_CONTEXT_V1).expect("static contract"),
        contract(KRW_GURU_INVESTIGATION_QUESTION_DRAFT_V1).expect("static contract"),
        contract(KRW_GURU_COMPANY_BRIEF_INPUT_V1).expect("static contract"),
        contract(KRW_GURU_INVESTIGATION_BRIEF_V1).expect("static contract"),
        contract(KRW_GURU_COMPANY_BRIEF_RESULT_V1).expect("static contract"),
        contract(KRW_GURU_INPUT_CORRECTION_V1).expect("static contract"),
        contract(KRW_GURU_COMPANY_RESEARCH_CONTEXT_V1).expect("static contract"),
        contract(KRW_GURU_AGENT_EVIDENCE_ANALYSIS_V1).expect("static contract"),
        contract(KRW_GURU_EVIDENCE_REVIEW_INPUT_V1).expect("static contract"),
        contract(KRW_GURU_VALIDATED_EVIDENCE_ANALYSIS_V1).expect("static contract"),
        contract(KRW_GURU_EVIDENCE_REVIEW_RESULT_V1).expect("static contract"),
    ]
}

/// Required JSON `null | T`; a missing field does not deserialize.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GuruRequiredNullable<T>(pub Option<T>);

impl<'de, T> Deserialize<'de> for GuruRequiredNullable<T>
where
    T: Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Option::<T>::deserialize(deserializer).map(Self)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GuruCompanyContextAnchor {
    pub anchor_id: String,
    pub kind: String,
    pub text: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GuruFilingAvailability {
    pub has_current_filing: bool,
    pub has_annual_baseline: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct FrontGuruCompanyContextAnchor {
    anchor_id: String,
    kind: String,
    text: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct FrontGuruFilingAvailability {
    has_current_filing: bool,
    has_annual_baseline: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct FrontGuruLightCompanyContext {
    format: String,
    ticker: String,
    company_name: String,
    sector: Option<String>,
    industry: Option<String>,
    business_description: String,
    primary_activities: Vec<String>,
    products_or_segments: Vec<String>,
    revenue_logic: Option<String>,
    context_anchors: Vec<FrontGuruCompanyContextAnchor>,
    filing_availability: FrontGuruFilingAvailability,
}

impl From<FrontGuruLightCompanyContext> for GuruLightCompanyContext {
    fn from(value: FrontGuruLightCompanyContext) -> Self {
        Self {
            format: value.format,
            ticker: value.ticker,
            company_name: value.company_name,
            sector: value.sector,
            industry: value.industry,
            business_description: value.business_description,
            primary_activities: value.primary_activities,
            products_or_segments: value.products_or_segments,
            revenue_logic: value.revenue_logic,
            context_anchors: value
                .context_anchors
                .into_iter()
                .map(|anchor| GuruCompanyContextAnchor {
                    anchor_id: anchor.anchor_id,
                    kind: anchor.kind,
                    text: anchor.text,
                })
                .collect(),
            filing_availability: GuruFilingAvailability {
                has_current_filing: value.filing_availability.has_current_filing,
                has_annual_baseline: value.filing_availability.has_annual_baseline,
            },
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GuruLightCompanyContext {
    pub format: String,
    pub ticker: String,
    pub company_name: String,
    pub sector: Option<String>,
    pub industry: Option<String>,
    pub business_description: String,
    pub primary_activities: Vec<String>,
    pub products_or_segments: Vec<String>,
    pub revenue_logic: Option<String>,
    pub context_anchors: Vec<GuruCompanyContextAnchor>,
    pub filing_availability: GuruFilingAvailability,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GuruQueryContextInput {
    pub question: String,
    pub author_keys: Option<Vec<String>>,
    pub ticker: Option<String>,
    pub company_context: Option<GuruLightCompanyContext>,
    pub intent_family: Option<String>,
    pub limit_lens: Option<u8>,
    pub limit_consultation: Option<u8>,
    pub limit_data_needs: Option<u8>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GuruResearchPackMeta {
    pub pack_id: String,
    pub guru_keys: Vec<String>,
    pub question: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub corpus_version: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GuruAnswerability {
    pub direct_source_match: bool,
    pub confidence: String,
    pub source_match_strength: String,
    pub recommended_answer_mode: String,
    pub reason: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GuruResearchIntent {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub family: Option<String>,
    pub requires_company_evidence: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ticker: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct GuruResearchPack {
    pub format: String,
    pub research_status: String,
    pub pack_meta: GuruResearchPackMeta,
    pub answerability: GuruAnswerability,
    pub intent: GuruResearchIntent,
    pub persona_profile: BTreeMap<String, Value>,
    pub philosophy_context: BTreeMap<String, Value>,
    pub selected_lenses: Vec<BTreeMap<String, Value>>,
    pub consultation_moves: Vec<BTreeMap<String, Value>>,
    pub data_needs: Vec<BTreeMap<String, Value>>,
    pub company_context: BTreeMap<String, Value>,
    pub source_anchors: Vec<BTreeMap<String, Value>>,
    pub clarifying_questions: Vec<String>,
    pub company_bridge: BTreeMap<String, Value>,
    pub trace_recommendations: Vec<BTreeMap<String, Value>>,
    pub agent_autonomy: BTreeMap<String, Value>,
    pub do_not_call: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GuruSelectedAuthor {
    pub author_key: String,
    pub display_name: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct GuruQueryContextResult {
    pub research_context_version: String,
    pub research_status: String,
    pub answerability: GuruAnswerability,
    pub intent: GuruResearchIntent,
    pub runtime: BTreeMap<String, Value>,
    pub selected_author_keys: Vec<String>,
    pub selected_authors: Vec<GuruSelectedAuthor>,
    pub requires_company_evidence: bool,
    pub company_context: BTreeMap<String, Value>,
    pub filing_evidence_requirements: Vec<String>,
    pub agent_autonomy: BTreeMap<String, Value>,
    pub do_not_call: Vec<String>,
    pub research_pack: GuruResearchPack,
    pub usage: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GuruInvestigationQuestionDraft {
    pub question: String,
    pub guru_principle_ids: Vec<String>,
    pub company_context_anchor_ids: Vec<String>,
    pub hypothesis: String,
    pub counter_hypothesis: String,
    pub evidence_needed: Vec<String>,
    pub strengthens_if: String,
    pub weakens_if: String,
    pub why_material: String,
    pub decision_role: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GuruInvestigationQuestion {
    pub question: String,
    pub guru_principle_ids: Vec<String>,
    pub company_context_anchor_ids: Vec<String>,
    pub hypothesis: String,
    pub counter_hypothesis: String,
    pub evidence_needed: Vec<String>,
    pub strengthens_if: String,
    pub weakens_if: String,
    pub why_material: String,
    pub decision_role: String,
    pub question_id: String,
}

impl GuruInvestigationQuestion {
    fn draft(&self) -> GuruInvestigationQuestionDraft {
        GuruInvestigationQuestionDraft {
            question: self.question.clone(),
            guru_principle_ids: self.guru_principle_ids.clone(),
            company_context_anchor_ids: self.company_context_anchor_ids.clone(),
            hypothesis: self.hypothesis.clone(),
            counter_hypothesis: self.counter_hypothesis.clone(),
            evidence_needed: self.evidence_needed.clone(),
            strengthens_if: self.strengthens_if.clone(),
            weakens_if: self.weakens_if.clone(),
            why_material: self.why_material.clone(),
            decision_role: self.decision_role.clone(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GuruInvestigationBrief {
    pub format: String,
    pub brief_hash: String,
    pub research_pack_id: String,
    pub author_key: String,
    pub ticker: String,
    pub company_context_hash: String,
    pub questions: Vec<GuruInvestigationQuestion>,
}

/// Kernel-owned bridge between the philosophy result and ordinary company
/// research.  The bridge deliberately has one central tension, while its
/// `evidence_needed` list is allowed to fan out into independent
/// `ResearchProposal v4` objectives.  It is an internal typed view, not a new
/// provider-facing contract, so adding a research objective does not add a
/// second Guru question or another remote protocol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuruResearchFrame {
    pub research_pack_id: String,
    pub brief_hash: String,
    pub author_key: String,
    pub ticker: String,
    pub question_id: String,
    pub question: String,
    pub guru_principle_ids: Vec<String>,
    pub company_context_anchor_ids: Vec<String>,
    pub hypothesis: String,
    pub counter_hypothesis: String,
    pub evidence_needed: Vec<String>,
    pub strengthens_if: String,
    pub weakens_if: String,
    pub why_material: String,
}

/// Compile one already sealed investigation brief into the frame consumed by
/// the research planner and evidence projector.  The function intentionally
/// does not accept model-authored additions: all fields come from the sealed
/// brief and therefore remain bound to the selected author, ticker, anchors,
/// and principle IDs.
pub fn compile_guru_research_frame(
    brief_value: &Value,
) -> Result<GuruResearchFrame, ContractValueError> {
    validate_value(KRW_GURU_INVESTIGATION_BRIEF_V1, brief_value)?;
    let brief: GuruInvestigationBrief = decode(brief_value, KRW_GURU_INVESTIGATION_BRIEF_V1)?;
    let question = brief
        .questions
        .into_iter()
        .next()
        .ok_or(ContractValueError::Semantic(
            KRW_GURU_INVESTIGATION_BRIEF_V1,
        ))?;
    Ok(GuruResearchFrame {
        research_pack_id: brief.research_pack_id,
        brief_hash: brief.brief_hash,
        author_key: brief.author_key,
        ticker: brief.ticker,
        question_id: question.question_id,
        question: question.question,
        guru_principle_ids: question.guru_principle_ids,
        company_context_anchor_ids: question.company_context_anchor_ids,
        hypothesis: question.hypothesis,
        counter_hypothesis: question.counter_hypothesis,
        evidence_needed: question.evidence_needed,
        strengthens_if: question.strengthens_if,
        weakens_if: question.weakens_if,
        why_material: question.why_material,
    })
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(untagged)]
pub enum GuruQueryContextEnvelope {
    ResearchPack(Box<GuruResearchPack>),
    QueryResult(Box<GuruQueryContextResult>),
}

impl GuruQueryContextEnvelope {
    fn pack(&self) -> &GuruResearchPack {
        match self {
            Self::ResearchPack(pack) => pack,
            Self::QueryResult(result) => &result.research_pack,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct GuruCompanyBriefInput {
    pub question: String,
    pub author_keys: Vec<String>,
    pub ticker: String,
    pub company_name: Option<String>,
    pub company_context: GuruLightCompanyContext,
    pub guru_query_context: GuruQueryContextEnvelope,
    pub investigation_questions: Vec<GuruInvestigationQuestionDraft>,
    pub intent_family: Option<String>,
    pub limit_lens: Option<u8>,
    pub limit_consultation: Option<u8>,
    pub limit_data_needs: Option<u8>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct GuruCompanyBriefResult {
    pub company_brief_context_version: String,
    pub research_status: String,
    pub answerability: GuruAnswerability,
    pub runtime: BTreeMap<String, Value>,
    pub selected_author_keys: Vec<String>,
    pub requires_company_evidence: bool,
    pub company_filing_brief: BTreeMap<String, Value>,
    pub investigation_brief: GuruRequiredNullable<GuruInvestigationBrief>,
    pub next_step: String,
    pub do_not_call: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct GuruInputCorrection {
    pub status: String,
    pub code: String,
    pub message: String,
    pub required_change: String,
    pub invalid_fields: Vec<String>,
    pub allowed_next_tools: Vec<String>,
    pub violations: Option<Vec<BTreeMap<String, Value>>>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GuruEvidenceSource {
    pub object_ids: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct GuruCompanyEvidenceUnit {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub object_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub object_type: Option<String>,
    pub ticker: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub period: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub directness: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence_grade: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metric: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub metric_points: Vec<Value>,
    pub source: GuruEvidenceSource,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct GuruCompanyResearchContext {
    pub format: String,
    pub ticker: String,
    pub brief_hash: String,
    pub question_ids: Vec<String>,
    pub evidence_units: Vec<GuruCompanyEvidenceUnit>,
    pub source_object_ids: Vec<String>,
    pub research_status: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GuruEvidenceAssessment {
    pub question_id: String,
    pub evidence_object_ids: Vec<String>,
    pub verdict: String,
    pub reasoning: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GuruAgentEvidenceAnalysis {
    pub assessments: Vec<GuruEvidenceAssessment>,
    pub overall_judgment: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct GuruEvidenceReviewInput {
    pub question: String,
    pub author_keys: Vec<String>,
    pub ticker: String,
    pub company_name: Option<String>,
    pub company_research_context: GuruCompanyResearchContext,
    pub investigation_brief: GuruInvestigationBrief,
    pub agent_analysis: GuruAgentEvidenceAnalysis,
    pub company_context: Option<GuruLightCompanyContext>,
    pub intent_family: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GuruDecisionQuestion {
    pub question_id: String,
    pub decision_role: String,
    pub question: String,
    pub hypothesis: String,
    pub counter_hypothesis: String,
    pub why_material: String,
    pub change_condition: String,
    pub verdict: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GuruDecisionFrame {
    pub main_tension_question_id: String,
    pub questions: Vec<GuruDecisionQuestion>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GuruEvidenceValidation {
    pub all_questions_assessed: bool,
    pub evidence_is_question_scoped: bool,
    pub evidence_mode: String,
    pub allowed_verdicts: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GuruValidatedEvidenceAnalysis {
    pub format: String,
    pub brief_hash: String,
    pub evidence_mode: String,
    pub research_context_hash: String,
    pub author_key: String,
    pub ticker: String,
    pub agent_analysis: GuruAgentEvidenceAnalysis,
    pub decision_frame: GuruDecisionFrame,
    pub validation: GuruEvidenceValidation,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GuruReviewUsage {
    pub purpose: String,
    pub final_answer: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GuruEvidenceReviewResult {
    pub validated_evidence_analysis: GuruValidatedEvidenceAnalysis,
    pub usage: GuruReviewUsage,
}

fn decode<'de, T>(value: &'de Value, contract_id: &'static str) -> Result<T, ContractValueError>
where
    T: Deserialize<'de>,
{
    T::deserialize(value).map_err(|_| ContractValueError::Shape(contract_id))
}

fn bounded_text(value: &str, minimum: usize, maximum: usize) -> bool {
    let length = value.chars().count();
    (minimum..=maximum).contains(&length) && !value.contains('\0')
}

fn nonempty(value: &str, maximum: usize) -> bool {
    bounded_text(value, 1, maximum) && !value.trim().is_empty()
}

fn valid_author(value: &str) -> bool {
    matches!(
        value,
        "buffett" | "marks" | "ackman" | "flatt" | "terry_smith"
    )
}

fn valid_ticker(value: &str) -> bool {
    let mut bytes = value.bytes();
    matches!(bytes.next(), Some(first) if first.is_ascii_uppercase())
        && value.len() <= 32
        && bytes.all(|byte| {
            byte.is_ascii_uppercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'-')
        })
}

fn valid_identifier(value: &str, allow_colon: bool) -> bool {
    let mut bytes = value.bytes();
    matches!(bytes.next(), Some(first) if first.is_ascii_alphanumeric())
        && value.len() <= 256
        && bytes.all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(byte, b'_' | b'.' | b'-')
                || (allow_colon && byte == b':')
        })
}

fn valid_bare_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn valid_question_id(value: &str) -> bool {
    value.len() == 18
        && value.starts_with("q_")
        && value.as_bytes()[2..]
            .iter()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn unique_strings(values: &[String]) -> bool {
    let mut seen = BTreeSet::new();
    values.iter().all(|value| seen.insert(value.as_str()))
}

fn valid_string_list(
    values: &[String],
    minimum: usize,
    maximum: usize,
    string_maximum: usize,
    unique: bool,
) -> bool {
    (minimum..=maximum).contains(&values.len())
        && values.iter().all(|value| nonempty(value, string_maximum))
        && (!unique || unique_strings(values))
}

fn valid_research_status(value: &str) -> bool {
    matches!(
        value,
        "sufficient_lens"
            | "partial_lens"
            | "ontology_gap"
            | "needs_clarification"
            | "needs_company_evidence"
    )
}

fn validate_bounds(value: &Value) -> Result<(), ContractValueError> {
    let canonical = serde_jcs::to_vec(value).map_err(ContractValueError::Json)?;
    if canonical.len() > 2 * 1024 * 1024 {
        return Err(ContractValueError::Limit("Guru contract canonical bytes"));
    }
    let mut stack = vec![(value, 0_u8)];
    let mut visited = 0_usize;
    while let Some((current, depth)) = stack.pop() {
        visited = visited
            .checked_add(1)
            .ok_or(ContractValueError::Limit("Guru contract items"))?;
        if visited > 32_768 || depth > 20 {
            return Err(ContractValueError::Limit("Guru contract depth/items"));
        }
        match current {
            Value::String(text) if text.len() > 64 * 1024 || text.contains('\0') => {
                return Err(ContractValueError::Limit("Guru contract string bytes"));
            }
            Value::Array(values) => {
                if values.len() > 512 {
                    return Err(ContractValueError::Limit("Guru contract array items"));
                }
                stack.extend(values.iter().map(|value| (value, depth.saturating_add(1))));
            }
            Value::Object(values) => {
                if values.len() > 256
                    || values
                        .keys()
                        .any(|key| key.len() > 512 || key.contains('\0'))
                {
                    return Err(ContractValueError::Limit("Guru contract object properties"));
                }
                stack.extend(
                    values
                        .values()
                        .map(|value| (value, depth.saturating_add(1))),
                );
            }
            _ => {}
        }
    }
    Ok(())
}

fn validate_light_context(context: &GuruLightCompanyContext) -> bool {
    let mut anchor_ids = BTreeSet::new();
    context.format == LIGHT_CONTEXT_FORMAT
        && valid_ticker(&context.ticker)
        && nonempty(&context.company_name, 512)
        && context
            .sector
            .as_deref()
            .is_none_or(|value| nonempty(value, 256))
        && context
            .industry
            .as_deref()
            .is_none_or(|value| nonempty(value, 256))
        && nonempty(&context.business_description, 8_000)
        && valid_string_list(&context.primary_activities, 0, 6, 1_000, false)
        && valid_string_list(&context.products_or_segments, 0, 8, 1_000, false)
        && context
            .revenue_logic
            .as_deref()
            .is_none_or(|value| nonempty(value, 4_000))
        && (1..=16).contains(&context.context_anchors.len())
        && context.context_anchors.iter().all(|anchor| {
            valid_identifier(&anchor.anchor_id, false)
                && anchor_ids.insert(anchor.anchor_id.as_str())
                && matches!(
                    anchor.kind.as_str(),
                    "business_description"
                        | "primary_activity"
                        | "product"
                        | "segment"
                        | "revenue_logic"
                        | "sector"
                )
                && nonempty(&anchor.text, 4_000)
        })
}

fn validate_answerability(answerability: &GuruAnswerability) -> bool {
    matches!(answerability.confidence.as_str(), "low" | "medium" | "high")
        && matches!(
            answerability.source_match_strength.as_str(),
            "none" | "weak" | "related" | "direct"
        )
        && matches!(
            answerability.recommended_answer_mode.as_str(),
            "lens_grounded_answer"
                | "lens_with_company_bridge"
                | "clarify_then_answer"
                | "state_ontology_gap"
        )
        && nonempty(&answerability.reason, 4_000)
}

fn validate_research_pack(pack: &GuruResearchPack) -> bool {
    let authors_valid = (1..=5).contains(&pack.pack_meta.guru_keys.len())
        && unique_strings(&pack.pack_meta.guru_keys)
        && pack
            .pack_meta
            .guru_keys
            .iter()
            .all(|author| valid_author(author));
    let company_status = pack.research_status == "needs_company_evidence";
    pack.format == RESEARCH_PACK_FORMAT
        && valid_research_status(&pack.research_status)
        && valid_identifier(&pack.pack_meta.pack_id, true)
        && authors_valid
        && nonempty(&pack.pack_meta.question, 4_000)
        && pack
            .pack_meta
            .corpus_version
            .as_deref()
            .is_none_or(|value| nonempty(value, 256))
        && validate_answerability(&pack.answerability)
        && pack
            .intent
            .family
            .as_deref()
            .is_none_or(|value| nonempty(value, 128))
        && pack.intent.ticker.as_deref().is_none_or(valid_ticker)
        && company_status == pack.intent.requires_company_evidence
        && pack.persona_profile.len() <= 64
        && pack.philosophy_context.len() <= 128
        && pack.selected_lenses.len() <= 10
        && pack.consultation_moves.len() <= 8
        && pack.data_needs.len() <= 12
        && pack.company_context.len() <= 128
        && pack.source_anchors.len() <= 8
        && valid_string_list(&pack.clarifying_questions, 0, 8, 4_000, false)
        && pack.company_bridge.len() <= 128
        && pack.trace_recommendations.len() <= 16
        && pack.agent_autonomy.len() <= 64
        && valid_string_list(&pack.do_not_call, 0, 16, 4_000, false)
        && valid_string_list(&pack.warnings, 0, 32, 4_000, false)
}

fn validate_draft(draft: &GuruInvestigationQuestionDraft) -> bool {
    nonempty(&draft.question, 4_000)
        && (1..=16).contains(&draft.guru_principle_ids.len())
        && unique_strings(&draft.guru_principle_ids)
        && draft
            .guru_principle_ids
            .iter()
            .all(|value| valid_identifier(value, true))
        && (1..=16).contains(&draft.company_context_anchor_ids.len())
        && unique_strings(&draft.company_context_anchor_ids)
        && draft
            .company_context_anchor_ids
            .iter()
            .all(|value| valid_identifier(value, false))
        && nonempty(&draft.hypothesis, 4_000)
        && nonempty(&draft.counter_hypothesis, 4_000)
        && valid_string_list(&draft.evidence_needed, 1, 16, 1_000, true)
        && nonempty(&draft.strengthens_if, 4_000)
        && nonempty(&draft.weakens_if, 4_000)
        && nonempty(&draft.why_material, 4_000)
        && draft.decision_role == "main_tension"
}

fn validate_draft_linkage(
    pack: &GuruResearchPack,
    context: &GuruLightCompanyContext,
    draft: &GuruInvestigationQuestionDraft,
) -> bool {
    let selected_principles = pack
        .selected_lenses
        .iter()
        .filter_map(|lens| lens.get("reviewed_id").and_then(Value::as_str))
        .collect::<BTreeSet<_>>();
    let trusted_anchors = context
        .context_anchors
        .iter()
        .map(|anchor| anchor.anchor_id.as_str())
        .collect::<BTreeSet<_>>();
    // The sealed brief keeps one central investment tension, but its proof
    // needs are not a three-item report template.  A complex company
    // question may need independent numeric, qualitative, counter-signal,
    // and mechanism evidence.  The schema already bounds this list at 16;
    // keep the linkage check aligned with that contract and let the
    // ResearchProposal compiler choose the minimum sufficient physical plan.
    (1..=16).contains(&draft.evidence_needed.len())
        && !selected_principles.is_empty()
        && draft
            .guru_principle_ids
            .iter()
            .all(|id| selected_principles.contains(id.as_str()))
        && draft
            .company_context_anchor_ids
            .iter()
            .all(|id| trusted_anchors.contains(id.as_str()))
}

fn canonical_bare_hash<T: Serialize>(value: &T) -> Result<String, ContractValueError> {
    let canonical = serde_jcs::to_vec(value).map_err(ContractValueError::Json)?;
    let digest = Sha256::digest(canonical);
    Ok(hex_digest(&digest))
}

#[derive(Serialize)]
struct UnsignedBrief<'a> {
    format: &'a str,
    research_pack_id: &'a str,
    author_key: &'a str,
    ticker: &'a str,
    company_context_hash: &'a str,
    questions: &'a [GuruInvestigationQuestion],
}

fn validate_brief(brief: &GuruInvestigationBrief) -> Result<bool, ContractValueError> {
    let Some(question) = brief.questions.first() else {
        return Ok(false);
    };
    let unsigned = UnsignedBrief {
        format: &brief.format,
        research_pack_id: &brief.research_pack_id,
        author_key: &brief.author_key,
        ticker: &brief.ticker,
        company_context_hash: &brief.company_context_hash,
        questions: &brief.questions,
    };
    Ok(brief.format == INVESTIGATION_BRIEF_FORMAT
        && valid_bare_hash(&brief.brief_hash)
        && valid_identifier(&brief.research_pack_id, true)
        && valid_author(&brief.author_key)
        && valid_ticker(&brief.ticker)
        && valid_bare_hash(&brief.company_context_hash)
        && brief.questions.len() == 1
        && validate_draft(&question.draft())
        && valid_question_id(&question.question_id)
        && canonical_bare_hash(&unsigned)? == brief.brief_hash)
}

fn validate_query_input(input: &GuruQueryContextInput) -> bool {
    nonempty(&input.question, 4_000)
        && input.author_keys.as_ref().is_none_or(|authors| {
            (1..=5).contains(&authors.len())
                && unique_strings(authors)
                && authors.iter().all(|author| valid_author(author))
        })
        && input.ticker.as_deref().is_none_or(valid_ticker)
        && input
            .company_context
            .as_ref()
            .is_none_or(validate_light_context)
        && input
            .company_context
            .as_ref()
            .zip(input.ticker.as_deref())
            .is_none_or(|(context, ticker)| context.ticker == ticker)
        && input
            .intent_family
            .as_deref()
            .is_none_or(|value| nonempty(value, 128))
        && input
            .limit_lens
            .is_none_or(|value| (1..=10).contains(&value))
        && input
            .limit_consultation
            .is_none_or(|value| (1..=8).contains(&value))
        && input
            .limit_data_needs
            .is_none_or(|value| (1..=12).contains(&value))
}

fn validate_query_result(result: &GuruQueryContextResult) -> bool {
    let pack = &result.research_pack;
    result.research_context_version == "krw-guru-query-context/v1"
        && validate_research_pack(pack)
        && result.research_status == pack.research_status
        && result.answerability == pack.answerability
        && result.intent == pack.intent
        && result.selected_author_keys == pack.pack_meta.guru_keys
        && result.selected_authors.len() == result.selected_author_keys.len()
        && result
            .selected_authors
            .iter()
            .zip(&result.selected_author_keys)
            .all(|(author, key)| author.author_key == *key && nonempty(&author.display_name, 256))
        && result.requires_company_evidence == pack.intent.requires_company_evidence
        // The pack keeps the richer ontology projection used for ranking.
        // The result boundary may instead carry the trusted neutral light
        // context used by the sealed company workflow.  Keep accepting the
        // old equal-map shape for replayed vectors, but validate the new
        // light projection and bind it to the pack ticker when present.
        && (result.company_context == pack.company_context
            || serde_json::to_value(&result.company_context)
                .ok()
                .and_then(|value| normalize_light_company_context(&value).ok())
                .is_some_and(|context| {
                    pack.intent
                        .ticker
                        .as_deref()
                        .is_some_and(|ticker| {
                            context.get("ticker").and_then(Value::as_str) == Some(ticker)
                        })
                }))
        && result.agent_autonomy == pack.agent_autonomy
        && result.do_not_call == pack.do_not_call
        && result.runtime.len() <= 64
        && valid_string_list(&result.filing_evidence_requirements, 0, 64, 1_000, true)
        && result.usage.len() <= 16
}

fn validate_company_brief_input(input: &GuruCompanyBriefInput) -> bool {
    let pack = input.guru_query_context.pack();
    nonempty(&input.question, 4_000)
        && (1..=5).contains(&input.author_keys.len())
        && unique_strings(&input.author_keys)
        && input.author_keys.iter().all(|author| valid_author(author))
        && valid_ticker(&input.ticker)
        && input
            .company_name
            .as_deref()
            .is_none_or(|value| nonempty(value, 512))
        && validate_light_context(&input.company_context)
        && input.company_context.ticker == input.ticker
        && validate_research_pack(pack)
        && input.author_keys == pack.pack_meta.guru_keys
        && pack
            .intent
            .ticker
            .as_deref()
            .is_none_or(|ticker| ticker == input.ticker)
        && input.question == pack.pack_meta.question
        && input.investigation_questions.len() == 1
        && input.investigation_questions.iter().all(validate_draft)
        && input
            .investigation_questions
            .iter()
            .all(|draft| validate_draft_linkage(pack, &input.company_context, draft))
        && input
            .intent_family
            .as_deref()
            .is_none_or(|value| nonempty(value, 128))
        && input
            .limit_lens
            .is_none_or(|value| (1..=10).contains(&value))
        && input
            .limit_consultation
            .is_none_or(|value| (1..=8).contains(&value))
        && input
            .limit_data_needs
            .is_none_or(|value| (1..=12).contains(&value))
}

fn validate_company_brief_result(
    result: &GuruCompanyBriefResult,
) -> Result<bool, ContractValueError> {
    let nested = result.company_filing_brief.get("investigation_brief");
    let top = result
        .investigation_brief
        .0
        .as_ref()
        .map(serde_json::to_value)
        .transpose()
        .map_err(ContractValueError::Json)?;
    let nested_matches = match (&top, nested) {
        (Some(top), Some(nested)) => top == nested,
        (None, None | Some(Value::Null)) => true,
        _ => false,
    };
    let brief_valid = result
        .investigation_brief
        .0
        .as_ref()
        .map(validate_brief)
        .transpose()?
        .unwrap_or(true);
    Ok(
        result.company_brief_context_version == "krw-guru-company-brief-context/v1"
            && valid_research_status(&result.research_status)
            && validate_answerability(&result.answerability)
            && result.runtime.len() <= 64
            && (1..=5).contains(&result.selected_author_keys.len())
            && unique_strings(&result.selected_author_keys)
            && result
                .selected_author_keys
                .iter()
                .all(|author| valid_author(author))
            && result.company_filing_brief.len() <= 128
            && brief_valid
            && nested_matches
            && nonempty(&result.next_step, 1_000)
            && valid_string_list(&result.do_not_call, 0, 8, 4_000, false),
    )
}

fn brief_correction_code(code: &str) -> bool {
    matches!(
        code,
        "exactly_one_key_question_required" | "invalid_key_question"
    )
}

fn review_correction_code(code: &str) -> bool {
    matches!(
        code,
        "invalid_agent_analysis"
            | "invalid_company_research_context"
            | "missing_agent_analysis"
            | "missing_company_research_context"
            | "missing_investigation_brief"
            | "question_scoped_evidence_required"
            | "sealed_question_context_required"
            | "sealed_question_coverage_required"
            | "contextual_evidence_cannot_support"
            | "unresolved_verdict_must_not_cite_evidence"
    )
}

fn validate_correction(correction: &GuruInputCorrection) -> bool {
    let expected_tool = if brief_correction_code(&correction.code) {
        "krw_guru_company_brief"
    } else if review_correction_code(&correction.code) {
        "krw_guru_review_company_evidence"
    } else {
        return false;
    };
    correction.status == "input_correction_required"
        && nonempty(&correction.message, 8_000)
        && nonempty(&correction.required_change, 8_000)
        && valid_string_list(&correction.invalid_fields, 1, 32, 512, false)
        && correction.allowed_next_tools.len() == 1
        && correction.allowed_next_tools[0] == expected_tool
        && correction
            .violations
            .as_ref()
            .is_none_or(|violations| violations.len() <= 32)
}

fn validate_research_context(context: &GuruCompanyResearchContext) -> bool {
    if context.format != COMPANY_RESEARCH_CONTEXT_FORMAT
        || !valid_ticker(&context.ticker)
        || !valid_bare_hash(&context.brief_hash)
        || context.question_ids.len() != 1
        || !unique_strings(&context.question_ids)
        || !context.question_ids.iter().all(|id| valid_question_id(id))
        || !(1..=12).contains(&context.evidence_units.len())
        || !(1..=24).contains(&context.source_object_ids.len())
        || !unique_strings(&context.source_object_ids)
        || !context
            .source_object_ids
            .iter()
            .all(|id| valid_identifier(id, true))
    {
        return false;
    }
    let permitted = context
        .source_object_ids
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let mut observed = BTreeSet::new();
    let mut evidence_ids = BTreeSet::new();
    for unit in &context.evidence_units {
        if unit.ticker != context.ticker
            || unit
                .evidence_id
                .as_deref()
                .is_some_and(|id| !valid_identifier(id, true) || !evidence_ids.insert(id))
            || unit
                .object_id
                .as_deref()
                .is_some_and(|id| !valid_identifier(id, true))
            || unit
                .object_type
                .as_deref()
                .is_some_and(|value| !nonempty(value, 256))
            || !(1..=24).contains(&unit.source.object_ids.len())
            || !unique_strings(&unit.source.object_ids)
            || unit
                .source
                .object_ids
                .iter()
                .any(|id| !permitted.contains(id.as_str()))
        {
            return false;
        }
        observed.extend(unit.source.object_ids.iter().map(String::as_str));
    }
    observed == permitted
        && match context.research_status.as_str() {
            "evidence_found" => context.evidence_units.len() >= 3,
            "partial" => context.evidence_units.len() < 3,
            _ => false,
        }
}

fn validate_analysis(analysis: &GuruAgentEvidenceAnalysis) -> bool {
    analysis.assessments.len() == 1
        && nonempty(&analysis.overall_judgment, 8_000)
        && analysis.assessments.iter().all(|assessment| {
            valid_question_id(&assessment.question_id)
                && assessment.evidence_object_ids.len() <= 24
                && assessment
                    .evidence_object_ids
                    .iter()
                    .all(|id| valid_identifier(id, true))
                && matches!(assessment.verdict.as_str(), "mixed" | "unresolved")
                && (assessment.verdict != "unresolved" || assessment.evidence_object_ids.is_empty())
                && nonempty(&assessment.reasoning, 4_000)
        })
}

fn validate_review_linkage(
    brief: &GuruInvestigationBrief,
    context: &GuruCompanyResearchContext,
    analysis: &GuruAgentEvidenceAnalysis,
) -> Result<bool, ContractValueError> {
    if !validate_brief(brief)?
        || !validate_research_context(context)
        || !validate_analysis(analysis)
    {
        return Ok(false);
    }
    let question_id = &brief.questions[0].question_id;
    let assessment = &analysis.assessments[0];
    let permitted = context
        .source_object_ids
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    Ok(context.brief_hash == brief.brief_hash
        && context.ticker == brief.ticker
        && context.question_ids[0] == *question_id
        && assessment.question_id == *question_id
        && assessment
            .evidence_object_ids
            .iter()
            .all(|id| permitted.contains(id.as_str())))
}

fn validate_review_input(input: &GuruEvidenceReviewInput) -> Result<bool, ContractValueError> {
    Ok(nonempty(&input.question, 4_000)
        && input.author_keys.len() == 1
        && input.author_keys[0] == input.investigation_brief.author_key
        && input.ticker == input.investigation_brief.ticker
        && input
            .company_name
            .as_deref()
            .is_none_or(|value| nonempty(value, 512))
        && input.company_context.as_ref().is_none_or(|context| {
            validate_light_context(context) && context.ticker == input.ticker
        })
        && input
            .intent_family
            .as_deref()
            .is_none_or(|value| nonempty(value, 128))
        && validate_review_linkage(
            &input.investigation_brief,
            &input.company_research_context,
            &input.agent_analysis,
        )?)
}

fn validate_validated_analysis(result: &GuruValidatedEvidenceAnalysis) -> bool {
    let Some(assessment) = result.agent_analysis.assessments.first() else {
        return false;
    };
    let Some(decision) = result.decision_frame.questions.first() else {
        return false;
    };
    result.format == VALIDATED_EVIDENCE_FORMAT
        && valid_bare_hash(&result.brief_hash)
        && result.evidence_mode == "contextual"
        && valid_bare_hash(&result.research_context_hash)
        && valid_author(&result.author_key)
        && valid_ticker(&result.ticker)
        && validate_analysis(&result.agent_analysis)
        && result.decision_frame.questions.len() == 1
        && result.decision_frame.main_tension_question_id == assessment.question_id
        && decision.question_id == assessment.question_id
        && decision.decision_role == "main_tension"
        && decision.verdict == assessment.verdict
        && nonempty(&decision.question, 4_000)
        && nonempty(&decision.hypothesis, 4_000)
        && nonempty(&decision.counter_hypothesis, 4_000)
        && nonempty(&decision.why_material, 4_000)
        && nonempty(&decision.change_condition, 4_000)
        && result.validation.all_questions_assessed
        && result.validation.evidence_is_question_scoped
        && result.validation.evidence_mode == "contextual"
        && result.validation.allowed_verdicts.len() == 2
        && result.validation.allowed_verdicts[0] == "mixed"
        && result.validation.allowed_verdicts[1] == "unresolved"
}

fn validate_review_result(result: &GuruEvidenceReviewResult) -> bool {
    validate_validated_analysis(&result.validated_evidence_analysis)
        && result.usage.purpose == "validate_main_guru_analysis_only"
        && result.usage.final_answer
            == "The main Guru writes the final consultation from its validated analysis."
}

/// Validate one Guru contract with closed, allocation-bounded Rust checks.
pub(super) fn validate_value(contract_id: &str, value: &Value) -> Result<(), ContractValueError> {
    validate_bounds(value)?;
    let valid = match contract_id {
        KRW_GURU_QUERY_CONTEXT_INPUT_V1 => {
            validate_query_input(&decode(value, KRW_GURU_QUERY_CONTEXT_INPUT_V1)?)
        }
        KRW_GURU_QUERY_CONTEXT_RESULT_V1 => {
            validate_query_result(&decode(value, KRW_GURU_QUERY_CONTEXT_RESULT_V1)?)
        }
        KRW_GURU_RESEARCH_PACK_V1 => {
            validate_research_pack(&decode(value, KRW_GURU_RESEARCH_PACK_V1)?)
        }
        KRW_GURU_LIGHT_COMPANY_CONTEXT_V1 => {
            validate_light_context(&decode(value, KRW_GURU_LIGHT_COMPANY_CONTEXT_V1)?)
        }
        KRW_GURU_INVESTIGATION_QUESTION_DRAFT_V1 => {
            validate_draft(&decode(value, KRW_GURU_INVESTIGATION_QUESTION_DRAFT_V1)?)
        }
        KRW_GURU_COMPANY_BRIEF_INPUT_V1 => {
            validate_company_brief_input(&decode(value, KRW_GURU_COMPANY_BRIEF_INPUT_V1)?)
        }
        KRW_GURU_INVESTIGATION_BRIEF_V1 => {
            validate_brief(&decode(value, KRW_GURU_INVESTIGATION_BRIEF_V1)?)?
        }
        KRW_GURU_COMPANY_BRIEF_RESULT_V1 => {
            validate_company_brief_result(&decode(value, KRW_GURU_COMPANY_BRIEF_RESULT_V1)?)?
        }
        KRW_GURU_INPUT_CORRECTION_V1 => {
            validate_correction(&decode(value, KRW_GURU_INPUT_CORRECTION_V1)?)
        }
        KRW_GURU_COMPANY_RESEARCH_CONTEXT_V1 => {
            validate_research_context(&decode(value, KRW_GURU_COMPANY_RESEARCH_CONTEXT_V1)?)
        }
        KRW_GURU_AGENT_EVIDENCE_ANALYSIS_V1 => {
            validate_analysis(&decode(value, KRW_GURU_AGENT_EVIDENCE_ANALYSIS_V1)?)
        }
        KRW_GURU_EVIDENCE_REVIEW_INPUT_V1 => {
            validate_review_input(&decode(value, KRW_GURU_EVIDENCE_REVIEW_INPUT_V1)?)?
        }
        KRW_GURU_VALIDATED_EVIDENCE_ANALYSIS_V1 => {
            validate_validated_analysis(&decode(value, KRW_GURU_VALIDATED_EVIDENCE_ANALYSIS_V1)?)
        }
        KRW_GURU_EVIDENCE_REVIEW_RESULT_V1 => {
            validate_review_result(&decode(value, KRW_GURU_EVIDENCE_REVIEW_RESULT_V1)?)
        }
        _ => return Err(ContractValueError::UnknownContract(contract_id.to_owned())),
    };
    if valid {
        Ok(())
    } else {
        Err(ContractValueError::Semantic(contract_static_id(
            contract_id,
        )))
    }
}

fn contract_static_id(contract_id: &str) -> &'static str {
    contract(contract_id).map_or("krw-guru/unknown", |descriptor| descriptor.id)
}

/// Content identity used to pin one immutable `ResearchPack` for a run.
pub fn research_pack_identity(value: &Value) -> Result<String, ContractValueError> {
    validate_value(KRW_GURU_RESEARCH_PACK_V1, value)?;
    let canonical = serde_jcs::to_vec(value).map_err(ContractValueError::Json)?;
    Ok(format!("sha256:{}", hex_digest(&Sha256::digest(canonical))))
}

/// Validate the one model-authored question draft against only IDs returned by
/// the committed `ResearchPack` and trusted light-company context.
pub fn validate_investigation_draft_linkage(
    pack_value: &Value,
    context_value: &Value,
    draft_value: &Value,
) -> Result<(), ContractValueError> {
    validate_value(KRW_GURU_RESEARCH_PACK_V1, pack_value)?;
    validate_value(KRW_GURU_LIGHT_COMPANY_CONTEXT_V1, context_value)?;
    validate_value(KRW_GURU_INVESTIGATION_QUESTION_DRAFT_V1, draft_value)?;
    let pack: GuruResearchPack = decode(pack_value, KRW_GURU_RESEARCH_PACK_V1)?;
    let context: GuruLightCompanyContext =
        decode(context_value, KRW_GURU_LIGHT_COMPANY_CONTEXT_V1)?;
    let draft: GuruInvestigationQuestionDraft =
        decode(draft_value, KRW_GURU_INVESTIGATION_QUESTION_DRAFT_V1)?;
    if validate_draft_linkage(&pack, &context, &draft) {
        Ok(())
    } else {
        Err(ContractValueError::Semantic(
            KRW_GURU_INVESTIGATION_QUESTION_DRAFT_V1,
        ))
    }
}

/// Deterministically build the physical brief-sealing request. The model owns
/// only the single question draft; fixed identity, trusted company context,
/// limits, and the committed `ResearchPack` are injected by the kernel.
pub fn build_company_brief_input(
    query_input_value: &Value,
    research_pack_value: &Value,
    draft_value: &Value,
) -> Result<Value, ContractValueError> {
    validate_value(KRW_GURU_QUERY_CONTEXT_INPUT_V1, query_input_value)?;
    validate_value(KRW_GURU_RESEARCH_PACK_V1, research_pack_value)?;
    validate_value(KRW_GURU_INVESTIGATION_QUESTION_DRAFT_V1, draft_value)?;
    let query: GuruQueryContextInput = decode(query_input_value, KRW_GURU_QUERY_CONTEXT_INPUT_V1)?;
    let authors = query
        .author_keys
        .as_ref()
        .filter(|authors| authors.len() == 1);
    let ticker = query.ticker.as_deref();
    let context = query.company_context.as_ref();
    let (Some(authors), Some(ticker), Some(context)) = (authors, ticker, context) else {
        return Err(ContractValueError::Semantic(
            KRW_GURU_COMPANY_BRIEF_INPUT_V1,
        ));
    };
    validate_investigation_draft_linkage(
        research_pack_value,
        &serde_json::to_value(context).map_err(ContractValueError::Json)?,
        draft_value,
    )?;
    let pack: GuruResearchPack = decode(research_pack_value, KRW_GURU_RESEARCH_PACK_V1)?;
    if pack.pack_meta.guru_keys != *authors
        || pack.pack_meta.question != query.question
        || pack.intent.ticker.as_deref() != Some(ticker)
        || context.ticker != ticker
    {
        return Err(ContractValueError::Semantic(
            KRW_GURU_COMPANY_BRIEF_INPUT_V1,
        ));
    }
    let mut input = serde_json::Map::from_iter([
        ("question".into(), Value::String(query.question)),
        (
            "author_keys".into(),
            serde_json::to_value(authors).map_err(ContractValueError::Json)?,
        ),
        ("ticker".into(), Value::String(ticker.to_owned())),
        (
            "company_name".into(),
            Value::String(context.company_name.clone()),
        ),
        (
            "company_context".into(),
            query_input_value
                .get("company_context")
                .cloned()
                .ok_or(ContractValueError::Shape(KRW_GURU_COMPANY_BRIEF_INPUT_V1))?,
        ),
        ("guru_query_context".into(), research_pack_value.clone()),
        (
            "investigation_questions".into(),
            Value::Array(vec![draft_value.clone()]),
        ),
    ]);
    for (key, value) in [
        ("intent_family", query.intent_family.map(Value::String)),
        (
            "limit_lens",
            query.limit_lens.map(|value| Value::from(u64::from(value))),
        ),
        (
            "limit_consultation",
            query
                .limit_consultation
                .map(|value| Value::from(u64::from(value))),
        ),
        (
            "limit_data_needs",
            query
                .limit_data_needs
                .map(|value| Value::from(u64::from(value))),
        ),
    ] {
        if let Some(value) = value {
            input.insert(key.into(), value);
        }
    }
    let input = Value::Object(input);
    validate_value(KRW_GURU_COMPANY_BRIEF_INPUT_V1, &input)?;
    Ok(input)
}

/// Deterministically build the physical evidence-review request. The model
/// supplies only its typed analysis; every identity and private artifact comes
/// from committed run state.
pub fn build_evidence_review_input(
    query_input_value: &Value,
    brief_value: &Value,
    context_value: &Value,
    analysis_value: &Value,
) -> Result<Value, ContractValueError> {
    validate_value(KRW_GURU_QUERY_CONTEXT_INPUT_V1, query_input_value)?;
    validate_value(KRW_GURU_INVESTIGATION_BRIEF_V1, brief_value)?;
    validate_value(KRW_GURU_COMPANY_RESEARCH_CONTEXT_V1, context_value)?;
    validate_value(KRW_GURU_AGENT_EVIDENCE_ANALYSIS_V1, analysis_value)?;
    validate_review_linkage_values(brief_value, context_value, analysis_value)?;
    let query: GuruQueryContextInput = decode(query_input_value, KRW_GURU_QUERY_CONTEXT_INPUT_V1)?;
    let authors = query
        .author_keys
        .as_ref()
        .filter(|authors| authors.len() == 1);
    let ticker = query.ticker.as_deref();
    let company_context = query.company_context.as_ref();
    let (Some(authors), Some(ticker), Some(company_context)) = (authors, ticker, company_context)
    else {
        return Err(ContractValueError::Semantic(
            KRW_GURU_EVIDENCE_REVIEW_INPUT_V1,
        ));
    };
    let mut input = serde_json::Map::from_iter([
        ("question".into(), Value::String(query.question)),
        (
            "author_keys".into(),
            serde_json::to_value(authors).map_err(ContractValueError::Json)?,
        ),
        ("ticker".into(), Value::String(ticker.to_owned())),
        (
            "company_name".into(),
            Value::String(company_context.company_name.clone()),
        ),
        (
            "company_context".into(),
            query_input_value
                .get("company_context")
                .cloned()
                .ok_or(ContractValueError::Shape(KRW_GURU_EVIDENCE_REVIEW_INPUT_V1))?,
        ),
        ("investigation_brief".into(), brief_value.clone()),
        ("company_research_context".into(), context_value.clone()),
        ("agent_analysis".into(), analysis_value.clone()),
    ]);
    if let Some(intent) = query.intent_family {
        input.insert("intent_family".into(), Value::String(intent));
    }
    let input = Value::Object(input);
    validate_evidence_review_input_origin(query_input_value, &input)?;
    Ok(input)
}

/// Normalize the model-authored analysis before it crosses the physical
/// evidence-review boundary.
///
/// The model is responsible for the reasoning prose, but it is not a reliable
/// owner of opaque question/object identifiers.  There is exactly one sealed
/// question and one trusted evidence-object set, so the kernel can bind those
/// fields deterministically without inventing evidence or changing the
/// conclusion's evidentiary meaning.  Unknown verdicts are conservatively
/// treated as unresolved; a mixed verdict is retained only when at least one
/// trusted evidence object remains linked.
pub fn normalize_guru_agent_evidence_analysis(
    brief_value: &Value,
    context_value: &Value,
    analysis_value: &Value,
) -> Result<Value, ContractValueError> {
    validate_value(KRW_GURU_INVESTIGATION_BRIEF_V1, brief_value)?;
    validate_value(KRW_GURU_COMPANY_RESEARCH_CONTEXT_V1, context_value)?;
    let brief: GuruInvestigationBrief = decode(brief_value, KRW_GURU_INVESTIGATION_BRIEF_V1)?;
    let context: GuruCompanyResearchContext =
        decode(context_value, KRW_GURU_COMPANY_RESEARCH_CONTEXT_V1)?;
    let question = brief.questions.first().ok_or(ContractValueError::Semantic(
        KRW_GURU_INVESTIGATION_BRIEF_V1,
    ))?;
    let permitted = context
        .source_object_ids
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let supplied = analysis_value
        .as_object()
        .and_then(|object| object.get("assessments"))
        .and_then(Value::as_array)
        .and_then(|assessments| assessments.first())
        .and_then(Value::as_object);

    let reasoning = supplied
        .and_then(|assessment| assessment.get("reasoning"))
        .and_then(Value::as_str)
        .filter(|value| nonempty(value, 4_000))
        .map_or_else(
            || {
                "The model analysis could not be safely linked to the sealed evidence context."
                    .to_owned()
            },
            str::to_owned,
        );
    let overall_judgment = analysis_value
        .as_object()
        .and_then(|object| object.get("overall_judgment"))
        .and_then(Value::as_str)
        .filter(|value| nonempty(value, 8_000))
        .map_or_else(
            || "The available evidence is insufficient for a safely linked judgment.".to_owned(),
            str::to_owned,
        );

    let evidence_object_ids = supplied
        .and_then(|assessment| assessment.get("evidence_object_ids"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|id| permitted.contains(id))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let raw_verdict = supplied
        .and_then(|assessment| assessment.get("verdict"))
        .and_then(Value::as_str)
        .unwrap_or("unresolved");
    let verdict = match raw_verdict {
        "mixed" | "supported" | "partially_supported" if !evidence_object_ids.is_empty() => "mixed",
        _ => "unresolved",
    };

    Ok(serde_json::json!({
        "assessments": [{
            "question_id": question.question_id.clone(),
            "evidence_object_ids": if verdict == "unresolved" {
                Vec::<String>::new()
            } else {
                evidence_object_ids
            },
            "verdict": verdict,
            "reasoning": reasoning,
        }],
        "overall_judgment": overall_judgment,
    }))
}

/// Enforce the trusted boundary from one sealed Guru question into a filing
/// `SearchPlan`. The plan may contain the full bounded set of physical clauses
/// selected by the `ResearchProposal` compiler; the model is not forced to
/// compress several independent proof needs into a fixed small clause count. The
/// sealed question and ticker remain exact, while evidence coverage is judged
/// from the observed `ResearchState` rather than by brittle text equality between
/// Korean proof descriptions and model-authored retrieval concepts.
pub fn validate_guru_company_search_plan(
    brief_value: &Value,
    plan_value: &Value,
) -> Result<(), ContractValueError> {
    let frame = compile_guru_research_frame(brief_value)?;
    super::validate_value(SEARCH_PLAN_V2, plan_value)?;
    let plan = plan_value
        .as_object()
        .ok_or(ContractValueError::Shape(SEARCH_PLAN_V2))?;
    let clauses = plan
        .get("clauses")
        .and_then(Value::as_array)
        .filter(|clauses| (1..=12).contains(&clauses.len()))
        .ok_or(ContractValueError::Semantic(SEARCH_PLAN_V2))?;
    let exact_ticker_array = |value: Option<&Value>| {
        value.and_then(Value::as_array).is_some_and(|tickers| {
            tickers.len() == 1 && tickers[0].as_str() == Some(frame.ticker.as_str())
        })
    };
    if plan.get("question").and_then(Value::as_str) != Some(frame.question.as_str())
        || !exact_ticker_array(plan.get("tickers"))
        || plan.get("universe").is_some_and(|value| !value.is_null())
    {
        return Err(ContractValueError::Semantic(SEARCH_PLAN_V2));
    }
    for clause in clauses {
        let clause = clause
            .as_object()
            .ok_or(ContractValueError::Shape(SEARCH_PLAN_V2))?;
        if clause.get("required").and_then(Value::as_bool) != Some(true)
            || !exact_ticker_array(clause.get("tickers"))
        {
            return Err(ContractValueError::Semantic(SEARCH_PLAN_V2));
        }
    }
    Ok(())
}

/// Normalize the authoritative TypeScript host's camel-case light context to
/// the Python Guru server's canonical snake-case model.  Missing nullable
/// fields become explicit `null`, matching the Pydantic `model_dump` used in
/// the server's `company_context_hash`.
pub fn normalize_light_company_context(value: &Value) -> Result<Value, ContractValueError> {
    validate_bounds(value)?;
    let canonical = GuruLightCompanyContext::deserialize(value)
        .or_else(|_| FrontGuruLightCompanyContext::deserialize(value).map(Into::into))
        .map_err(|_| ContractValueError::Shape(KRW_GURU_LIGHT_COMPANY_CONTEXT_V1))?;
    if !validate_light_context(&canonical) {
        return Err(ContractValueError::Semantic(
            KRW_GURU_LIGHT_COMPANY_CONTEXT_V1,
        ));
    }
    serde_json::to_value(canonical).map_err(ContractValueError::Json)
}

/// Construct the private, downstream form of a committed Guru retrieval
/// request. The physical first call intentionally contains no company context:
/// that neutral orientation is produced by the trusted Guru runtime and is
/// accepted only after the retrieval exchange has been validated. Downstream
/// brief/review builders consume this enriched value so the model never owns
/// the company identity or context anchors.
pub fn enrich_guru_query_input_with_result_context(
    query_input_value: &Value,
    query_result_value: &Value,
) -> Result<Value, ContractValueError> {
    validate_guru_query_exchange(query_input_value, query_result_value)?;
    let result: GuruQueryContextResult =
        decode(query_result_value, KRW_GURU_QUERY_CONTEXT_RESULT_V1)?;
    let context = normalized_query_result_context(&result)?;
    let mut enriched = query_input_value
        .as_object()
        .cloned()
        .ok_or(ContractValueError::Shape(KRW_GURU_QUERY_CONTEXT_INPUT_V1))?;
    enriched.insert("company_context".into(), context);
    let enriched = Value::Object(enriched);
    validate_value(KRW_GURU_QUERY_CONTEXT_INPUT_V1, &enriched)?;
    Ok(enriched)
}

fn normalized_query_result_context(
    result: &GuruQueryContextResult,
) -> Result<Value, ContractValueError> {
    let raw = serde_json::to_value(&result.company_context).map_err(ContractValueError::Json)?;
    // Only the trusted Guru runtime's neutral light company context is a
    // valid result boundary. A bare ticker or empty object is the legacy
    // replay shape and is rejected outright.
    normalize_light_company_context(&raw)
}

/// Bind the company-scoped Guru retrieval result to the exact request that
/// selected it. The default `AgentSpec` always supplies one fixed author and
/// immutable ticker; the trusted Guru runtime attaches neutral light company
/// context to the result. A broad multi-author or company-free result is
/// therefore not a valid result for this path even though the underlying
/// public MCP contract supports those modes.
pub fn validate_guru_query_exchange(
    input_value: &Value,
    result_value: &Value,
) -> Result<(), ContractValueError> {
    validate_value(KRW_GURU_QUERY_CONTEXT_INPUT_V1, input_value)?;
    validate_value(KRW_GURU_QUERY_CONTEXT_RESULT_V1, result_value)?;
    let input: GuruQueryContextInput = decode(input_value, KRW_GURU_QUERY_CONTEXT_INPUT_V1)?;
    let result: GuruQueryContextResult = decode(result_value, KRW_GURU_QUERY_CONTEXT_RESULT_V1)?;
    let Some(authors) = input.author_keys.as_ref() else {
        return Err(ContractValueError::Semantic(
            KRW_GURU_QUERY_CONTEXT_RESULT_V1,
        ));
    };
    let Some(ticker) = input.ticker.as_deref() else {
        return Err(ContractValueError::Semantic(
            KRW_GURU_QUERY_CONTEXT_RESULT_V1,
        ));
    };
    let context_value = normalized_query_result_context(&result)?;
    let context: GuruLightCompanyContext =
        decode(&context_value, KRW_GURU_LIGHT_COMPANY_CONTEXT_V1)?;
    let input_context_matches = match input.company_context.as_ref() {
        None => true,
        Some(input_context) => {
            serde_json::to_value(input_context).map_err(ContractValueError::Json)? == context_value
        }
    };
    let pack = &result.research_pack;
    let valid = authors.len() == 1
        && result.selected_author_keys == *authors
        && pack.pack_meta.guru_keys == *authors
        && pack.pack_meta.question == input.question
        && pack.intent.requires_company_evidence
        && result.requires_company_evidence
        && pack.intent.ticker.as_deref() == Some(ticker)
        && context.ticker == ticker
        && input_context_matches;
    if valid {
        Ok(())
    } else {
        Err(ContractValueError::Semantic(
            KRW_GURU_QUERY_CONTEXT_RESULT_V1,
        ))
    }
}

/// Bind a company-brief request to the exact committed retrieval exchange.
/// The immutable `ResearchPack` may be supplied either directly or inside the
/// exact query result, matching the authoritative Python union contract.
pub fn validate_company_brief_input_linkage(
    query_input_value: &Value,
    query_result_value: &Value,
    brief_input_value: &Value,
) -> Result<(), ContractValueError> {
    validate_value(KRW_GURU_COMPANY_BRIEF_INPUT_V1, brief_input_value)?;
    let enriched_query_input =
        enrich_guru_query_input_with_result_context(query_input_value, query_result_value)?;
    let query_input: GuruQueryContextInput =
        decode(&enriched_query_input, KRW_GURU_QUERY_CONTEXT_INPUT_V1)?;
    let query_result: GuruQueryContextResult =
        decode(query_result_value, KRW_GURU_QUERY_CONTEXT_RESULT_V1)?;
    let brief_input: GuruCompanyBriefInput =
        decode(brief_input_value, KRW_GURU_COMPANY_BRIEF_INPUT_V1)?;
    let exact_pack = brief_input.guru_query_context.pack() == &query_result.research_pack;
    let exact_envelope = match &brief_input.guru_query_context {
        GuruQueryContextEnvelope::ResearchPack(_) => true,
        GuruQueryContextEnvelope::QueryResult(result) => result.as_ref() == &query_result,
    };
    let valid = exact_pack
        && exact_envelope
        && Some(&brief_input.author_keys) == query_input.author_keys.as_ref()
        && Some(brief_input.ticker.as_str()) == query_input.ticker.as_deref()
        && Some(&brief_input.company_context) == query_input.company_context.as_ref()
        && brief_input.company_name.as_deref().is_none_or(|name| {
            query_input
                .company_context
                .as_ref()
                .is_some_and(|context| context.company_name == name)
        })
        && brief_input.question == query_input.question
        && brief_input.intent_family == query_input.intent_family
        && brief_input.limit_lens == query_input.limit_lens
        && brief_input.limit_consultation == query_input.limit_consultation
        && brief_input.limit_data_needs == query_input.limit_data_needs;
    if valid {
        Ok(())
    } else {
        Err(ContractValueError::Semantic(
            KRW_GURU_COMPANY_BRIEF_INPUT_V1,
        ))
    }
}

/// Return the immutable `ResearchPack` identity carried by one validated brief
/// request without retaining the potentially much larger query result.
pub fn company_brief_research_pack_identity(
    brief_input_value: &Value,
) -> Result<String, ContractValueError> {
    validate_value(KRW_GURU_COMPANY_BRIEF_INPUT_V1, brief_input_value)?;
    let brief_input: GuruCompanyBriefInput =
        decode(brief_input_value, KRW_GURU_COMPANY_BRIEF_INPUT_V1)?;
    let pack = serde_json::to_value(brief_input.guru_query_context.pack())
        .map_err(ContractValueError::Json)?;
    research_pack_identity(&pack)
}

/// Memory-bounded linkage used by the live adapter after it has compacted the
/// committed query result to `(query input, ResearchPack hash)`.
pub fn validate_company_brief_input_identity(
    query_input_value: &Value,
    committed_pack_identity: &str,
    brief_input_value: &Value,
) -> Result<(), ContractValueError> {
    validate_value(KRW_GURU_QUERY_CONTEXT_INPUT_V1, query_input_value)?;
    validate_value(KRW_GURU_COMPANY_BRIEF_INPUT_V1, brief_input_value)?;
    let query_input: GuruQueryContextInput =
        decode(query_input_value, KRW_GURU_QUERY_CONTEXT_INPUT_V1)?;
    let brief_input: GuruCompanyBriefInput =
        decode(brief_input_value, KRW_GURU_COMPANY_BRIEF_INPUT_V1)?;
    let observed_identity = company_brief_research_pack_identity(brief_input_value)?;
    let valid = observed_identity == committed_pack_identity
        && Some(&brief_input.author_keys) == query_input.author_keys.as_ref()
        && Some(brief_input.ticker.as_str()) == query_input.ticker.as_deref()
        && Some(&brief_input.company_context) == query_input.company_context.as_ref()
        && brief_input.company_name.as_deref().is_none_or(|name| {
            query_input
                .company_context
                .as_ref()
                .is_some_and(|context| context.company_name == name)
        })
        && brief_input.question == query_input.question
        && brief_input.intent_family == query_input.intent_family
        && brief_input.limit_lens == query_input.limit_lens
        && brief_input.limit_consultation == query_input.limit_consultation
        && brief_input.limit_data_needs == query_input.limit_data_needs;
    if valid {
        Ok(())
    } else {
        Err(ContractValueError::Semantic(
            KRW_GURU_COMPANY_BRIEF_INPUT_V1,
        ))
    }
}

/// Validate a successful default-path seal using only its exact brief request.
pub fn validate_company_brief_result_linkage(
    brief_input_value: &Value,
    brief_result_value: &Value,
) -> Result<(), ContractValueError> {
    validate_value(KRW_GURU_COMPANY_BRIEF_INPUT_V1, brief_input_value)?;
    validate_value(KRW_GURU_COMPANY_BRIEF_RESULT_V1, brief_result_value)?;
    let brief_input: GuruCompanyBriefInput =
        decode(brief_input_value, KRW_GURU_COMPANY_BRIEF_INPUT_V1)?;
    let brief_result: GuruCompanyBriefResult =
        decode(brief_result_value, KRW_GURU_COMPANY_BRIEF_RESULT_V1)?;
    let Some(brief) = brief_result.investigation_brief.0.as_ref() else {
        return Err(ContractValueError::Semantic(
            KRW_GURU_COMPANY_BRIEF_RESULT_V1,
        ));
    };
    let brief_value = serde_json::to_value(brief).map_err(ContractValueError::Json)?;
    let pack_value = serde_json::to_value(brief_input.guru_query_context.pack())
        .map_err(ContractValueError::Json)?;
    let context_value =
        serde_json::to_value(&brief_input.company_context).map_err(ContractValueError::Json)?;
    validate_sealed_brief_linkage(&pack_value, &context_value, &brief_value)?;
    let pack = brief_input.guru_query_context.pack();
    let valid = brief_result.selected_author_keys == brief_input.author_keys
        && brief_result.research_status == pack.research_status
        && brief_result.answerability == pack.answerability
        && brief_result.requires_company_evidence
        && brief.author_key == brief_input.author_keys[0]
        && brief.ticker == brief_input.ticker;
    if valid {
        Ok(())
    } else {
        Err(ContractValueError::Semantic(
            KRW_GURU_COMPANY_BRIEF_RESULT_V1,
        ))
    }
}

/// Validate the successful seal returned for an exact committed retrieval and
/// brief request.  The default company workflow requires a non-null seal.
pub fn validate_company_brief_exchange(
    query_input_value: &Value,
    query_result_value: &Value,
    brief_input_value: &Value,
    brief_result_value: &Value,
) -> Result<(), ContractValueError> {
    validate_company_brief_input_linkage(query_input_value, query_result_value, brief_input_value)?;
    let query_result: GuruQueryContextResult =
        decode(query_result_value, KRW_GURU_QUERY_CONTEXT_RESULT_V1)?;
    let brief_input: GuruCompanyBriefInput =
        decode(brief_input_value, KRW_GURU_COMPANY_BRIEF_INPUT_V1)?;
    validate_company_brief_result_linkage(brief_input_value, brief_result_value)?;
    let brief_result: GuruCompanyBriefResult =
        decode(brief_result_value, KRW_GURU_COMPANY_BRIEF_RESULT_V1)?;
    let valid = brief_result.research_status == query_result.research_status
        && brief_result.answerability == query_result.answerability
        && brief_result.selected_author_keys == brief_input.author_keys;
    if valid {
        Ok(())
    } else {
        Err(ContractValueError::Semantic(
            KRW_GURU_COMPANY_BRIEF_RESULT_V1,
        ))
    }
}

/// Reproduce the authoritative TypeScript host projection from committed
/// filing-tool payloads into the immutable company research context. Results
/// are supplied in observation order and scanned newest-first.  This function
/// intentionally ignores non-ResearchState payloads and foreign tickers.
pub fn build_company_research_context(
    brief_value: &Value,
    observed_results: &[Value],
) -> Result<Value, ContractValueError> {
    let frame = compile_guru_research_frame(brief_value)?;
    let question_ids = vec![frame.question_id.clone()];
    let mut evidence_units = Vec::new();
    let mut source_object_ids = Vec::new();
    let mut seen_source_ids = BTreeSet::new();
    let mut seen_evidence = BTreeSet::new();

    for result in observed_results.iter().rev() {
        let research_state = if result.get("contract_version").and_then(Value::as_str)
            == Some("research-state/v2")
        {
            Some(result)
        } else {
            result
                .get("research_state")
                .filter(|value| value.is_object())
        };
        let Some(units) = research_state
            .and_then(|state| state.get("evidence_units"))
            .and_then(Value::as_array)
        else {
            continue;
        };
        for unit in units {
            let Some(object) = unit.as_object() else {
                continue;
            };
            let evidence_ticker = object
                .get("ticker")
                .and_then(Value::as_str)
                .map(str::trim)
                .map(str::to_uppercase)
                .unwrap_or_default();
            if evidence_ticker != frame.ticker {
                continue;
            }
            let mut object_ids = Vec::new();
            let mut unit_seen = BTreeSet::new();
            if let Some(object_id) = object.get("object_id").and_then(Value::as_str)
                && unit_seen.insert(object_id)
            {
                object_ids.push(object_id.to_owned());
            }
            if let Some(source_ids) = object
                .get("source")
                .and_then(Value::as_object)
                .and_then(|source| source.get("object_ids"))
                .and_then(Value::as_array)
            {
                for object_id in source_ids.iter().filter_map(Value::as_str) {
                    if unit_seen.insert(object_id) {
                        object_ids.push(object_id.to_owned());
                    }
                }
            }
            if object_ids.is_empty() {
                continue;
            }
            let evidence_key = object
                .get("evidence_id")
                .and_then(Value::as_str)
                .map_or_else(|| object_ids.join("|"), ToOwned::to_owned);
            if !seen_evidence.insert(evidence_key) {
                continue;
            }
            // Keep every projected unit linked to the bounded source-ID set.
            // A context result can contain many XBRL lineage IDs per unit;
            // previously we capped `source_object_ids` at 24 but copied all
            // IDs into each unit, making the deterministic projection fail
            // its own provenance validator as soon as the cap was reached.
            // Trim only newly unseen IDs after the cap; already admitted IDs
            // remain usable for later duplicate evidence units.
            let linked_object_ids = object_ids
                .into_iter()
                .filter(|object_id| {
                    if seen_source_ids.contains(object_id) {
                        true
                    } else if source_object_ids.len() < 24 {
                        seen_source_ids.insert(object_id.clone());
                        source_object_ids.push(object_id.clone());
                        true
                    } else {
                        false
                    }
                })
                .collect::<Vec<_>>();
            if linked_object_ids.is_empty() {
                continue;
            }
            if evidence_units.len() >= 12 {
                continue;
            }
            let mut projected = serde_json::Map::new();
            for field in ["evidence_id", "object_id", "object_type"] {
                if let Some(Value::String(value)) = object.get(field) {
                    projected.insert(field.to_owned(), Value::String(value.clone()));
                }
            }
            projected.insert("ticker".into(), Value::String(evidence_ticker));
            for field in [
                "period",
                "document_type",
                "title",
                "summary",
                "directness",
                "evidence_grade",
                "metric",
                "unit",
                "currency",
            ] {
                if let Some(Value::String(value)) = object.get(field) {
                    projected.insert(field.to_owned(), Value::String(value.clone()));
                }
            }
            if let Some(metric_points) = object.get("metric_points").and_then(Value::as_array) {
                let bounded = metric_points
                    .iter()
                    .take(MAX_GURU_METRIC_POINTS_PER_UNIT)
                    .cloned()
                    .collect::<Vec<_>>();
                if !bounded.is_empty() {
                    projected.insert("metric_points".into(), Value::Array(bounded));
                }
            }
            projected.insert(
                "source".into(),
                serde_json::json!({"object_ids": linked_object_ids}),
            );
            evidence_units.push(Value::Object(projected));
        }
    }
    if evidence_units.is_empty() || source_object_ids.is_empty() {
        return Err(ContractValueError::Semantic(
            KRW_GURU_COMPANY_RESEARCH_CONTEXT_V1,
        ));
    }
    let research_status = if evidence_units.len() >= 3 {
        "evidence_found"
    } else {
        "partial"
    };
    let context = serde_json::json!({
        "format": COMPANY_RESEARCH_CONTEXT_FORMAT,
        "ticker": frame.ticker,
        "brief_hash": frame.brief_hash,
        "question_ids": question_ids,
        "evidence_units": evidence_units,
        "source_object_ids": source_object_ids,
        "research_status": research_status,
    });
    validate_value(KRW_GURU_COMPANY_RESEARCH_CONTEXT_V1, &context)?;
    Ok(context)
}

/// Require a supplied review context to be the exact deterministic projection
/// of the filing results committed after the seal.
pub fn validate_company_research_context_provenance(
    brief_value: &Value,
    observed_results: &[Value],
    context_value: &Value,
) -> Result<(), ContractValueError> {
    let expected = build_company_research_context(brief_value, observed_results)?;
    validate_value(KRW_GURU_COMPANY_RESEARCH_CONTEXT_V1, context_value)?;
    if expected == *context_value {
        Ok(())
    } else {
        Err(ContractValueError::Semantic(
            KRW_GURU_COMPANY_RESEARCH_CONTEXT_V1,
        ))
    }
}

/// Validate the server-sealed brief against the exact selected pack and light
/// company context.  This catches stale packs, re-authored questions, wrong
/// authors/tickers, and principle/anchor injection before filing research.
pub fn validate_sealed_brief_linkage(
    pack_value: &Value,
    context_value: &Value,
    brief_value: &Value,
) -> Result<(), ContractValueError> {
    validate_value(KRW_GURU_RESEARCH_PACK_V1, pack_value)?;
    validate_value(KRW_GURU_LIGHT_COMPANY_CONTEXT_V1, context_value)?;
    validate_value(KRW_GURU_INVESTIGATION_BRIEF_V1, brief_value)?;
    let pack: GuruResearchPack = decode(pack_value, KRW_GURU_RESEARCH_PACK_V1)?;
    let context: GuruLightCompanyContext =
        decode(context_value, KRW_GURU_LIGHT_COMPANY_CONTEXT_V1)?;
    let brief: GuruInvestigationBrief = decode(brief_value, KRW_GURU_INVESTIGATION_BRIEF_V1)?;
    let selected_ids = pack
        .selected_lenses
        .iter()
        .filter(|lens| {
            lens.get("author_key")
                .and_then(Value::as_str)
                .is_none_or(|author| author == brief.author_key)
        })
        .filter_map(|lens| lens.get("reviewed_id").and_then(Value::as_str))
        .collect::<BTreeSet<_>>();
    let anchor_ids = context
        .context_anchors
        .iter()
        .map(|anchor| anchor.anchor_id.as_str())
        .collect::<BTreeSet<_>>();
    let question = &brief.questions[0];
    let valid = brief.research_pack_id == pack.pack_meta.pack_id
        && pack.pack_meta.guru_keys.contains(&brief.author_key)
        && brief.ticker == context.ticker
        && brief.company_context_hash == canonical_bare_hash(&context)?
        && !selected_ids.is_empty()
        && question
            .guru_principle_ids
            .iter()
            .all(|id| selected_ids.contains(id.as_str()))
        && question
            .company_context_anchor_ids
            .iter()
            .all(|id| anchor_ids.contains(id.as_str()));
    if valid {
        Ok(())
    } else {
        Err(ContractValueError::Semantic(
            KRW_GURU_INVESTIGATION_BRIEF_V1,
        ))
    }
}

/// Validate the default contextual review boundary.  Only `mixed` or
/// `unresolved` are accepted; cited IDs must come from the kernel-built
/// filing context, and unresolved assessments cite nothing.
pub fn validate_review_linkage_values(
    brief_value: &Value,
    context_value: &Value,
    analysis_value: &Value,
) -> Result<(), ContractValueError> {
    validate_value(KRW_GURU_INVESTIGATION_BRIEF_V1, brief_value)?;
    validate_value(KRW_GURU_COMPANY_RESEARCH_CONTEXT_V1, context_value)?;
    validate_value(KRW_GURU_AGENT_EVIDENCE_ANALYSIS_V1, analysis_value)?;
    let brief = decode(brief_value, KRW_GURU_INVESTIGATION_BRIEF_V1)?;
    let context = decode(context_value, KRW_GURU_COMPANY_RESEARCH_CONTEXT_V1)?;
    let analysis = decode(analysis_value, KRW_GURU_AGENT_EVIDENCE_ANALYSIS_V1)?;
    if validate_review_linkage(&brief, &context, &analysis)? {
        Ok(())
    } else {
        Err(ContractValueError::Semantic(
            KRW_GURU_AGENT_EVIDENCE_ANALYSIS_V1,
        ))
    }
}

/// Bind the public metadata of a review request to the exact fixed-author
/// retrieval that began the sealed workflow. Runtime-owned seal and filing
/// context are checked separately and more strictly by
/// [`validate_evidence_review_exchange`].
pub fn validate_evidence_review_input_origin(
    query_input_value: &Value,
    review_input_value: &Value,
) -> Result<(), ContractValueError> {
    validate_value(KRW_GURU_QUERY_CONTEXT_INPUT_V1, query_input_value)?;
    validate_value(KRW_GURU_EVIDENCE_REVIEW_INPUT_V1, review_input_value)?;
    let query: GuruQueryContextInput = decode(query_input_value, KRW_GURU_QUERY_CONTEXT_INPUT_V1)?;
    let review: GuruEvidenceReviewInput =
        decode(review_input_value, KRW_GURU_EVIDENCE_REVIEW_INPUT_V1)?;
    let company_name_matches = review.company_name.as_deref().is_none_or(|name| {
        query
            .company_context
            .as_ref()
            .is_some_and(|context| context.company_name == name)
    });
    let company_context_matches = review
        .company_context
        .as_ref()
        .is_none_or(|context| query.company_context.as_ref() == Some(context));
    let intent_matches = review
        .intent_family
        .as_ref()
        .is_none_or(|intent| query.intent_family.as_ref() == Some(intent));
    let valid = review.question == query.question
        && Some(&review.author_keys) == query.author_keys.as_ref()
        && Some(review.ticker.as_str()) == query.ticker.as_deref()
        && company_name_matches
        && company_context_matches
        && intent_matches;
    if valid {
        Ok(())
    } else {
        Err(ContractValueError::Semantic(
            KRW_GURU_EVIDENCE_REVIEW_INPUT_V1,
        ))
    }
}

/// Bind a returned validated review to the exact brief and runtime context.
pub fn validate_review_result_linkage(
    brief_value: &Value,
    context_value: &Value,
    result_value: &Value,
) -> Result<(), ContractValueError> {
    validate_value(KRW_GURU_INVESTIGATION_BRIEF_V1, brief_value)?;
    validate_value(KRW_GURU_COMPANY_RESEARCH_CONTEXT_V1, context_value)?;
    validate_value(KRW_GURU_EVIDENCE_REVIEW_RESULT_V1, result_value)?;
    let brief: GuruInvestigationBrief = decode(brief_value, KRW_GURU_INVESTIGATION_BRIEF_V1)?;
    let result: GuruEvidenceReviewResult =
        decode(result_value, KRW_GURU_EVIDENCE_REVIEW_RESULT_V1)?;
    let validated = &result.validated_evidence_analysis;
    let decision = &validated.decision_frame.questions[0];
    let question = &brief.questions[0];
    let valid = validated.brief_hash == brief.brief_hash
        // The exact context is already carried in `review_input_value` and is
        // compared with the committed Rust-built context by the complete
        // exchange validator. The Python review service may canonicalize the
        // same Pydantic projection with a release-specific JSON form, so the
        // returned digest is checked for shape here rather than re-hashed
        // across languages.
        && valid_bare_hash(&validated.research_context_hash)
        && validated.author_key == brief.author_key
        && validated.ticker == brief.ticker
        && validated.agent_analysis.assessments[0].question_id == question.question_id
        && decision.question_id == question.question_id
        && decision.question == question.question
        && decision.hypothesis == question.hypothesis
        && decision.counter_hypothesis == question.counter_hypothesis
        && decision.why_material == question.why_material
        && decision.change_condition == question.weakens_if;
    if valid {
        Ok(())
    } else {
        Err(ContractValueError::Semantic(
            KRW_GURU_EVIDENCE_REVIEW_RESULT_V1,
        ))
    }
}

/// Validate the complete review exchange against the exact committed seal and
/// deterministic filing-context projection.  The server is a validator here;
/// it cannot replace the main agent's analysis or rewrite runtime artifacts.
pub fn validate_evidence_review_exchange(
    committed_brief_value: &Value,
    committed_context_value: &Value,
    review_input_value: &Value,
    review_result_value: &Value,
) -> Result<(), ContractValueError> {
    validate_value(KRW_GURU_EVIDENCE_REVIEW_INPUT_V1, review_input_value)?;
    let input: GuruEvidenceReviewInput =
        decode(review_input_value, KRW_GURU_EVIDENCE_REVIEW_INPUT_V1)?;
    let input_brief =
        serde_json::to_value(&input.investigation_brief).map_err(ContractValueError::Json)?;
    let input_context =
        serde_json::to_value(&input.company_research_context).map_err(ContractValueError::Json)?;
    if input_brief != *committed_brief_value || input_context != *committed_context_value {
        return Err(ContractValueError::Semantic(
            KRW_GURU_EVIDENCE_REVIEW_INPUT_V1,
        ));
    }
    validate_review_linkage_values(
        committed_brief_value,
        committed_context_value,
        &serde_json::to_value(&input.agent_analysis).map_err(ContractValueError::Json)?,
    )?;
    validate_review_result_linkage(
        committed_brief_value,
        committed_context_value,
        review_result_value,
    )?;
    let result: GuruEvidenceReviewResult =
        decode(review_result_value, KRW_GURU_EVIDENCE_REVIEW_RESULT_V1)?;
    if result.validated_evidence_analysis.agent_analysis == input.agent_analysis {
        Ok(())
    } else {
        Err(ContractValueError::Semantic(
            KRW_GURU_EVIDENCE_REVIEW_RESULT_V1,
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedGuruBundle {
    pub manifest_sha256: String,
    pub authority_sha256: String,
    pub schema_bundle_sha256: String,
    pub conformance_vectors_sha256: String,
    pub contract_count: usize,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GuruManifest {
    format: String,
    authority: Value,
    authority_sha256: String,
    schema_bundle: ArtifactReference,
    conformance_vectors: ArtifactReference,
    contracts: BTreeMap<String, ManifestContract>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ArtifactReference {
    path: String,
    sha256: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestContract {
    authority_kind: String,
    authority_ref: String,
    schema_path: String,
    schema_sha256: String,
    semantic_validation: String,
}

fn verify_canonical(name: &str, bytes: &[u8]) -> Result<Value, ContractArtifactError> {
    let value: Value = serde_json::from_slice(bytes)?;
    if serde_jcs::to_vec(&value)? != bytes {
        return Err(ContractArtifactError::NonCanonical(name.to_owned()));
    }
    Ok(value)
}

fn verify_hash(name: &str, bytes: &[u8], expected: &str) -> Result<(), ContractArtifactError> {
    let actual = format!("sha256:{}", hex_digest(&Sha256::digest(bytes)));
    if actual == expected {
        Ok(())
    } else {
        Err(ContractArtifactError::HashMismatch {
            artifact: name.to_owned(),
            expected: expected.to_owned(),
            actual,
        })
    }
}

fn hex_digest(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(output, "{byte:02x}").expect("writing into String cannot fail");
    }
    output
}

/// Verify authority pins, every schema/hash, bundle membership, and vector
/// artifact identity.  Intended for build/readiness checks, not the hot path.
pub(super) fn verify_embedded() -> Result<VerifiedGuruBundle, ContractArtifactError> {
    let manifest_value = verify_canonical("krw-guru/manifest.json", MANIFEST_BYTES)?;
    verify_hash(
        "krw-guru/manifest.json",
        MANIFEST_BYTES,
        GURU_GENERATED_MANIFEST_SHA256,
    )?;
    let manifest: GuruManifest = serde_json::from_value(manifest_value)?;
    if manifest.format != GURU_FORMAT
        || manifest.contracts.len() != CONTRACT_COUNT
        || manifest.schema_bundle.path != "schema-bundle.json"
        || manifest.conformance_vectors.path != "conformance-vectors.json"
    {
        return Err(ContractArtifactError::InvalidManifest(
            "invalid Guru manifest header",
        ));
    }
    verify_hash(
        "krw-guru authority",
        &serde_jcs::to_vec(&manifest.authority)?,
        &manifest.authority_sha256,
    )?;
    let bundle = verify_canonical("krw-guru/schema-bundle.json", BUNDLE_BYTES)?;
    verify_hash(
        "krw-guru/schema-bundle.json",
        BUNDLE_BYTES,
        &manifest.schema_bundle.sha256,
    )?;
    if bundle.get("format").and_then(Value::as_str) != Some(GURU_FORMAT)
        || bundle.get("authority_sha256").and_then(Value::as_str)
            != Some(&manifest.authority_sha256)
    {
        return Err(ContractArtifactError::InvalidManifest(
            "Guru schema bundle authority mismatch",
        ));
    }
    let bundled = bundle.get("contracts").and_then(Value::as_object).ok_or(
        ContractArtifactError::InvalidManifest("Guru schema bundle has no contracts"),
    )?;
    for descriptor in descriptors() {
        let schema_value = verify_canonical(descriptor.id, descriptor.schema)?;
        verify_hash(descriptor.id, descriptor.schema, descriptor.schema_sha256)?;
        let entry =
            manifest
                .contracts
                .get(descriptor.id)
                .ok_or(ContractArtifactError::InvalidManifest(
                    "Guru manifest is missing a contract",
                ))?;
        let expected_path = format!("schemas/{}.json", descriptor.id.replace('/', "-"));
        if entry.schema_path != expected_path
            || entry.schema_sha256 != descriptor.schema_sha256
            || entry.authority_kind != "typescript-host-plus-python-guru-mcp"
            || entry.authority_ref != "default sealed Guru workflow"
            || entry.semantic_validation != "bounded_rust_typed_and_cross_artifact_validator"
            || bundled.get(descriptor.id) != Some(&schema_value)
        {
            return Err(ContractArtifactError::InvalidManifest(
                "Guru contract manifest or bundle mismatch",
            ));
        }
    }
    let vectors = verify_canonical("krw-guru/conformance-vectors.json", VECTOR_BYTES)?;
    verify_hash(
        "krw-guru/conformance-vectors.json",
        VECTOR_BYTES,
        &manifest.conformance_vectors.sha256,
    )?;
    if vectors.get("format").and_then(Value::as_str) != Some(CONFORMANCE_FORMAT)
        || vectors.get("authority_sha256").and_then(Value::as_str)
            != Some(&manifest.authority_sha256)
    {
        return Err(ContractArtifactError::InvalidManifest(
            "Guru conformance vector authority mismatch",
        ));
    }
    Ok(VerifiedGuruBundle {
        manifest_sha256: GURU_GENERATED_MANIFEST_SHA256.to_owned(),
        authority_sha256: manifest.authority_sha256,
        schema_bundle_sha256: manifest.schema_bundle.sha256,
        conformance_vectors_sha256: manifest.conformance_vectors.sha256,
        contract_count: manifest.contracts.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Deserialize)]
    struct Vectors {
        vectors: Vec<Vector>,
    }

    #[derive(Debug, Deserialize)]
    struct Vector {
        contract_id: String,
        valid: bool,
        value: Value,
    }

    fn fixture(contract_id: &str) -> Value {
        let vectors: Vectors = serde_json::from_slice(VECTOR_BYTES).expect("vectors");
        vectors
            .vectors
            .into_iter()
            .find(|vector| vector.contract_id == contract_id && vector.valid)
            .expect("positive fixture")
            .value
    }

    fn sealed_search_plan() -> Value {
        let brief = fixture(KRW_GURU_INVESTIGATION_BRIEF_V1);
        let question = &brief["questions"][0];
        let needs = question["evidence_needed"]
            .as_array()
            .expect("sealed proof needs");
        let clauses = needs
            .iter()
            .enumerate()
            .map(|(index, need)| {
                serde_json::json!({
                    "clause_id": format!("sealed_need_{}", index + 1),
                    "retrieval_query": format!("AAPL {}", need.as_str().unwrap()),
                    "required_concepts": [need],
                    "required_predicates": [],
                    "required": true,
                    "tickers": [brief["ticker"]],
                    "directness": "direct_required",
                    "object_types": [],
                    "metrics": [],
                    "metric_dimensions": [],
                    "metric_scope": "company_total",
                    "calculation_window": null
                })
            })
            .collect::<Vec<_>>();
        serde_json::json!({
            "question": question["question"],
            "intent": "company_research",
            "clauses": clauses,
            "tickers": [brief["ticker"]],
            "document_types": ["10-Q", "10-K"],
            "periods": [],
            "universe": null,
            "comparison_axes": ["directness"],
            "answer_scope": "direct",
            "uncertainty": "low",
            "limit_results": 8,
            "limit_tickers": 1
        })
    }

    #[test]
    fn artifact_set_is_canonical_hash_bound_and_complete() {
        let verified = verify_embedded().expect("Guru artifacts must verify");
        assert_eq!(verified.contract_count, CONTRACT_COUNT);
        assert_eq!(descriptors().len(), CONTRACT_COUNT);
    }

    #[test]
    fn all_generated_conformance_vectors_match_bounded_validator() {
        let vectors: Vectors = serde_json::from_slice(VECTOR_BYTES).expect("vectors");
        assert_eq!(vectors.vectors.len(), CONTRACT_COUNT * 2);
        for vector in vectors.vectors {
            assert_eq!(
                validate_value(&vector.contract_id, &vector.value).is_ok(),
                vector.valid,
                "{}",
                vector.contract_id
            );
        }
    }

    #[test]
    fn pack_identity_and_seal_linkage_are_content_bound() {
        let pack = fixture(KRW_GURU_RESEARCH_PACK_V1);
        let context = fixture(KRW_GURU_LIGHT_COMPANY_CONTEXT_V1);
        let brief = fixture(KRW_GURU_INVESTIGATION_BRIEF_V1);
        let identity = research_pack_identity(&pack).expect("identity");
        assert!(identity.starts_with("sha256:"));
        validate_sealed_brief_linkage(&pack, &context, &brief).expect("linked seal");

        let mut changed = pack;
        changed["selected_lenses"] = serde_json::json!([]);
        assert!(validate_sealed_brief_linkage(&changed, &context, &brief).is_err());

        let mut changed_brief = brief;
        changed_brief["ticker"] = Value::String("MSFT".to_owned());
        assert!(validate_value(KRW_GURU_INVESTIGATION_BRIEF_V1, &changed_brief).is_err());
    }

    #[test]
    fn kernel_builders_inject_private_artifacts_from_committed_inputs() {
        let query_input = fixture(KRW_GURU_QUERY_CONTEXT_INPUT_V1);
        let query_result = fixture(KRW_GURU_QUERY_CONTEXT_RESULT_V1);
        let pack = query_result["research_pack"].clone();
        let draft = fixture(KRW_GURU_INVESTIGATION_QUESTION_DRAFT_V1);
        let brief_input =
            build_company_brief_input(&query_input, &pack, &draft).expect("brief input");
        validate_company_brief_input_identity(
            &query_input,
            &research_pack_identity(&pack).expect("pack identity"),
            &brief_input,
        )
        .expect("kernel-injected brief input");
        assert_eq!(
            brief_input["investigation_questions"],
            serde_json::json!([draft])
        );
        assert_eq!(brief_input["guru_query_context"], pack);
        let mut foreign_principle = brief_input.clone();
        foreign_principle["investigation_questions"][0]["guru_principle_ids"] =
            serde_json::json!(["guru:marks:invented"]);
        assert!(
            validate_value(KRW_GURU_COMPANY_BRIEF_INPUT_V1, &foreign_principle).is_err(),
            "a physical envelope may reference only principles selected by the sealed pack"
        );

        let brief = fixture(KRW_GURU_INVESTIGATION_BRIEF_V1);
        let context = fixture(KRW_GURU_COMPANY_RESEARCH_CONTEXT_V1);
        let analysis = fixture(KRW_GURU_AGENT_EVIDENCE_ANALYSIS_V1);
        let review_input = build_evidence_review_input(&query_input, &brief, &context, &analysis)
            .expect("review input");
        assert_eq!(review_input["investigation_brief"], brief);
        assert_eq!(review_input["company_research_context"], context);
        assert_eq!(review_input["agent_analysis"], analysis);
    }

    #[test]
    fn sealed_question_allows_the_full_bounded_linked_search_plan() {
        let brief = fixture(KRW_GURU_INVESTIGATION_BRIEF_V1);
        let plan = sealed_search_plan();
        validate_guru_company_search_plan(&brief, &plan).expect("sealed SearchPlan");

        let mut too_many = plan.clone();
        let duplicate = too_many["clauses"][0].clone();
        for index in 0..20 {
            let mut value = duplicate.clone();
            value["clause_id"] = Value::String(format!("extra_clause_{index}"));
            too_many["clauses"].as_array_mut().unwrap().push(value);
        }
        assert!(validate_guru_company_search_plan(&brief, &too_many).is_err());

        let mut rewritten_question = plan.clone();
        rewritten_question["question"] = Value::String("A broader company sweep".into());
        assert!(validate_guru_company_search_plan(&brief, &rewritten_question).is_err());

        let mut wrong_ticker = plan;
        wrong_ticker["clauses"][0]["tickers"] = serde_json::json!(["MSFT"]);
        assert!(validate_guru_company_search_plan(&brief, &wrong_ticker).is_err());
    }

    #[test]
    fn front_camel_case_light_context_normalizes_to_sealing_model() {
        let expected = fixture(KRW_GURU_LIGHT_COMPANY_CONTEXT_V1);
        let camel = serde_json::json!({
            "format": expected["format"],
            "ticker": expected["ticker"],
            "companyName": expected["company_name"],
            "sector": expected["sector"],
            "businessDescription": expected["business_description"],
            "primaryActivities": expected["primary_activities"],
            "productsOrSegments": expected["products_or_segments"],
            "revenueLogic": expected["revenue_logic"],
            "contextAnchors": expected["context_anchors"].as_array().unwrap().iter().map(|anchor| {
                serde_json::json!({
                    "anchorId": anchor["anchor_id"],
                    "kind": anchor["kind"],
                    "text": anchor["text"],
                })
            }).collect::<Vec<_>>(),
            "filingAvailability": {
                "hasCurrentFiling": expected["filing_availability"]["has_current_filing"],
                "hasAnnualBaseline": expected["filing_availability"]["has_annual_baseline"],
            },
        });
        let normalized = normalize_light_company_context(&camel).expect("normalizes");
        assert_eq!(normalized["company_name"], expected["company_name"]);
        assert!(normalized.get("companyName").is_none());
        assert_eq!(normalized["industry"], Value::Null);

        let pack = fixture(KRW_GURU_RESEARCH_PACK_V1);
        let brief = fixture(KRW_GURU_INVESTIGATION_BRIEF_V1);
        validate_sealed_brief_linkage(&pack, &normalized, &brief).expect("normalized hash");
    }

    #[test]
    fn contextual_review_accepts_only_linked_mixed_or_unresolved_analysis() {
        let brief = fixture(KRW_GURU_INVESTIGATION_BRIEF_V1);
        let context = fixture(KRW_GURU_COMPANY_RESEARCH_CONTEXT_V1);
        let analysis = fixture(KRW_GURU_AGENT_EVIDENCE_ANALYSIS_V1);
        validate_review_linkage_values(&brief, &context, &analysis).expect("linked review");

        let mut invented = analysis.clone();
        invented["assessments"][0]["evidence_object_ids"] =
            serde_json::json!(["claim:AAPL:invented"]);
        assert!(validate_review_linkage_values(&brief, &context, &invented).is_err());

        let mut unsupported = analysis;
        unsupported["assessments"][0]["verdict"] = Value::String("supported".to_owned());
        assert!(validate_value(KRW_GURU_AGENT_EVIDENCE_ANALYSIS_V1, &unsupported).is_err());
    }

    #[test]
    fn returned_review_is_bound_to_exact_brief_and_runtime_context() {
        let brief = fixture(KRW_GURU_INVESTIGATION_BRIEF_V1);
        let context = fixture(KRW_GURU_COMPANY_RESEARCH_CONTEXT_V1);
        let result = fixture(KRW_GURU_EVIDENCE_REVIEW_RESULT_V1);
        validate_review_result_linkage(&brief, &context, &result).expect("linked result");

        let mut changed = result;
        changed["validated_evidence_analysis"]["ticker"] = Value::String("MSFT".to_owned());
        assert!(validate_review_result_linkage(&brief, &context, &changed).is_err());
    }

    #[test]
    fn correction_next_tool_is_code_bound_and_never_invents_verification() {
        let correction = fixture(KRW_GURU_INPUT_CORRECTION_V1);
        validate_value(KRW_GURU_INPUT_CORRECTION_V1, &correction).expect("correction");
        assert_eq!(
            correction["allowed_next_tools"],
            serde_json::json!(["krw_guru_review_company_evidence"])
        );

        let mut invalid = correction;
        invalid["allowed_next_tools"] = serde_json::json!(["krw_ontology_verify_evidence"]);
        assert!(validate_value(KRW_GURU_INPUT_CORRECTION_V1, &invalid).is_err());
    }

    #[test]
    fn default_query_brief_and_review_exchanges_are_exactly_bound() {
        let query_input = fixture(KRW_GURU_QUERY_CONTEXT_INPUT_V1);
        let query_result = fixture(KRW_GURU_QUERY_CONTEXT_RESULT_V1);
        let brief_input = fixture(KRW_GURU_COMPANY_BRIEF_INPUT_V1);
        let brief_result = fixture(KRW_GURU_COMPANY_BRIEF_RESULT_V1);
        validate_guru_query_exchange(&query_input, &query_result).expect("query exchange");
        validate_company_brief_exchange(&query_input, &query_result, &brief_input, &brief_result)
            .expect("brief exchange");

        let review_input = fixture(KRW_GURU_EVIDENCE_REVIEW_INPUT_V1);
        let review_result = fixture(KRW_GURU_EVIDENCE_REVIEW_RESULT_V1);
        let brief = fixture(KRW_GURU_INVESTIGATION_BRIEF_V1);
        let context = fixture(KRW_GURU_COMPANY_RESEARCH_CONTEXT_V1);
        validate_evidence_review_input_origin(&query_input, &review_input).expect("review origin");
        validate_evidence_review_exchange(&brief, &context, &review_input, &review_result)
            .expect("review exchange");

        let mut spoofed = query_result;
        spoofed["selected_author_keys"] = serde_json::json!(["marks"]);
        assert!(validate_guru_query_exchange(&query_input, &spoofed).is_err());

        let mut changed_question = fixture(KRW_GURU_EVIDENCE_REVIEW_INPUT_V1);
        changed_question["question"] = Value::String("A different user question.".to_owned());
        assert!(validate_evidence_review_input_origin(&query_input, &changed_question).is_err());

        let mut rewritten = review_input;
        rewritten["company_research_context"]["source_object_ids"] =
            serde_json::json!(["claim:AAPL:invented"]);
        assert!(
            validate_evidence_review_exchange(&brief, &context, &rewritten, &review_result)
                .is_err()
        );

        let exact_input = fixture(KRW_GURU_EVIDENCE_REVIEW_INPUT_V1);
        let mut replaced_analysis = review_result;
        replaced_analysis["validated_evidence_analysis"]["agent_analysis"]["overall_judgment"] =
            Value::String("The server must not replace the main agent analysis.".to_owned());
        assert!(
            validate_evidence_review_exchange(&brief, &context, &exact_input, &replaced_analysis)
                .is_err()
        );
    }

    #[test]
    fn runtime_company_context_is_derived_only_from_observed_filing_units() {
        let brief = fixture(KRW_GURU_INVESTIGATION_BRIEF_V1);
        let expected = fixture(KRW_GURU_COMPANY_RESEARCH_CONTEXT_V1);
        let observed = serde_json::json!({
            "contract_version": "research-state/v2",
            "evidence_units": expected["evidence_units"],
        });
        let built = build_company_research_context(&brief, std::slice::from_ref(&observed))
            .expect("deterministic context");
        assert_eq!(built, expected);
        validate_company_research_context_provenance(&brief, &[observed], &expected)
            .expect("provenance");

        let foreign = serde_json::json!({
            "contract_version": "research-state/v2",
            "evidence_units": [{
                "evidence_id": "ev-foreign",
                "object_id": "claim:MSFT:foreign",
                "object_type": "ResearchClaim",
                "ticker": "MSFT",
                "source": {"object_ids": ["claim:MSFT:foreign"]}
            }]
        });
        assert!(build_company_research_context(&brief, &[foreign]).is_err());
    }
}
