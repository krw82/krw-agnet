//! Kernel-owned product contracts for routing, notebook transformation, and
//! display planning.
//!
//! These contracts deliberately separate model proposals from authority. A
//! router decision is bound to its canonical request, notebook output is bound
//! to one immutable transform input, and a display plan can only reference
//! units from a committed answer source. None of the output contracts contains
//! an arbitrary JSON escape hatch.

use std::collections::{BTreeMap, BTreeSet};

use krw_agent_protocol::ContentHash;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{ContractDescriptor, ContractValueError};

pub const ROUTING_REQUEST_V1: &str = "routing-request/v1";
pub const ROUTING_DECISION_V2: &str = "routing-decision/v2";
pub const NOTEBOOK_TRANSFORM_INPUT_V1: &str = "notebook-transform-input/v1";
pub const NOTEBOOK_TRANSFORM_V2: &str = "notebook-transform/v2";
pub const CANONICAL_DISPLAY_SOURCE_V1: &str = "canonical-display-source/v1";
pub const DISPLAY_PLAN_V2: &str = "display-plan/v2";

pub const ROUTING_REQUEST_V1_SCHEMA_SHA256: &str =
    "sha256:1455f7456615f74b2766ef3bfa4d94ffebeb67a7f99a2aff098e95aecab70ef0";
pub const ROUTING_DECISION_V2_SCHEMA_SHA256: &str =
    "sha256:02b86a4fb19673ffa772d49ca6a4067e43d686121e44e41dbc73990c67abfc39";
pub const NOTEBOOK_TRANSFORM_INPUT_V1_SCHEMA_SHA256: &str =
    "sha256:52f7a7195c236c0305a436deb4eb88441f91f747f7325fb34aaa1d0948b6bc27";
pub const NOTEBOOK_TRANSFORM_V2_SCHEMA_SHA256: &str =
    "sha256:2ebc0aac5e25631da1f0a93754c477904caf2f598d888e545d807159d1a2fb56";
pub const CANONICAL_DISPLAY_SOURCE_V1_SCHEMA_SHA256: &str =
    "sha256:688ea27e5e4b143ddceddd2785334036adefec8171e4def4a40b4cce66bc1b7a";
pub const DISPLAY_PLAN_V2_SCHEMA_SHA256: &str =
    "sha256:6f3f62af5aa631894be1d5de2a4f37d88a7ebfaf3a838241d1dcfa7fdeba0ce3";

const ROUTING_REQUEST_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../contracts/kernel/v2/schemas/routing-request-v1.json"
));
const ROUTING_DECISION_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../contracts/kernel/v2/schemas/routing-decision-v2.json"
));
const NOTEBOOK_TRANSFORM_INPUT_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../contracts/kernel/v2/schemas/notebook-transform-input-v1.json"
));
const NOTEBOOK_TRANSFORM_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../contracts/kernel/v2/schemas/notebook-transform-v2.json"
));
const CANONICAL_DISPLAY_SOURCE_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../contracts/kernel/v2/schemas/canonical-display-source-v1.json"
));
const DISPLAY_PLAN_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../contracts/kernel/v2/schemas/display-plan-v2.json"
));

pub(crate) fn contract(contract_id: &str) -> Option<ContractDescriptor> {
    let (id, schema_sha256, schema) = match contract_id {
        ROUTING_REQUEST_V1 => (
            ROUTING_REQUEST_V1,
            ROUTING_REQUEST_V1_SCHEMA_SHA256,
            ROUTING_REQUEST_BYTES,
        ),
        ROUTING_DECISION_V2 => (
            ROUTING_DECISION_V2,
            ROUTING_DECISION_V2_SCHEMA_SHA256,
            ROUTING_DECISION_BYTES,
        ),
        NOTEBOOK_TRANSFORM_INPUT_V1 => (
            NOTEBOOK_TRANSFORM_INPUT_V1,
            NOTEBOOK_TRANSFORM_INPUT_V1_SCHEMA_SHA256,
            NOTEBOOK_TRANSFORM_INPUT_BYTES,
        ),
        NOTEBOOK_TRANSFORM_V2 => (
            NOTEBOOK_TRANSFORM_V2,
            NOTEBOOK_TRANSFORM_V2_SCHEMA_SHA256,
            NOTEBOOK_TRANSFORM_BYTES,
        ),
        CANONICAL_DISPLAY_SOURCE_V1 => (
            CANONICAL_DISPLAY_SOURCE_V1,
            CANONICAL_DISPLAY_SOURCE_V1_SCHEMA_SHA256,
            CANONICAL_DISPLAY_SOURCE_BYTES,
        ),
        DISPLAY_PLAN_V2 => (
            DISPLAY_PLAN_V2,
            DISPLAY_PLAN_V2_SCHEMA_SHA256,
            DISPLAY_PLAN_BYTES,
        ),
        _ => return None,
    };
    Some(ContractDescriptor {
        id,
        schema_sha256,
        schema,
    })
}

pub(crate) fn descriptors() -> Vec<ContractDescriptor> {
    [
        ROUTING_REQUEST_V1,
        ROUTING_DECISION_V2,
        NOTEBOOK_TRANSFORM_INPUT_V1,
        NOTEBOOK_TRANSFORM_V2,
        CANONICAL_DISPLAY_SOURCE_V1,
        DISPLAY_PLAN_V2,
    ]
    .into_iter()
    .map(|id| contract(id).expect("static product contract"))
    .collect()
}

pub(crate) fn validate_value(contract_id: &str, value: &Value) -> Result<(), ContractValueError> {
    match contract_id {
        ROUTING_REQUEST_V1 => parse::<RoutingRequestV1>(value, ROUTING_REQUEST_V1)?.validate(),
        ROUTING_DECISION_V2 => parse::<RoutingDecisionV2>(value, ROUTING_DECISION_V2)?.validate(),
        NOTEBOOK_TRANSFORM_INPUT_V1 => {
            parse::<NotebookTransformInputV1>(value, NOTEBOOK_TRANSFORM_INPUT_V1)?.validate()
        }
        NOTEBOOK_TRANSFORM_V2 => {
            parse::<NotebookTransformV2>(value, NOTEBOOK_TRANSFORM_V2)?.validate()
        }
        CANONICAL_DISPLAY_SOURCE_V1 => {
            parse::<CanonicalDisplaySourceV1>(value, CANONICAL_DISPLAY_SOURCE_V1)?.validate()
        }
        DISPLAY_PLAN_V2 => parse::<DisplayPlanV2>(value, DISPLAY_PLAN_V2)?.validate(),
        _ => Err(ContractValueError::UnknownContract(contract_id.to_owned())),
    }
}

fn parse<T: for<'de> Deserialize<'de>>(
    value: &Value,
    contract: &'static str,
) -> Result<T, ContractValueError> {
    serde_json::from_value(value.clone()).map_err(|_| ContractValueError::Shape(contract))
}

fn canonical_hash<T: Serialize>(value: &T) -> Result<ContentHash, ContractValueError> {
    serde_jcs::to_vec(value)
        .map(ContentHash::sha256)
        .map_err(ContractValueError::Json)
}

fn valid_hash(value: &str) -> bool {
    ContentHash::parse(value).is_ok()
}

fn bounded_text(value: &str, min: usize, max: usize) -> bool {
    value.len() >= min && value.len() <= max && !value.contains('\0')
}

fn valid_ticker(value: &str) -> bool {
    let bytes = value.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 32
        && bytes[0].is_ascii_alphanumeric()
        && bytes.iter().all(|byte| {
            byte.is_ascii_uppercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'-')
        })
}

fn valid_id(value: &str) -> bool {
    let bytes = value.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 128
        && bytes[0].is_ascii_alphanumeric()
        && bytes.iter().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-' | b':' | b'/')
        })
}

fn unique_bounded_ids(values: &[String], max: usize) -> bool {
    values.len() <= max
        && values.iter().all(|value| valid_id(value))
        && values.iter().collect::<BTreeSet<_>>().len() == values.len()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProductRunKind {
    CompanyResearch,
    CompanyResearchEn,
    EarningsDeepDive,
    ScenarioSensitivity,
    IdeaGeneration,
    NewsDiscovery,
    NewsResearch,
    MarketMoveResearch,
    NewsFilingCorrelation,
    GuruAckman,
    GuruBuffett,
    GuruFlatt,
    GuruMarks,
    GuruTerrySmith,
    SourceFilingFollowup,
}

impl ProductRunKind {
    const fn analysis_mode(self) -> AnalysisMode {
        match self {
            Self::NewsDiscovery | Self::NewsResearch | Self::MarketMoveResearch => {
                AnalysisMode::News
            }
            Self::CompanyResearch
            | Self::CompanyResearchEn
            | Self::EarningsDeepDive
            | Self::ScenarioSensitivity
            | Self::IdeaGeneration
            | Self::NewsFilingCorrelation
            | Self::GuruAckman
            | Self::GuruBuffett
            | Self::GuruFlatt
            | Self::GuruMarks
            | Self::GuruTerrySmith
            | Self::SourceFilingFollowup => AnalysisMode::Company,
        }
    }

    const fn is_auto_routable(self) -> bool {
        matches!(
            self,
            Self::CompanyResearch
                | Self::ScenarioSensitivity
                | Self::IdeaGeneration
                | Self::NewsDiscovery
                | Self::MarketMoveResearch
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalysisMode {
    Company,
    News,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatScopeType {
    Global,
    Company,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResponseLocale {
    #[serde(rename = "ko-KR")]
    KoKr,
    #[serde(rename = "en-US")]
    EnUs,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutingRequestV1 {
    pub schema_version: u16,
    pub question: String,
    pub current_analysis_mode: AnalysisMode,
    pub current_run_kind: ProductRunKind,
    pub trusted_tickers: Vec<String>,
    pub scope_type: ChatScopeType,
    pub has_user_urls: bool,
    pub response_locale: ResponseLocale,
    pub session_memory_ref: Option<String>,
}

impl RoutingRequestV1 {
    pub fn validate(&self) -> Result<(), ContractValueError> {
        if self.schema_version != 1
            || !bounded_text(&self.question, 1, 64 * 1024)
            || self.trusted_tickers.len() > 50
            || self
                .trusted_tickers
                .iter()
                .any(|ticker| !valid_ticker(ticker))
            || self.trusted_tickers.iter().collect::<BTreeSet<_>>().len()
                != self.trusted_tickers.len()
            || self
                .session_memory_ref
                .as_deref()
                .is_some_and(|value| !valid_hash(value))
        {
            return Err(ContractValueError::Shape(ROUTING_REQUEST_V1));
        }
        if self.scope_type == ChatScopeType::Company && self.trusted_tickers.is_empty() {
            return Err(ContractValueError::Semantic(ROUTING_REQUEST_V1));
        }
        Ok(())
    }

    pub fn content_hash(&self) -> Result<ContentHash, ContractValueError> {
        self.validate()?;
        canonical_hash(self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteOrigin {
    Preserved,
    Deterministic,
    Model,
    Fallback,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteConfidence {
    High,
    Medium,
    Low,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteReasonCode {
    PreservedExplicitRoute,
    MarketPriceMove,
    CurrentNewsDiscovery,
    FilingGroundedResearch,
    CandidateDiscovery,
    ScenarioQuestion,
    SectorCohortResearch,
    ModelClassification,
    SafeCompanyDefault,
    RouterUnavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutingDecisionV2 {
    pub schema_version: u16,
    pub input_hash: String,
    pub run_kind: ProductRunKind,
    pub analysis_mode: AnalysisMode,
    pub origin: RouteOrigin,
    pub confidence: RouteConfidence,
    pub reason_code: RouteReasonCode,
}

impl RoutingDecisionV2 {
    pub fn validate(&self) -> Result<(), ContractValueError> {
        if self.schema_version != 2
            || !valid_hash(&self.input_hash)
            || self.analysis_mode != self.run_kind.analysis_mode()
            || (self.origin != RouteOrigin::Preserved && !self.run_kind.is_auto_routable())
            || !route_reason_matches(self)
        {
            return Err(ContractValueError::Semantic(ROUTING_DECISION_V2));
        }
        Ok(())
    }
}

fn route_reason_matches(decision: &RoutingDecisionV2) -> bool {
    match decision.reason_code {
        RouteReasonCode::PreservedExplicitRoute => decision.origin == RouteOrigin::Preserved,
        RouteReasonCode::MarketPriceMove => {
            decision.run_kind == ProductRunKind::MarketMoveResearch
                && decision.origin == RouteOrigin::Deterministic
        }
        RouteReasonCode::CurrentNewsDiscovery => {
            decision.run_kind == ProductRunKind::NewsDiscovery
                && decision.origin == RouteOrigin::Deterministic
        }
        RouteReasonCode::FilingGroundedResearch
        | RouteReasonCode::SectorCohortResearch
        | RouteReasonCode::SafeCompanyDefault => {
            decision.run_kind == ProductRunKind::CompanyResearch
        }
        RouteReasonCode::CandidateDiscovery => decision.run_kind == ProductRunKind::IdeaGeneration,
        RouteReasonCode::ScenarioQuestion => {
            decision.run_kind == ProductRunKind::ScenarioSensitivity
        }
        RouteReasonCode::ModelClassification => decision.origin == RouteOrigin::Model,
        RouteReasonCode::RouterUnavailable => {
            decision.origin == RouteOrigin::Fallback
                && decision.run_kind == ProductRunKind::CompanyResearch
                && decision.confidence == RouteConfidence::Low
        }
    }
}

pub fn validate_routing_linkage(
    request: &RoutingRequestV1,
    decision: &RoutingDecisionV2,
) -> Result<(), ContractValueError> {
    request.validate()?;
    decision.validate()?;
    if decision.input_hash != request.content_hash()?.as_str() {
        return Err(ContractValueError::Semantic(ROUTING_DECISION_V2));
    }
    if request.has_user_urls
        && (decision.origin != RouteOrigin::Preserved
            || decision.run_kind != request.current_run_kind)
    {
        return Err(ContractValueError::Semantic(ROUTING_DECISION_V2));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NotebookUpdateMode {
    MergeResearchAnswer,
    ExtractOpenQuestions,
    AppendNote,
    ReorganizeNotebook,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NotebookConversationV1 {
    pub source_message_hash: String,
    pub answer_bundle_hash: Option<String>,
    pub title: String,
    pub user_question: String,
    pub assistant_answer: String,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NotebookTransformInputV1 {
    pub schema_version: u16,
    pub ticker: String,
    pub company_name: String,
    pub basis_period: Option<String>,
    pub update_mode: NotebookUpdateMode,
    pub existing_notebook_md: String,
    pub recent_conversations: Vec<NotebookConversationV1>,
}

impl NotebookTransformInputV1 {
    pub fn validate(&self) -> Result<(), ContractValueError> {
        if self.schema_version != 1
            || !valid_ticker(&self.ticker)
            || !bounded_text(&self.company_name, 1, 256)
            || self
                .basis_period
                .as_deref()
                .is_some_and(|value| !bounded_text(value, 1, 128))
            || !bounded_text(&self.existing_notebook_md, 1, 100_000)
            || self.recent_conversations.len() > 8
        {
            return Err(ContractValueError::Shape(NOTEBOOK_TRANSFORM_INPUT_V1));
        }
        let mut hashes = BTreeSet::new();
        let mut conversation_bytes = 0_usize;
        for conversation in &self.recent_conversations {
            conversation_bytes = conversation_bytes
                .saturating_add(conversation.title.len())
                .saturating_add(conversation.user_question.len())
                .saturating_add(conversation.assistant_answer.len());
            if !valid_hash(&conversation.source_message_hash)
                || !hashes.insert(&conversation.source_message_hash)
                || conversation
                    .answer_bundle_hash
                    .as_deref()
                    .is_some_and(|value| !valid_hash(value))
                || !bounded_text(&conversation.title, 1, 512)
                || !bounded_text(&conversation.user_question, 1, 8 * 1024)
                || !bounded_text(&conversation.assistant_answer, 1, 24 * 1024)
                || !bounded_text(&conversation.created_at, 1, 64)
            {
                return Err(ContractValueError::Shape(NOTEBOOK_TRANSFORM_INPUT_V1));
            }
        }
        if conversation_bytes > 128 * 1024 {
            return Err(ContractValueError::Limit(NOTEBOOK_TRANSFORM_INPUT_V1));
        }
        Ok(())
    }

    pub fn content_hash(&self) -> Result<ContentHash, ContractValueError> {
        self.validate()?;
        canonical_hash(self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NotebookWriteMode {
    MergeResearchAnswer,
    AppendNote,
    ReorganizeNotebook,
}

impl NotebookWriteMode {
    const fn as_update_mode(self) -> NotebookUpdateMode {
        match self {
            Self::MergeResearchAnswer => NotebookUpdateMode::MergeResearchAnswer,
            Self::AppendNote => NotebookUpdateMode::AppendNote,
            Self::ReorganizeNotebook => NotebookUpdateMode::ReorganizeNotebook,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum NotebookTransformV2 {
    NotebookMarkdown {
        schema_version: u16,
        source_input_hash: String,
        ticker: String,
        update_mode: NotebookWriteMode,
        markdown: String,
    },
    OpenQuestions {
        schema_version: u16,
        source_input_hash: String,
        ticker: String,
        update_mode: NotebookUpdateMode,
        questions: Vec<String>,
    },
}

impl NotebookTransformV2 {
    pub fn validate(&self) -> Result<(), ContractValueError> {
        match self {
            Self::NotebookMarkdown {
                schema_version,
                source_input_hash,
                ticker,
                markdown,
                ..
            } => {
                if *schema_version != 2 || !valid_hash(source_input_hash) || !valid_ticker(ticker) {
                    return Err(ContractValueError::Shape(NOTEBOOK_TRANSFORM_V2));
                }
                validate_notebook_markdown(markdown)
            }
            Self::OpenQuestions {
                schema_version,
                source_input_hash,
                ticker,
                update_mode,
                questions,
            } => {
                if *schema_version != 2
                    || !valid_hash(source_input_hash)
                    || !valid_ticker(ticker)
                    || *update_mode != NotebookUpdateMode::ExtractOpenQuestions
                {
                    return Err(ContractValueError::Shape(NOTEBOOK_TRANSFORM_V2));
                }
                validate_open_questions(questions)
            }
        }
    }
}

pub fn validate_notebook_linkage(
    input: &NotebookTransformInputV1,
    output: &NotebookTransformV2,
) -> Result<(), ContractValueError> {
    input.validate()?;
    output.validate()?;
    let input_hash = input.content_hash()?;
    let (source_input_hash, ticker, output_mode) = match output {
        NotebookTransformV2::NotebookMarkdown {
            source_input_hash,
            ticker,
            update_mode,
            ..
        } => (source_input_hash, ticker, update_mode.as_update_mode()),
        NotebookTransformV2::OpenQuestions {
            source_input_hash,
            ticker,
            update_mode,
            ..
        } => (source_input_hash, ticker, *update_mode),
    };
    if source_input_hash != input_hash.as_str()
        || ticker != &input.ticker
        || output_mode != input.update_mode
    {
        return Err(ContractValueError::Semantic(NOTEBOOK_TRANSFORM_V2));
    }
    Ok(())
}

const NOTEBOOK_HEADINGS: [&str; 4] = [
    "내가 보고 있는 이유",
    "긍정 논리",
    "반대 논리",
    "생각이 바뀔 수 있는 신호",
];

fn validate_notebook_markdown(markdown: &str) -> Result<(), ContractValueError> {
    if !bounded_text(markdown, 1, 100_000)
        || markdown.contains('\r')
        || markdown.contains("```")
        || markdown.contains("~~~")
    {
        return Err(ContractValueError::Shape(NOTEBOOK_TRANSFORM_V2));
    }
    let lines = markdown.lines().collect::<Vec<_>>();
    if lines.len() > 512 || lines.iter().any(|line| line.len() > 2_000) {
        return Err(ContractValueError::Limit(NOTEBOOK_TRANSFORM_V2));
    }
    let headings = lines
        .iter()
        .filter_map(|line| line.strip_prefix("## "))
        .collect::<Vec<_>>();
    if headings != NOTEBOOK_HEADINGS
        || lines.iter().any(|line| {
            line.starts_with("# ")
                || (line.starts_with('#') && !line.starts_with("## "))
                || matches!(
                    line.trim(),
                    "## 다음에 볼 것"
                        | "## 확인할 질문"
                        | "## 최근 업데이트"
                        | "## 새로 확인한 점"
                        | "## 현재 생각"
                )
                || is_notebook_metadata_line(line)
        })
    {
        return Err(ContractValueError::Semantic(NOTEBOOK_TRANSFORM_V2));
    }

    let mut section_content = [false; 4];
    let mut section_index = None;
    let mut bullet_count = 0_usize;
    let mut numeric_count = 0_usize;
    let mut table_rows = 0_usize;
    let mut table_blocks = 0_usize;
    let mut previous_table = false;
    for line in &lines {
        if let Some(heading) = line.strip_prefix("## ") {
            section_index = NOTEBOOK_HEADINGS
                .iter()
                .position(|expected| *expected == heading);
            previous_table = false;
            continue;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            previous_table = false;
            continue;
        }
        let Some(index) = section_index else {
            return Err(ContractValueError::Semantic(NOTEBOOK_TRANSFORM_V2));
        };
        section_content[index] = true;
        let number_tokens = count_numeric_tokens(trimmed);
        numeric_count = numeric_count.saturating_add(number_tokens);
        if trimmed.starts_with("- ") || trimmed.starts_with("* ") {
            bullet_count += 1;
            if number_tokens > 1 {
                return Err(ContractValueError::Semantic(NOTEBOOK_TRANSFORM_V2));
            }
        }
        let table_line = trimmed.contains('|');
        if table_line {
            table_rows += 1;
            if !previous_table {
                table_blocks += 1;
            }
        }
        previous_table = table_line;
    }
    if section_content.contains(&false)
        || bullet_count > 80
        || numeric_count > 16
        || table_rows > 16
        || table_blocks > 2
    {
        return Err(ContractValueError::Limit(NOTEBOOK_TRANSFORM_V2));
    }
    Ok(())
}

fn is_notebook_metadata_line(line: &str) -> bool {
    let trimmed = line.trim().trim_matches('*').trim();
    ["기준 분기", "기준 공시", "기준 기간", "마지막 업데이트"]
        .iter()
        .any(|label| {
            trimmed
                .strip_prefix(label)
                .is_some_and(|rest| rest.trim_start().starts_with([':', '：']))
        })
}

fn count_numeric_tokens(value: &str) -> usize {
    let mut in_number = false;
    let mut count = 0_usize;
    for character in value.chars() {
        if character.is_ascii_digit() {
            if !in_number {
                count += 1;
                in_number = true;
            }
        } else if !matches!(character, '.' | ',' | '-' | '/' | '%') {
            in_number = false;
        }
    }
    count
}

fn validate_open_questions(questions: &[String]) -> Result<(), ContractValueError> {
    if questions.len() > 5 {
        return Err(ContractValueError::Limit(NOTEBOOK_TRANSFORM_V2));
    }
    let mut normalized = BTreeSet::new();
    for question in questions {
        let trimmed = question.trim();
        let key = trimmed
            .trim_end_matches(['?', '？', '.', '!', '。'])
            .to_lowercase();
        if !bounded_text(trimmed, 2, 1_200)
            || trimmed.contains('\n')
            || !(trimmed.ends_with('?') || trimmed.ends_with('？'))
            || !normalized.insert(key)
            || [
                "매수",
                "매도",
                "목표가",
                "사도 돼",
                "팔아",
                "buy",
                "sell",
                "target price",
            ]
            .iter()
            .any(|term| trimmed.to_lowercase().contains(term))
        {
            return Err(ContractValueError::Semantic(NOTEBOOK_TRANSFORM_V2));
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CanonicalUnitType {
    Heading,
    Paragraph,
    Metric,
    PeriodDelta,
    BusinessSegment,
    Premise,
    ImpactChannel,
    RiskItem,
    DriverItem,
    HeadwindItem,
    WatchItem,
    TimelineEvent,
    ComparisonRow,
    Caveat,
    GlossaryTerm,
    EvidenceNote,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnitImportance {
    Required,
    Supporting,
    Optional,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnitConfidence {
    Direct,
    Indirect,
    Inferred,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanonicalDisplayUnitV1 {
    pub unit_id: String,
    pub unit_type: CanonicalUnitType,
    pub importance: UnitImportance,
    pub confidence: UnitConfidence,
    pub summary: String,
    pub content_hash: String,
    pub claim_ids: Vec<String>,
    pub evidence_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanonicalDisplaySourceV1 {
    pub schema_version: u16,
    pub final_receipt_hash: String,
    pub answer_bundle_hash: String,
    pub answer_ir_hash: String,
    pub locale: ResponseLocale,
    pub default_order: Vec<String>,
    pub units: Vec<CanonicalDisplayUnitV1>,
}

impl CanonicalDisplaySourceV1 {
    pub fn validate(&self) -> Result<(), ContractValueError> {
        if self.schema_version != 1
            || !valid_hash(&self.final_receipt_hash)
            || !valid_hash(&self.answer_bundle_hash)
            || !valid_hash(&self.answer_ir_hash)
            || self.units.is_empty()
            || self.units.len() > 64
            || self.default_order.len() != self.units.len()
        {
            return Err(ContractValueError::Shape(CANONICAL_DISPLAY_SOURCE_V1));
        }
        let mut units = BTreeMap::new();
        for unit in &self.units {
            if !valid_id(&unit.unit_id)
                || units.insert(unit.unit_id.as_str(), unit).is_some()
                || !bounded_text(&unit.summary, 1, 1_000)
                || !valid_hash(&unit.content_hash)
                || !unique_bounded_ids(&unit.claim_ids, 64)
                || !unique_bounded_ids(&unit.evidence_ids, 64)
            {
                return Err(ContractValueError::Shape(CANONICAL_DISPLAY_SOURCE_V1));
            }
        }
        let order = self.default_order.iter().collect::<BTreeSet<_>>();
        if order.len() != self.default_order.len()
            || order.iter().any(|id| !units.contains_key(id.as_str()))
        {
            return Err(ContractValueError::Semantic(CANONICAL_DISPLAY_SOURCE_V1));
        }
        Ok(())
    }

    pub fn content_hash(&self) -> Result<ContentHash, ContractValueError> {
        self.validate()?;
        canonical_hash(self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisplayBlockType {
    Markdown,
    MetricGrid,
    PeriodDelta,
    BusinessSegments,
    PremiseCard,
    ImpactChannels,
    RiskDriverMap,
    Timeline,
    ComparisonTable,
    EvidenceChain,
    CaveatList,
    Glossary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisplayDensity {
    Compact,
    Comfortable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisplayEmphasis {
    Normal,
    High,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DisplayPlanBlockV2 {
    pub block_id: String,
    pub block_type: DisplayBlockType,
    pub source_unit_ids: Vec<String>,
    pub density: DisplayDensity,
    pub emphasis: DisplayEmphasis,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DisplayPlanV2 {
    pub schema_version: u16,
    pub source_hash: String,
    pub blocks: Vec<DisplayPlanBlockV2>,
}

impl DisplayPlanV2 {
    pub fn validate(&self) -> Result<(), ContractValueError> {
        if self.schema_version != 2
            || !valid_hash(&self.source_hash)
            || self.blocks.is_empty()
            || self.blocks.len() > 16
        {
            return Err(ContractValueError::Shape(DISPLAY_PLAN_V2));
        }
        let mut block_ids = BTreeSet::new();
        for block in &self.blocks {
            if !valid_id(&block.block_id)
                || !block_ids.insert(&block.block_id)
                || block.source_unit_ids.is_empty()
                || !unique_bounded_ids(&block.source_unit_ids, 64)
            {
                return Err(ContractValueError::Shape(DISPLAY_PLAN_V2));
            }
        }
        Ok(())
    }
}

pub fn validate_display_plan_linkage(
    source: &CanonicalDisplaySourceV1,
    plan: &DisplayPlanV2,
) -> Result<(), ContractValueError> {
    source.validate()?;
    plan.validate()?;
    if plan.source_hash != source.content_hash()?.as_str() {
        return Err(ContractValueError::Semantic(DISPLAY_PLAN_V2));
    }
    let units = source
        .units
        .iter()
        .map(|unit| (unit.unit_id.as_str(), unit))
        .collect::<BTreeMap<_, _>>();
    let positions = source
        .default_order
        .iter()
        .enumerate()
        .map(|(index, id)| (id.as_str(), index))
        .collect::<BTreeMap<_, _>>();
    let mut selected = BTreeSet::new();
    let mut prior_position = None;
    for block in &plan.blocks {
        for unit_id in &block.source_unit_ids {
            let Some(unit) = units.get(unit_id.as_str()) else {
                return Err(ContractValueError::Semantic(DISPLAY_PLAN_V2));
            };
            if !selected.insert(unit_id.as_str())
                || !block_type_accepts(block.block_type, unit.unit_type)
            {
                return Err(ContractValueError::Semantic(DISPLAY_PLAN_V2));
            }
            let position = positions[unit_id.as_str()];
            if prior_position.is_some_and(|prior| position <= prior) {
                return Err(ContractValueError::Semantic(DISPLAY_PLAN_V2));
            }
            prior_position = Some(position);
        }
    }
    if source.units.iter().any(|unit| {
        unit.importance == UnitImportance::Required && !selected.contains(unit.unit_id.as_str())
    }) {
        return Err(ContractValueError::Semantic(DISPLAY_PLAN_V2));
    }
    Ok(())
}

const fn block_type_accepts(block: DisplayBlockType, unit: CanonicalUnitType) -> bool {
    match block {
        DisplayBlockType::Markdown => true,
        DisplayBlockType::MetricGrid => matches!(unit, CanonicalUnitType::Metric),
        DisplayBlockType::PeriodDelta => matches!(unit, CanonicalUnitType::PeriodDelta),
        DisplayBlockType::BusinessSegments => {
            matches!(unit, CanonicalUnitType::BusinessSegment)
        }
        DisplayBlockType::PremiseCard => matches!(unit, CanonicalUnitType::Premise),
        DisplayBlockType::ImpactChannels => matches!(unit, CanonicalUnitType::ImpactChannel),
        DisplayBlockType::RiskDriverMap => matches!(
            unit,
            CanonicalUnitType::RiskItem
                | CanonicalUnitType::DriverItem
                | CanonicalUnitType::HeadwindItem
                | CanonicalUnitType::WatchItem
        ),
        DisplayBlockType::Timeline => matches!(unit, CanonicalUnitType::TimelineEvent),
        DisplayBlockType::ComparisonTable => matches!(unit, CanonicalUnitType::ComparisonRow),
        DisplayBlockType::EvidenceChain => matches!(unit, CanonicalUnitType::EvidenceNote),
        DisplayBlockType::CaveatList => matches!(unit, CanonicalUnitType::Caveat),
        DisplayBlockType::Glossary => matches!(unit, CanonicalUnitType::GlossaryTerm),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash(label: &str) -> String {
        ContentHash::sha256(label).to_string()
    }

    #[test]
    fn routing_decision_is_input_bound_and_analysis_mode_is_derived() {
        let request = RoutingRequestV1 {
            schema_version: 1,
            question: "오늘 주가가 왜 빠졌어?".into(),
            current_analysis_mode: AnalysisMode::Company,
            current_run_kind: ProductRunKind::CompanyResearch,
            trusted_tickers: vec!["AAPL".into()],
            scope_type: ChatScopeType::Company,
            has_user_urls: false,
            response_locale: ResponseLocale::KoKr,
            session_memory_ref: None,
        };
        let decision = RoutingDecisionV2 {
            schema_version: 2,
            input_hash: request.content_hash().unwrap().to_string(),
            run_kind: ProductRunKind::MarketMoveResearch,
            analysis_mode: AnalysisMode::News,
            origin: RouteOrigin::Deterministic,
            confidence: RouteConfidence::High,
            reason_code: RouteReasonCode::MarketPriceMove,
        };
        validate_routing_linkage(&request, &decision).unwrap();
        let mut tampered = decision;
        tampered.analysis_mode = AnalysisMode::Company;
        assert!(validate_routing_linkage(&request, &tampered).is_err());
    }

    #[test]
    fn notebook_output_requires_exact_sections_and_input_linkage() {
        let input = NotebookTransformInputV1 {
            schema_version: 1,
            ticker: "AAPL".into(),
            company_name: "Apple".into(),
            basis_period: Some("FY2026 Q2".into()),
            update_mode: NotebookUpdateMode::MergeResearchAnswer,
            existing_notebook_md: "## 내가 보고 있는 이유\n- 기존 메모".into(),
            recent_conversations: vec![],
        };
        let output = NotebookTransformV2::NotebookMarkdown {
            schema_version: 2,
            source_input_hash: input.content_hash().unwrap().to_string(),
            ticker: "AAPL".into(),
            update_mode: NotebookWriteMode::MergeResearchAnswer,
            markdown: NOTEBOOK_HEADINGS
                .iter()
                .map(|heading| format!("## {heading}\n- 아직 확인할 핵심"))
                .collect::<Vec<_>>()
                .join("\n\n"),
        };
        validate_notebook_linkage(&input, &output).unwrap();
        let invalid = NotebookTransformV2::NotebookMarkdown {
            schema_version: 2,
            source_input_hash: hash("wrong"),
            ticker: "AAPL".into(),
            update_mode: NotebookWriteMode::MergeResearchAnswer,
            markdown: "## 현재 생각\n- 임의 섹션".into(),
        };
        assert!(validate_notebook_linkage(&input, &invalid).is_err());
    }

    #[test]
    fn display_plan_cannot_invent_reorder_or_omit_required_units() {
        let source = CanonicalDisplaySourceV1 {
            schema_version: 1,
            final_receipt_hash: hash("final-receipt"),
            answer_bundle_hash: hash("bundle"),
            answer_ir_hash: hash("answer"),
            locale: ResponseLocale::KoKr,
            default_order: vec!["u1".into(), "u2".into()],
            units: vec![
                CanonicalDisplayUnitV1 {
                    unit_id: "u1".into(),
                    unit_type: CanonicalUnitType::Paragraph,
                    importance: UnitImportance::Required,
                    confidence: UnitConfidence::Direct,
                    summary: "핵심 결론".into(),
                    content_hash: hash("u1"),
                    claim_ids: vec!["c1".into()],
                    evidence_ids: vec!["e1".into()],
                },
                CanonicalDisplayUnitV1 {
                    unit_id: "u2".into(),
                    unit_type: CanonicalUnitType::Metric,
                    importance: UnitImportance::Supporting,
                    confidence: UnitConfidence::Direct,
                    summary: "핵심 수치".into(),
                    content_hash: hash("u2"),
                    claim_ids: vec!["c2".into()],
                    evidence_ids: vec!["e2".into()],
                },
            ],
        };
        let plan = DisplayPlanV2 {
            schema_version: 2,
            source_hash: source.content_hash().unwrap().to_string(),
            blocks: vec![
                DisplayPlanBlockV2 {
                    block_id: "b1".into(),
                    block_type: DisplayBlockType::Markdown,
                    source_unit_ids: vec!["u1".into()],
                    density: DisplayDensity::Comfortable,
                    emphasis: DisplayEmphasis::High,
                },
                DisplayPlanBlockV2 {
                    block_id: "b2".into(),
                    block_type: DisplayBlockType::MetricGrid,
                    source_unit_ids: vec!["u2".into()],
                    density: DisplayDensity::Compact,
                    emphasis: DisplayEmphasis::Normal,
                },
            ],
        };
        validate_display_plan_linkage(&source, &plan).unwrap();

        let mut invented = plan.clone();
        invented.blocks[1].source_unit_ids = vec!["invented".into()];
        assert!(validate_display_plan_linkage(&source, &invented).is_err());

        let mut reordered = plan;
        reordered.blocks.swap(0, 1);
        assert!(validate_display_plan_linkage(&source, &reordered).is_err());
    }
}
