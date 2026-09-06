//! Read-only capability contracts owned by `krw-ontology-front`.
//!
//! Unlike the ontology contracts, these artifacts are not Python/Pydantic
//! exports. They are an audited, source-hash-pinned snapshot of the actual
//! TypeScript/Zod MCP inputs and explicit serializer/return types. The front
//! repository remains authoritative; source drift requires a fresh export.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Deserializer};
use serde_json::Value;

use super::{ContractArtifactError, ContractDescriptor, ContractValueError};

include!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../contracts/krw-front/v1/bindings.rs"
));

macro_rules! schema_bytes {
    ($name:ident, $file:literal) => {
        const $name: &[u8] = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../contracts/krw-front/v1/schemas/",
            $file
        ));
    };
}

schema_bytes!(FEED_CONTEXT_INPUT_BYTES, "krw-feed-context-input-v2.json");
schema_bytes!(FEED_CONTEXT_BYTES, "krw-feed-context-v2.json");
schema_bytes!(FEED_GET_INPUT_BYTES, "krw-feed-get-items-input-v1.json");
schema_bytes!(FEED_GET_RESULT_BYTES, "krw-feed-get-items-result-v1.json");
schema_bytes!(FEED_LIST_INPUT_BYTES, "krw-feed-list-items-input-v1.json");
schema_bytes!(FEED_LIST_RESULT_BYTES, "krw-feed-list-items-result-v1.json");
schema_bytes!(FILING_BRIEF_INPUT_BYTES, "krw-filing-brief-input-v1.json");
schema_bytes!(FILING_BRIEF_RESULT_BYTES, "krw-filing-brief-result-v1.json");
schema_bytes!(
    FILING_DOCUMENTS_INPUT_BYTES,
    "krw-filing-documents-input-v1.json"
);
schema_bytes!(
    FILING_DOCUMENTS_RESULT_BYTES,
    "krw-filing-documents-result-v1.json"
);
schema_bytes!(FILING_GET_INPUT_BYTES, "krw-filing-get-input-v1.json");
schema_bytes!(FILING_METADATA_BYTES, "krw-filing-metadata-v1.json");
schema_bytes!(
    FILING_READ_DOCUMENT_INPUT_BYTES,
    "krw-filing-read-document-input-v1.json"
);
schema_bytes!(
    FILING_READ_DOCUMENT_RESULT_BYTES,
    "krw-filing-read-document-result-v1.json"
);
schema_bytes!(
    FILING_READ_SECTION_INPUT_BYTES,
    "krw-filing-read-section-input-v1.json"
);
schema_bytes!(
    FILING_READ_SECTION_RESULT_BYTES,
    "krw-filing-read-section-result-v1.json"
);
schema_bytes!(FILING_SEARCH_INPUT_BYTES, "krw-filing-search-input-v1.json");
schema_bytes!(
    FILING_SEARCH_RESULT_BYTES,
    "krw-filing-search-result-v1.json"
);
schema_bytes!(
    FILING_SECTIONS_INPUT_BYTES,
    "krw-filing-sections-input-v1.json"
);
schema_bytes!(
    FILING_SECTIONS_RESULT_BYTES,
    "krw-filing-sections-result-v1.json"
);
schema_bytes!(FORM4_INPUT_BYTES, "krw-form4-transactions-input-v1.json");
schema_bytes!(FORM4_RESULT_BYTES, "krw-form4-transactions-result-v1.json");

const FRONT_MANIFEST_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../contracts/krw-front/v1/manifest.json"
));
const FRONT_BUNDLE_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../contracts/krw-front/v1/schema-bundle.json"
));
const FRONT_VECTOR_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../contracts/krw-front/v1/conformance-vectors.json"
));

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
        KRW_FEED_CONTEXT_INPUT_V2 => descriptor!(
            KRW_FEED_CONTEXT_INPUT_V2,
            KRW_FEED_CONTEXT_INPUT_V2_SCHEMA_SHA256,
            FEED_CONTEXT_INPUT_BYTES
        ),
        KRW_FEED_CONTEXT_V2 => descriptor!(
            KRW_FEED_CONTEXT_V2,
            KRW_FEED_CONTEXT_V2_SCHEMA_SHA256,
            FEED_CONTEXT_BYTES
        ),
        KRW_FEED_GET_ITEMS_INPUT_V1 => descriptor!(
            KRW_FEED_GET_ITEMS_INPUT_V1,
            KRW_FEED_GET_ITEMS_INPUT_V1_SCHEMA_SHA256,
            FEED_GET_INPUT_BYTES
        ),
        KRW_FEED_GET_ITEMS_RESULT_V1 => descriptor!(
            KRW_FEED_GET_ITEMS_RESULT_V1,
            KRW_FEED_GET_ITEMS_RESULT_V1_SCHEMA_SHA256,
            FEED_GET_RESULT_BYTES
        ),
        KRW_FEED_LIST_ITEMS_INPUT_V1 => descriptor!(
            KRW_FEED_LIST_ITEMS_INPUT_V1,
            KRW_FEED_LIST_ITEMS_INPUT_V1_SCHEMA_SHA256,
            FEED_LIST_INPUT_BYTES
        ),
        KRW_FEED_LIST_ITEMS_RESULT_V1 => descriptor!(
            KRW_FEED_LIST_ITEMS_RESULT_V1,
            KRW_FEED_LIST_ITEMS_RESULT_V1_SCHEMA_SHA256,
            FEED_LIST_RESULT_BYTES
        ),
        KRW_FILING_BRIEF_INPUT_V1 => descriptor!(
            KRW_FILING_BRIEF_INPUT_V1,
            KRW_FILING_BRIEF_INPUT_V1_SCHEMA_SHA256,
            FILING_BRIEF_INPUT_BYTES
        ),
        KRW_FILING_BRIEF_RESULT_V1 => descriptor!(
            KRW_FILING_BRIEF_RESULT_V1,
            KRW_FILING_BRIEF_RESULT_V1_SCHEMA_SHA256,
            FILING_BRIEF_RESULT_BYTES
        ),
        KRW_FILING_DOCUMENTS_INPUT_V1 => descriptor!(
            KRW_FILING_DOCUMENTS_INPUT_V1,
            KRW_FILING_DOCUMENTS_INPUT_V1_SCHEMA_SHA256,
            FILING_DOCUMENTS_INPUT_BYTES
        ),
        KRW_FILING_DOCUMENTS_RESULT_V1 => descriptor!(
            KRW_FILING_DOCUMENTS_RESULT_V1,
            KRW_FILING_DOCUMENTS_RESULT_V1_SCHEMA_SHA256,
            FILING_DOCUMENTS_RESULT_BYTES
        ),
        KRW_FILING_GET_INPUT_V1 => descriptor!(
            KRW_FILING_GET_INPUT_V1,
            KRW_FILING_GET_INPUT_V1_SCHEMA_SHA256,
            FILING_GET_INPUT_BYTES
        ),
        KRW_FILING_METADATA_V1 => descriptor!(
            KRW_FILING_METADATA_V1,
            KRW_FILING_METADATA_V1_SCHEMA_SHA256,
            FILING_METADATA_BYTES
        ),
        KRW_FILING_READ_DOCUMENT_INPUT_V1 => descriptor!(
            KRW_FILING_READ_DOCUMENT_INPUT_V1,
            KRW_FILING_READ_DOCUMENT_INPUT_V1_SCHEMA_SHA256,
            FILING_READ_DOCUMENT_INPUT_BYTES
        ),
        KRW_FILING_READ_DOCUMENT_RESULT_V1 => descriptor!(
            KRW_FILING_READ_DOCUMENT_RESULT_V1,
            KRW_FILING_READ_DOCUMENT_RESULT_V1_SCHEMA_SHA256,
            FILING_READ_DOCUMENT_RESULT_BYTES
        ),
        KRW_FILING_READ_SECTION_INPUT_V1 => descriptor!(
            KRW_FILING_READ_SECTION_INPUT_V1,
            KRW_FILING_READ_SECTION_INPUT_V1_SCHEMA_SHA256,
            FILING_READ_SECTION_INPUT_BYTES
        ),
        KRW_FILING_READ_SECTION_RESULT_V1 => descriptor!(
            KRW_FILING_READ_SECTION_RESULT_V1,
            KRW_FILING_READ_SECTION_RESULT_V1_SCHEMA_SHA256,
            FILING_READ_SECTION_RESULT_BYTES
        ),
        KRW_FILING_SEARCH_INPUT_V1 => descriptor!(
            KRW_FILING_SEARCH_INPUT_V1,
            KRW_FILING_SEARCH_INPUT_V1_SCHEMA_SHA256,
            FILING_SEARCH_INPUT_BYTES
        ),
        KRW_FILING_SEARCH_RESULT_V1 => descriptor!(
            KRW_FILING_SEARCH_RESULT_V1,
            KRW_FILING_SEARCH_RESULT_V1_SCHEMA_SHA256,
            FILING_SEARCH_RESULT_BYTES
        ),
        KRW_FILING_SECTIONS_INPUT_V1 => descriptor!(
            KRW_FILING_SECTIONS_INPUT_V1,
            KRW_FILING_SECTIONS_INPUT_V1_SCHEMA_SHA256,
            FILING_SECTIONS_INPUT_BYTES
        ),
        KRW_FILING_SECTIONS_RESULT_V1 => descriptor!(
            KRW_FILING_SECTIONS_RESULT_V1,
            KRW_FILING_SECTIONS_RESULT_V1_SCHEMA_SHA256,
            FILING_SECTIONS_RESULT_BYTES
        ),
        KRW_FORM4_TRANSACTIONS_INPUT_V1 => descriptor!(
            KRW_FORM4_TRANSACTIONS_INPUT_V1,
            KRW_FORM4_TRANSACTIONS_INPUT_V1_SCHEMA_SHA256,
            FORM4_INPUT_BYTES
        ),
        KRW_FORM4_TRANSACTIONS_RESULT_V1 => descriptor!(
            KRW_FORM4_TRANSACTIONS_RESULT_V1,
            KRW_FORM4_TRANSACTIONS_RESULT_V1_SCHEMA_SHA256,
            FORM4_RESULT_BYTES
        ),
        _ => return None,
    })
}

pub(super) fn descriptors() -> [ContractDescriptor; 22] {
    [
        contract(KRW_FEED_CONTEXT_INPUT_V2).expect("static contract"),
        contract(KRW_FEED_CONTEXT_V2).expect("static contract"),
        contract(KRW_FEED_GET_ITEMS_INPUT_V1).expect("static contract"),
        contract(KRW_FEED_GET_ITEMS_RESULT_V1).expect("static contract"),
        contract(KRW_FEED_LIST_ITEMS_INPUT_V1).expect("static contract"),
        contract(KRW_FEED_LIST_ITEMS_RESULT_V1).expect("static contract"),
        contract(KRW_FILING_BRIEF_INPUT_V1).expect("static contract"),
        contract(KRW_FILING_BRIEF_RESULT_V1).expect("static contract"),
        contract(KRW_FILING_DOCUMENTS_INPUT_V1).expect("static contract"),
        contract(KRW_FILING_DOCUMENTS_RESULT_V1).expect("static contract"),
        contract(KRW_FILING_GET_INPUT_V1).expect("static contract"),
        contract(KRW_FILING_METADATA_V1).expect("static contract"),
        contract(KRW_FILING_READ_DOCUMENT_INPUT_V1).expect("static contract"),
        contract(KRW_FILING_READ_DOCUMENT_RESULT_V1).expect("static contract"),
        contract(KRW_FILING_READ_SECTION_INPUT_V1).expect("static contract"),
        contract(KRW_FILING_READ_SECTION_RESULT_V1).expect("static contract"),
        contract(KRW_FILING_SEARCH_INPUT_V1).expect("static contract"),
        contract(KRW_FILING_SEARCH_RESULT_V1).expect("static contract"),
        contract(KRW_FILING_SECTIONS_INPUT_V1).expect("static contract"),
        contract(KRW_FILING_SECTIONS_RESULT_V1).expect("static contract"),
        contract(KRW_FORM4_TRANSACTIONS_INPUT_V1).expect("static contract"),
        contract(KRW_FORM4_TRANSACTIONS_RESULT_V1).expect("static contract"),
    ]
}

/// Required JSON `null | T`; unlike `Option<T>`, a missing property fails.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequiredNullable<T>(pub Option<T>);

impl<'de, T> Deserialize<'de> for RequiredNullable<T>
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

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FeedGetItemsInput {
    pub issue_ids: Vec<String>,
    pub include_sources: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FeedListItemsInput {
    pub tickers: Option<Vec<String>>,
    pub published_after: Option<String>,
    pub limit: Option<u8>,
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FeedContextInput {
    pub issue_ids: Vec<String>,
    pub tickers: Option<Vec<String>>,
    pub max_posts_per_issue: Option<u8>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FeedEntity {
    pub ticker: RequiredNullable<String>,
    pub name: String,
    pub relationship: String,
    pub relationship_reason: String,
    pub confidence: f64,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FeedPost {
    pub id: String,
    pub source_type: String,
    pub external_item_id: String,
    pub x_post_id: String,
    pub filing_event_id: RequiredNullable<String>,
    pub source_class: String,
    pub publisher: String,
    pub author_handle: RequiredNullable<String>,
    pub canonical_url: String,
    pub original_text: String,
    pub published_at: String,
    pub public_metrics: BTreeMap<String, Value>,
    pub source_metadata: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FeedIssue {
    pub id: String,
    pub event_key: String,
    pub title_ko: String,
    pub summary_ko: String,
    pub impact_path: Vec<String>,
    pub confirmed_facts: Vec<String>,
    pub unconfirmed_facts: Vec<String>,
    pub status_labels: Vec<String>,
    pub source_summary: BTreeMap<String, Value>,
    pub source_count: u64,
    pub market_score: f64,
    pub risk_level: String,
    pub first_observed_at: RequiredNullable<String>,
    pub published_at: RequiredNullable<String>,
    pub updated_at: RequiredNullable<String>,
    pub entities: Vec<FeedEntity>,
    pub source_posts: Vec<FeedPost>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FeedGetItemsResult {
    pub environment: String,
    pub items: Vec<FeedIssue>,
    pub missing_issue_ids: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FeedPagination {
    pub limit: u8,
    pub next_cursor: RequiredNullable<String>,
    pub has_more: bool,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FeedListItemsResult {
    pub environment: String,
    pub items: Vec<FeedIssue>,
    pub pagination: FeedPagination,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FeedCompanyGroup {
    pub ticker: String,
    pub issue_ids: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FeedGap {
    pub issue_id: String,
    pub code: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FeedExtraction {
    pub method: RequiredNullable<String>,
    pub confidence: RequiredNullable<f64>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FeedMedia {
    pub id: String,
    pub ordinal: u32,
    pub media_type: String,
    pub media_url: RequiredNullable<String>,
    pub content_hash: RequiredNullable<String>,
    pub extraction_status: String,
    pub extraction_model: RequiredNullable<String>,
    pub extraction_confidence: RequiredNullable<f64>,
    pub extraction: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FeedSourceMaterial {
    pub issue_id: String,
    pub source_item_id: String,
    pub source_type: String,
    pub external_item_id: String,
    pub x_post_id: String,
    pub filing_event_id: RequiredNullable<String>,
    pub source_class: String,
    pub publisher: String,
    pub author_handle: RequiredNullable<String>,
    pub canonical_url: String,
    pub published_at: String,
    pub title: RequiredNullable<String>,
    pub content_kind: RequiredNullable<String>,
    pub signal_level: RequiredNullable<String>,
    pub extraction: FeedExtraction,
    pub source_metadata: BTreeMap<String, Value>,
    pub original_text: String,
    pub original_text_truncated: bool,
    pub original_text_omitted_due_to_budget: bool,
    pub original_text_hash: RequiredNullable<String>,
    pub public_metrics: BTreeMap<String, Value>,
    pub media: Vec<FeedMedia>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FeedResearchPacket {
    pub issue_id: String,
    pub id: String,
    pub version: u32,
    pub status: String,
    pub source_context_hash: String,
    pub generated_at: RequiredNullable<String>,
    pub packet: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FeedContextResult {
    pub version: String,
    pub environment: String,
    pub context_hash: String,
    pub requested_issue_ids: Vec<String>,
    pub items: Vec<FeedIssue>,
    pub tickers: Vec<String>,
    pub company_groups: Vec<FeedCompanyGroup>,
    pub gaps: Vec<FeedGap>,
    pub source_materials: Vec<FeedSourceMaterial>,
    pub research_packets: Vec<FeedResearchPacket>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FilingIdInput {
    pub filing_event_id: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FilingSearchInput {
    pub ticker: String,
    pub form_type: Option<String>,
    pub limit: Option<u8>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FilingReadSectionInput {
    pub filing_event_id: String,
    pub section_key: String,
    pub offset: Option<u32>,
    pub max_chars: Option<u32>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FilingReadDocumentInput {
    pub filing_event_id: String,
    pub document_key: String,
    pub offset: Option<u32>,
    pub max_chars: Option<u32>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FilingMetadata {
    pub filing_event_id: String,
    pub ticker: String,
    pub cik: String,
    pub accession_number: String,
    pub form_type: String,
    pub filing_date: String,
    pub report_date: RequiredNullable<String>,
    pub accepted_at: RequiredNullable<String>,
    pub sec_items: Vec<String>,
    pub event_tags: Vec<String>,
    pub filing_detail_url: String,
    pub primary_document_url: RequiredNullable<String>,
    pub enrichment_status: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FilingSection {
    pub key: String,
    pub kind: String,
    pub heading: String,
    #[serde(rename = "itemCode")]
    pub item_code: RequiredNullable<String>,
    pub confidence: String,
    pub text_length: u32,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FilingDocument {
    pub document_key: String,
    pub document_name: String,
    pub document_type: RequiredNullable<String>,
    pub description: RequiredNullable<String>,
    pub document_url: String,
    pub sequence: RequiredNullable<u32>,
    pub size_bytes: RequiredNullable<u64>,
    pub is_primary: bool,
    pub recommendation_score: u8,
    pub readable: bool,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FilingCitation {
    pub source: String,
    pub filing_event_id: String,
    pub accession_number: String,
    pub filing_detail_url: String,
    pub document_url: String,
    pub section_key: String,
    pub section_heading: String,
    pub extraction: String,
    pub fetched_at: String,
    pub document_key: Option<String>,
    pub document_name: Option<String>,
    pub document_type: Option<RequiredNullable<String>>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FilingBriefFact {
    pub fact_ko: String,
    pub section_key: String,
    pub source_quote: String,
    pub section_heading: String,
    pub source_document_url: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FilingBrief {
    pub filing_event_id: String,
    pub locale: String,
    pub headline_ko: String,
    pub summary_sentences: Vec<String>,
    pub facts: Vec<FilingBriefFact>,
    pub question_suggestions: Vec<String>,
    pub uncertainty_notes: Vec<String>,
    pub source_document_url: String,
    pub source_section_keys: Vec<String>,
    pub source_truncated: bool,
    pub generator_model: String,
    pub prompt_version: String,
    pub schema_version: u32,
    pub generated_at: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FilingBriefResult {
    pub filing: FilingMetadata,
    pub status: String,
    pub brief: RequiredNullable<FilingBrief>,
    pub machine_generated: bool,
    pub note: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FilingSectionsResult {
    pub filing: FilingMetadata,
    pub sections: Vec<FilingSection>,
    pub document_url: String,
    pub fetched_at: String,
    pub document_truncated: bool,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FilingReadSectionResult {
    pub filing: FilingMetadata,
    pub section: FilingSection,
    pub text: String,
    pub offset: u32,
    pub next_offset: RequiredNullable<u32>,
    pub document_truncated: bool,
    pub citation: FilingCitation,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FilingDocumentsResult {
    pub filing: FilingMetadata,
    pub documents: Vec<FilingDocument>,
    pub fetched_at: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FilingReadDocumentResult {
    pub filing: FilingMetadata,
    pub document: FilingDocument,
    pub text: String,
    pub offset: u32,
    pub next_offset: RequiredNullable<u32>,
    pub document_truncated: bool,
    pub citation: FilingCitation,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
#[allow(clippy::struct_excessive_bools)] // Exact SEC Form 4 ownership flags.
pub struct Form4Owner {
    pub filing_event_id: String,
    pub reporting_owner_index: u32,
    pub owner_cik: RequiredNullable<String>,
    pub owner_name: String,
    pub is_director: bool,
    pub is_officer: bool,
    pub is_ten_percent_owner: bool,
    pub is_other: bool,
    pub officer_title: RequiredNullable<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Form4Transaction {
    pub id: String,
    pub filing_event_id: String,
    pub reporting_owner_index: u32,
    pub transaction_index: u32,
    pub security_kind: String,
    pub security_title: String,
    pub transaction_date: RequiredNullable<String>,
    pub transaction_code: RequiredNullable<String>,
    pub equity_swap_involved: RequiredNullable<bool>,
    pub transaction_shares: RequiredNullable<f64>,
    pub transaction_price_per_share: RequiredNullable<f64>,
    pub acquired_disposed_code: RequiredNullable<String>,
    pub shares_owned_following: RequiredNullable<f64>,
    pub direct_indirect_code: RequiredNullable<String>,
    pub nature_of_ownership: RequiredNullable<String>,
    pub reported_transaction_value: RequiredNullable<f64>,
    pub reported_value_basis: String,
    pub footnotes: Value,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Form4Summary {
    pub filing_event_id: String,
    pub reporting_owner_count: u32,
    pub transaction_count: u32,
    pub reported_purchase_count: u32,
    pub reported_sale_count: u32,
    pub reported_purchase_shares: RequiredNullable<f64>,
    pub reported_sale_shares: RequiredNullable<f64>,
    pub reported_purchase_value: RequiredNullable<f64>,
    pub reported_sale_value: RequiredNullable<f64>,
    pub net_reported_shares: RequiredNullable<f64>,
    pub tenb5_one_status: String,
    pub has_reported_open_market_trade: bool,
    pub source_xml_version: RequiredNullable<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Form4TransactionsResult {
    pub filing: FilingMetadata,
    pub summary: RequiredNullable<Form4Summary>,
    pub reporting_owners: Vec<Form4Owner>,
    pub transactions: Vec<Form4Transaction>,
    pub source: String,
    pub citation: FilingCitation,
}

fn decode<'de, T>(value: &'de Value, contract_id: &'static str) -> Result<T, ContractValueError>
where
    T: Deserialize<'de>,
{
    T::deserialize(value).map_err(|_| ContractValueError::Shape(contract_id))
}

fn utf16_len(value: &str) -> usize {
    value.encode_utf16().count()
}

fn bounded_text(value: &str, minimum: usize, maximum: usize) -> bool {
    let length = utf16_len(value);
    (minimum..=maximum).contains(&length) && !value.contains('\0')
}

fn bounded_nullable_text(value: &RequiredNullable<String>, maximum: usize) -> bool {
    value
        .0
        .as_deref()
        .is_none_or(|value| bounded_text(value, 1, maximum))
}

fn valid_uuid(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 36
        && [8, 13, 18, 23]
            .into_iter()
            .all(|index| bytes[index] == b'-')
        && bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| [8, 13, 18, 23].contains(&index) || byte.is_ascii_hexdigit())
        && matches!(bytes[14], b'1'..=b'5')
        && matches!(bytes[19].to_ascii_lowercase(), b'8' | b'9' | b'a' | b'b')
}

fn valid_ticker_output(value: &str, maximum: usize) -> bool {
    bounded_text(value, 1, maximum)
        && value.bytes().all(|byte| {
            byte.is_ascii_uppercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'-')
        })
}

fn normalized_ticker(value: &str, maximum: usize) -> Option<String> {
    if !bounded_text(value, 1, maximum) {
        return None;
    }
    let normalized = value.trim().to_ascii_uppercase();
    valid_ticker_output(&normalized, maximum).then_some(normalized)
}

fn valid_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 10
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes
            .iter()
            .enumerate()
            .any(|(index, byte)| !matches!(index, 4 | 7) && !byte.is_ascii_digit())
    {
        return false;
    }
    let year = value[..4].parse::<u16>().ok();
    let month = value[5..7].parse::<u8>().ok();
    let day = value[8..10].parse::<u8>().ok();
    let (Some(year), Some(month), Some(day)) = (year, month, day) else {
        return false;
    };
    let maximum_day = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year.is_multiple_of(400) || (year.is_multiple_of(4) && !year.is_multiple_of(100)) => {
            29
        }
        2 => 28,
        _ => return false,
    };
    (1..=maximum_day).contains(&day)
}

fn valid_datetime(value: &str) -> bool {
    if !bounded_text(value, 1, 64) || value.len() < 20 || value.as_bytes().get(10) != Some(&b'T') {
        return false;
    }
    if !valid_date(&value[..10]) {
        return false;
    }
    let time_and_zone = &value[11..];
    let time = if let Some(time) = time_and_zone.strip_suffix('Z') {
        time
    } else {
        let Some(zone_index) = time_and_zone
            .char_indices()
            .rfind(|(index, character)| *index >= 5 && matches!(character, '+' | '-'))
            .map(|(index, _)| index)
        else {
            return false;
        };
        let zone = &time_and_zone[zone_index + 1..];
        let zone_bytes = zone.as_bytes();
        if zone_bytes.len() != 5
            || zone_bytes[2] != b':'
            || !zone_bytes
                .iter()
                .enumerate()
                .all(|(index, byte)| index == 2 || byte.is_ascii_digit())
            || zone[..2].parse::<u8>().ok().is_none_or(|hour| hour > 23)
            || zone[3..]
                .parse::<u8>()
                .ok()
                .is_none_or(|minute| minute > 59)
        {
            return false;
        }
        &time_and_zone[..zone_index]
    };
    let Some((hour, remainder)) = time.split_once(':') else {
        return false;
    };
    let Some((minute, second_and_fraction)) = remainder.split_once(':') else {
        return false;
    };
    if hour.len() != 2
        || minute.len() != 2
        || !hour.bytes().all(|byte| byte.is_ascii_digit())
        || !minute.bytes().all(|byte| byte.is_ascii_digit())
        || hour.parse::<u8>().ok().is_none_or(|hour| hour > 23)
        || minute.parse::<u8>().ok().is_none_or(|minute| minute > 59)
    {
        return false;
    }
    let (second, fraction) = second_and_fraction
        .split_once('.')
        .map_or((second_and_fraction, None), |(second, fraction)| {
            (second, Some(fraction))
        });
    second.len() == 2
        && second.bytes().all(|byte| byte.is_ascii_digit())
        && second.parse::<u8>().ok().is_some_and(|second| second <= 59)
        && fraction.is_none_or(|fraction| {
            (1..=12).contains(&fraction.len()) && fraction.bytes().all(|byte| byte.is_ascii_digit())
        })
}

fn valid_https_url(value: &str) -> bool {
    bounded_text(value, 8, 2_000)
        && value.starts_with("https://")
        && !value.bytes().any(|byte| byte.is_ascii_whitespace())
}

fn valid_environment(value: &str) -> bool {
    matches!(value, "dev" | "staging" | "prod")
}

fn valid_form_type(value: &str) -> bool {
    matches!(value, "8-K" | "8-K/A" | "6-K" | "6-K/A" | "4" | "4/A")
}

fn valid_hash(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("sha256:")
        && value.as_bytes()[7..].iter().all(u8::is_ascii_hexdigit)
        && value.as_bytes()[7..]
            .iter()
            .all(|byte| !byte.is_ascii_uppercase())
}

fn unique_strings<'a>(values: impl IntoIterator<Item = &'a String>) -> bool {
    let mut unique = BTreeSet::new();
    values
        .into_iter()
        .all(|value| unique.insert(value.as_str()))
}

fn bounded_map(map: &BTreeMap<String, Value>, maximum: usize) -> bool {
    map.len() <= maximum && map.keys().all(|key| bounded_text(key, 1, 256))
}

fn validate_front_bounds(
    contract_id: &'static str,
    value: &Value,
) -> Result<(), ContractValueError> {
    let maximum_bytes = match contract_id {
        KRW_FORM4_TRANSACTIONS_RESULT_V1 => 8 * 1024 * 1024,
        KRW_FEED_CONTEXT_V2 => 4 * 1024 * 1024,
        id if id.ends_with("-input/v1") || id == KRW_FEED_CONTEXT_INPUT_V2 => 64 * 1024,
        _ => 2 * 1024 * 1024,
    };
    let bytes = serde_jcs::to_vec(value).map_err(ContractValueError::Json)?;
    if bytes.len() > maximum_bytes {
        return Err(ContractValueError::Limit("front contract canonical bytes"));
    }
    let mut stack = vec![(value, 0_u8)];
    let mut visited = 0_usize;
    while let Some((current, depth)) = stack.pop() {
        visited = visited
            .checked_add(1)
            .ok_or(ContractValueError::Limit("front value items"))?;
        if visited > 131_072 || depth > 20 {
            return Err(ContractValueError::Limit("front value depth/items"));
        }
        match current {
            Value::String(text) if text.len() > 512 * 1024 || text.contains('\0') => {
                return Err(ContractValueError::Limit("front string bytes"));
            }
            Value::Array(values) => {
                if values.len() > 4_096 {
                    return Err(ContractValueError::Limit("front array items"));
                }
                stack.extend(values.iter().map(|value| (value, depth.saturating_add(1))));
            }
            Value::Object(values) => {
                if values.len() > 256
                    || values
                        .keys()
                        .any(|key| key.len() > 1_024 || key.contains('\0'))
                {
                    return Err(ContractValueError::Limit("front object properties"));
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

fn validate_feed_issue(issue: &FeedIssue) -> bool {
    valid_uuid(&issue.id)
        && bounded_text(&issue.event_key, 0, 256)
        && bounded_text(&issue.title_ko, 0, 240)
        && bounded_text(&issue.summary_ko, 0, 1_500)
        && issue.impact_path.len() <= 5
        && issue
            .impact_path
            .iter()
            .all(|value| bounded_text(value, 1, 360))
        && issue.confirmed_facts.len() <= 5
        && issue
            .confirmed_facts
            .iter()
            .all(|value| bounded_text(value, 1, 360))
        && issue.unconfirmed_facts.len() <= 5
        && issue
            .unconfirmed_facts
            .iter()
            .all(|value| bounded_text(value, 1, 360))
        && issue.status_labels.len() <= 3
        && issue
            .status_labels
            .iter()
            .all(|value| bounded_text(value, 1, 80))
        && bounded_map(&issue.source_summary, 64)
        && issue.source_count <= 1_000_000
        && issue.market_score.is_finite()
        && (-1_000_000_000.0..=1_000_000_000.0).contains(&issue.market_score)
        && bounded_text(&issue.risk_level, 0, 32)
        && issue
            .first_observed_at
            .0
            .as_deref()
            .is_none_or(valid_datetime)
        && issue.published_at.0.as_deref().is_none_or(valid_datetime)
        && issue.updated_at.0.as_deref().is_none_or(valid_datetime)
        && issue.entities.len() <= 64
        && issue.entities.iter().all(validate_feed_entity)
        && issue.source_posts.len() <= 3
        && issue.source_posts.iter().all(validate_feed_post)
}

fn validate_feed_entity(entity: &FeedEntity) -> bool {
    entity
        .ticker
        .0
        .as_deref()
        .is_none_or(|ticker| valid_ticker_output(ticker, 16))
        && bounded_text(&entity.name, 0, 120)
        && bounded_text(&entity.relationship, 0, 80)
        && bounded_text(&entity.relationship_reason, 0, 240)
        && entity.confidence.is_finite()
        && (0.0..=1.0).contains(&entity.confidence)
}

fn validate_feed_post(post: &FeedPost) -> bool {
    bounded_text(&post.id, 0, 128)
        && bounded_text(&post.source_type, 0, 64)
        && bounded_text(&post.external_item_id, 0, 256)
        && bounded_text(&post.x_post_id, 0, 128)
        && post.filing_event_id.0.as_deref().is_none_or(valid_uuid)
        && bounded_text(&post.source_class, 0, 64)
        && bounded_text(&post.publisher, 0, 120)
        && bounded_nullable_text(&post.author_handle, 120)
        && valid_https_url(&post.canonical_url)
        && bounded_text(&post.original_text, 0, 1_200)
        && valid_datetime(&post.published_at)
        && bounded_map(&post.public_metrics, 64)
        && bounded_map(&post.source_metadata, 64)
}

fn validate_feed_media(media: &FeedMedia) -> bool {
    bounded_text(&media.id, 0, 128)
        && media.ordinal <= 1_000
        && bounded_text(&media.media_type, 0, 64)
        && media.media_url.0.as_deref().is_none_or(valid_https_url)
        && bounded_nullable_text(&media.content_hash, 256)
        && bounded_text(&media.extraction_status, 0, 64)
        && bounded_nullable_text(&media.extraction_model, 160)
        && media
            .extraction_confidence
            .0
            .is_none_or(|confidence| confidence.is_finite() && (0.0..=1.0).contains(&confidence))
        && bounded_map(&media.extraction, 128)
}

fn validate_feed_material(material: &FeedSourceMaterial) -> bool {
    valid_uuid(&material.issue_id)
        && bounded_text(&material.source_item_id, 0, 128)
        && bounded_text(&material.source_type, 0, 64)
        && bounded_text(&material.external_item_id, 0, 256)
        && bounded_text(&material.x_post_id, 0, 128)
        && material.filing_event_id.0.as_deref().is_none_or(valid_uuid)
        && bounded_text(&material.source_class, 0, 64)
        && bounded_text(&material.publisher, 0, 120)
        && bounded_nullable_text(&material.author_handle, 120)
        && valid_https_url(&material.canonical_url)
        && valid_datetime(&material.published_at)
        && bounded_nullable_text(&material.title, 1_000)
        && bounded_nullable_text(&material.content_kind, 80)
        && bounded_nullable_text(&material.signal_level, 80)
        && bounded_nullable_text(&material.extraction.method, 160)
        && material
            .extraction
            .confidence
            .0
            .is_none_or(|confidence| confidence.is_finite() && (0.0..=1.0).contains(&confidence))
        && bounded_map(&material.source_metadata, 64)
        && bounded_text(&material.original_text, 0, 25_000)
        && (!material.original_text_omitted_due_to_budget || material.original_text.is_empty())
        && bounded_nullable_text(&material.original_text_hash, 256)
        && bounded_map(&material.public_metrics, 64)
        && material.media.len() <= 32
        && material.media.iter().all(validate_feed_media)
}

fn validate_feed_packet(packet: &FeedResearchPacket) -> bool {
    valid_uuid(&packet.issue_id)
        && bounded_text(&packet.id, 0, 128)
        && packet.version <= 1_000_000
        && bounded_text(&packet.status, 0, 64)
        && bounded_text(&packet.source_context_hash, 0, 256)
        && packet.generated_at.0.as_deref().is_none_or(valid_datetime)
        && bounded_map(&packet.packet, 128)
}

fn validate_get_feed_input(input: &FeedGetItemsInput) -> bool {
    (1..=8).contains(&input.issue_ids.len()) && input.issue_ids.iter().all(|id| valid_uuid(id))
}

fn validate_list_feed_input(input: &FeedListItemsInput) -> bool {
    input.tickers.as_ref().is_none_or(|tickers| {
        tickers.len() <= 5
            && tickers
                .iter()
                .all(|ticker| normalized_ticker(ticker, 16).is_some())
    }) && input.published_after.as_deref().is_none_or(valid_datetime)
        && input.limit.is_none_or(|limit| (1..=20).contains(&limit))
        && input.cursor.as_deref().is_none_or(|cursor| {
            (1..=6).contains(&cursor.len()) && cursor.bytes().all(|byte| byte.is_ascii_digit())
        })
}

fn validate_context_input(input: &FeedContextInput) -> bool {
    (1..=8).contains(&input.issue_ids.len())
        && input.issue_ids.iter().all(|id| valid_uuid(id))
        && input.tickers.as_ref().is_none_or(|tickers| {
            tickers.len() <= 5
                && tickers
                    .iter()
                    .all(|ticker| normalized_ticker(ticker, 16).is_some())
        })
        && input
            .max_posts_per_issue
            .is_none_or(|maximum| (1..=3).contains(&maximum))
}

fn validate_get_feed_result(result: &FeedGetItemsResult) -> bool {
    valid_environment(&result.environment)
        && result.items.len() <= 8
        && result.items.iter().all(validate_feed_issue)
        && unique_strings(result.items.iter().map(|item| &item.id))
        && result.missing_issue_ids.len() <= 8
        && result.missing_issue_ids.iter().all(|id| valid_uuid(id))
        && unique_strings(&result.missing_issue_ids)
        && result
            .items
            .iter()
            .all(|item| !result.missing_issue_ids.contains(&item.id))
}

fn validate_list_feed_result(result: &FeedListItemsResult) -> bool {
    valid_environment(&result.environment)
        && result.items.len() <= 20
        && result.items.iter().all(validate_feed_issue)
        && result.items.iter().all(|item| item.source_posts.is_empty())
        && unique_strings(result.items.iter().map(|item| &item.id))
        && (1..=20).contains(&result.pagination.limit)
        && result
            .pagination
            .next_cursor
            .0
            .as_deref()
            .is_none_or(|cursor| {
                (1..=6).contains(&cursor.len()) && cursor.bytes().all(|byte| byte.is_ascii_digit())
            })
        && (result.pagination.has_more == result.pagination.next_cursor.0.is_some())
}

fn validate_context_result(result: &FeedContextResult) -> bool {
    if result.version != KRW_FEED_CONTEXT_V2
        || !valid_environment(&result.environment)
        || !valid_hash(&result.context_hash)
        || !(1..=8).contains(&result.requested_issue_ids.len())
        || !result.requested_issue_ids.iter().all(|id| valid_uuid(id))
        || !unique_strings(&result.requested_issue_ids)
        || result.items.len() > 8
        || !result.items.iter().all(validate_feed_issue)
        || !unique_strings(result.items.iter().map(|item| &item.id))
        || result.tickers.len() > 5
        || !result
            .tickers
            .iter()
            .all(|ticker| valid_ticker_output(ticker, 16))
        || !unique_strings(&result.tickers)
        || result.company_groups.len() != result.tickers.len()
        || result.gaps.len() > 8
        || result
            .gaps
            .iter()
            .any(|gap| !valid_uuid(&gap.issue_id) || gap.code != "not_found_or_not_published")
        || !unique_strings(result.gaps.iter().map(|gap| &gap.issue_id))
        || result.source_materials.len() > 24
        || !result.source_materials.iter().all(validate_feed_material)
        || result.research_packets.len() > 8
        || !result.research_packets.iter().all(validate_feed_packet)
    {
        return false;
    }

    let item_ids = result
        .items
        .iter()
        .map(|item| item.id.as_str())
        .collect::<BTreeSet<_>>();
    let gap_ids = result
        .gaps
        .iter()
        .map(|gap| gap.issue_id.as_str())
        .collect::<BTreeSet<_>>();
    let requested = result
        .requested_issue_ids
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    if !item_ids.is_disjoint(&gap_ids)
        || item_ids.union(&gap_ids).copied().collect::<BTreeSet<_>>() != requested
        || result
            .source_materials
            .iter()
            .any(|material| !item_ids.contains(material.issue_id.as_str()))
        || result
            .research_packets
            .iter()
            .any(|packet| !item_ids.contains(packet.issue_id.as_str()))
        || !unique_strings(
            result
                .research_packets
                .iter()
                .map(|packet| &packet.issue_id),
        )
        || result
            .source_materials
            .iter()
            .map(|material| utf16_len(&material.original_text))
            .sum::<usize>()
            > 120_000
    {
        return false;
    }

    let expected_item_order = result
        .requested_issue_ids
        .iter()
        .filter(|id| item_ids.contains(id.as_str()))
        .map(String::as_str)
        .collect::<Vec<_>>();
    if expected_item_order
        != result
            .items
            .iter()
            .map(|item| item.id.as_str())
            .collect::<Vec<_>>()
    {
        return false;
    }

    result
        .company_groups
        .iter()
        .zip(&result.tickers)
        .all(|(group, ticker)| {
            group.ticker == *ticker
                && group.issue_ids
                    == result
                        .items
                        .iter()
                        .filter(|item| {
                            item.entities
                                .iter()
                                .any(|entity| entity.ticker.0.as_deref() == Some(ticker))
                        })
                        .map(|item| item.id.clone())
                        .collect::<Vec<_>>()
        })
}

fn valid_accession(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 20
        && bytes[10] == b'-'
        && bytes[13] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| matches!(index, 10 | 13) || byte.is_ascii_digit())
}

fn normalized_cik(value: &str) -> Option<&str> {
    if value.is_empty() || value.len() > 10 || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let normalized = value.trim_start_matches('0');
    (!normalized.is_empty()).then_some(normalized)
}

fn same_filing_sec_url(metadata: &FilingMetadata, value: &str) -> bool {
    let Some(cik) = normalized_cik(&metadata.cik) else {
        return false;
    };
    if !valid_https_url(value) || !valid_accession(&metadata.accession_number) {
        return false;
    }
    let accession = metadata.accession_number.replace('-', "");
    value.starts_with(&format!(
        "https://www.sec.gov/Archives/edgar/data/{cik}/{accession}/"
    ))
}

fn validate_filing_metadata(metadata: &FilingMetadata) -> bool {
    valid_uuid(&metadata.filing_event_id)
        && valid_ticker_output(&metadata.ticker, 20)
        && normalized_cik(&metadata.cik).is_some()
        && valid_accession(&metadata.accession_number)
        && valid_form_type(&metadata.form_type)
        && valid_date(&metadata.filing_date)
        && metadata.report_date.0.as_deref().is_none_or(valid_date)
        && metadata.accepted_at.0.as_deref().is_none_or(valid_datetime)
        && metadata.sec_items.len() <= 128
        && metadata
            .sec_items
            .iter()
            .all(|item| bounded_text(item, 1, 32))
        && metadata.event_tags.len() <= 128
        && metadata
            .event_tags
            .iter()
            .all(|tag| bounded_text(tag, 1, 128))
        && same_filing_sec_url(metadata, &metadata.filing_detail_url)
        && metadata
            .primary_document_url
            .0
            .as_deref()
            .is_none_or(|url| same_filing_sec_url(metadata, url))
        && matches!(
            metadata.enrichment_status.as_str(),
            "not_requested" | "queued" | "ready" | "failed"
        )
}

fn validate_filing_id_input(input: &FilingIdInput) -> bool {
    valid_uuid(&input.filing_event_id)
}

fn validate_filing_search_input(input: &FilingSearchInput) -> bool {
    normalized_ticker(&input.ticker, 20).is_some()
        && input.form_type.as_deref().is_none_or(valid_form_type)
        && input.limit.is_none_or(|limit| (1..=25).contains(&limit))
}

fn validate_read_section_input(input: &FilingReadSectionInput) -> bool {
    valid_uuid(&input.filing_event_id)
        && bounded_text(&input.section_key, 1, 120)
        && input.offset.is_none_or(|offset| offset <= 3_000_000)
        && input
            .max_chars
            .is_none_or(|maximum| (1_000..=120_000).contains(&maximum))
}

fn validate_read_document_input(input: &FilingReadDocumentInput) -> bool {
    valid_uuid(&input.filing_event_id)
        && bounded_text(&input.document_key, 12, 120)
        && input.offset.is_none_or(|offset| offset <= 3_000_000)
        && input
            .max_chars
            .is_none_or(|maximum| (1_000..=120_000).contains(&maximum))
}

fn validate_filing_section(section: &FilingSection) -> bool {
    bounded_text(&section.key, 1, 120)
        && matches!(
            section.kind.as_str(),
            "sec_item" | "document_section" | "primary_document"
        )
        && bounded_text(&section.heading, 0, 1_000)
        && bounded_nullable_text(&section.item_code, 32)
        && matches!(
            section.confidence.as_str(),
            "exact" | "heuristic" | "fallback"
        )
        && section.text_length <= 3_000_000
        && match section.kind.as_str() {
            "sec_item" => section.item_code.0.is_some() && section.key.starts_with("item:"),
            "primary_document" => {
                section.key == "document:primary"
                    && section.item_code.0.is_none()
                    && section.confidence == "fallback"
            }
            "document_section" => section.item_code.0.is_none(),
            _ => false,
        }
}

fn valid_document_key(value: &str) -> bool {
    value.len() == 31
        && value.starts_with("document:")
        && value.as_bytes()[9..]
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn validate_filing_document(document: &FilingDocument, metadata: &FilingMetadata) -> bool {
    valid_document_key(&document.document_key)
        && bounded_text(&document.document_name, 1, 1_000)
        && bounded_nullable_text(&document.document_type, 1_000)
        && bounded_nullable_text(&document.description, 1_000)
        && same_filing_sec_url(metadata, &document.document_url)
        && document
            .sequence
            .0
            .is_none_or(|sequence| sequence <= 1_000_000)
        && document
            .size_bytes
            .0
            .is_none_or(|size| size <= 1_000_000_000_000)
        && document.recommendation_score <= 100
}

fn validate_citation(citation: &FilingCitation, metadata: &FilingMetadata) -> bool {
    if citation.source != "SEC EDGAR"
        || citation.filing_event_id != metadata.filing_event_id
        || citation.accession_number != metadata.accession_number
        || citation.filing_detail_url != metadata.filing_detail_url
        || !same_filing_sec_url(metadata, &citation.document_url)
        || !bounded_text(&citation.section_key, 1, 120)
        || !bounded_text(&citation.section_heading, 0, 1_200)
        || !matches!(
            citation.extraction.as_str(),
            "deterministic_html" | "stored_structured_form4"
        )
        || !valid_datetime(&citation.fetched_at)
    {
        return false;
    }
    match (
        citation.document_key.as_deref(),
        citation.document_name.as_deref(),
        citation.document_type.as_ref(),
    ) {
        (None, None, None) => true,
        (Some(key), Some(name), Some(document_type)) => {
            valid_document_key(key)
                && bounded_text(name, 1, 1_000)
                && bounded_nullable_text(document_type, 1_000)
        }
        _ => false,
    }
}

fn validate_filing_brief(brief: &FilingBrief, metadata: &FilingMetadata) -> bool {
    brief.filing_event_id == metadata.filing_event_id
        && brief.locale == "ko-KR"
        && bounded_text(&brief.headline_ko, 1, 240)
        && (1..=2).contains(&brief.summary_sentences.len())
        && brief
            .summary_sentences
            .iter()
            .all(|sentence| bounded_text(sentence, 1, 340))
        && (1..=4).contains(&brief.facts.len())
        && brief.question_suggestions.len() <= 2
        && brief
            .question_suggestions
            .iter()
            .all(|question| bounded_text(question, 1, 220))
        && brief.uncertainty_notes.len() <= 3
        && brief
            .uncertainty_notes
            .iter()
            .all(|note| bounded_text(note, 1, 240))
        && same_filing_sec_url(metadata, &brief.source_document_url)
        && (1..=4).contains(&brief.source_section_keys.len())
        && brief
            .source_section_keys
            .iter()
            .all(|key| bounded_text(key, 1, 180))
        && bounded_text(&brief.generator_model, 1, 160)
        && bounded_text(&brief.prompt_version, 1, 80)
        && (1..=1_000_000).contains(&brief.schema_version)
        && valid_datetime(&brief.generated_at)
        && brief.facts.iter().all(|fact| {
            bounded_text(&fact.fact_ko, 1, 260)
                && bounded_text(&fact.section_key, 1, 180)
                && brief.source_section_keys.contains(&fact.section_key)
                && bounded_text(&fact.source_quote, 1, 700)
                && bounded_text(&fact.section_heading, 1, 300)
                && fact.source_document_url == brief.source_document_url
        })
}

fn validate_brief_result(result: &FilingBriefResult) -> bool {
    validate_filing_metadata(&result.filing)
        && result.machine_generated
        && bounded_text(&result.note, 1, 1_000)
        && match (result.status.as_str(), result.brief.0.as_ref()) {
            ("ready", Some(brief)) => validate_filing_brief(brief, &result.filing),
            ("not_available", None) => true,
            _ => false,
        }
}

fn validate_search_result(results: &[FilingMetadata]) -> bool {
    results.len() <= 25
        && results.iter().all(validate_filing_metadata)
        && unique_strings(results.iter().map(|metadata| &metadata.filing_event_id))
}

fn validate_sections_result(result: &FilingSectionsResult) -> bool {
    validate_filing_metadata(&result.filing)
        && matches!(
            result.filing.form_type.as_str(),
            "8-K" | "8-K/A" | "6-K" | "6-K/A"
        )
        && (1..=512).contains(&result.sections.len())
        && result.sections.iter().all(validate_filing_section)
        && unique_strings(result.sections.iter().map(|section| &section.key))
        && result.sections.first().is_some_and(|section| {
            section.key == "document:primary" && section.kind == "primary_document"
        })
        && same_filing_sec_url(&result.filing, &result.document_url)
        && valid_datetime(&result.fetched_at)
}

fn validate_read_section_result(result: &FilingReadSectionResult) -> bool {
    if !validate_filing_metadata(&result.filing)
        || !matches!(
            result.filing.form_type.as_str(),
            "8-K" | "8-K/A" | "6-K" | "6-K/A"
        )
        || !validate_filing_section(&result.section)
        || !bounded_text(&result.text, 0, 120_000)
        || result.offset > 3_000_000
        || !validate_citation(&result.citation, &result.filing)
        || result.citation.extraction != "deterministic_html"
        || result.citation.section_key != result.section.key
        || result.citation.section_heading != result.section.heading
        || result.citation.document_key.is_some()
    {
        return false;
    }
    let end = result
        .offset
        .checked_add(u32::try_from(utf16_len(&result.text)).unwrap_or(u32::MAX));
    match (result.next_offset.0, end) {
        (Some(next), Some(end)) => next == end && next <= result.section.text_length,
        (None, Some(end)) => {
            end <= result.section.text_length || result.offset > result.section.text_length
        }
        _ => false,
    }
}

fn validate_documents_result(result: &FilingDocumentsResult) -> bool {
    validate_filing_metadata(&result.filing)
        && matches!(
            result.filing.form_type.as_str(),
            "8-K" | "8-K/A" | "6-K" | "6-K/A"
        )
        && (1..=256).contains(&result.documents.len())
        && result
            .documents
            .iter()
            .all(|document| validate_filing_document(document, &result.filing))
        && unique_strings(
            result
                .documents
                .iter()
                .map(|document| &document.document_key),
        )
        && unique_strings(
            result
                .documents
                .iter()
                .map(|document| &document.document_url),
        )
        && result
            .documents
            .iter()
            .filter(|document| document.is_primary)
            .count()
            <= 1
        && valid_datetime(&result.fetched_at)
}

fn validate_read_document_result(result: &FilingReadDocumentResult) -> bool {
    if !validate_filing_metadata(&result.filing)
        || !matches!(
            result.filing.form_type.as_str(),
            "8-K" | "8-K/A" | "6-K" | "6-K/A"
        )
        || !validate_filing_document(&result.document, &result.filing)
        || !result.document.readable
        || !bounded_text(&result.text, 0, 120_000)
        || result.offset > 3_000_000
        || !validate_citation(&result.citation, &result.filing)
        || result.citation.extraction != "deterministic_html"
        || result.citation.document_url != result.document.document_url
        || result.citation.section_key != result.document.document_key
        || result.citation.document_key.as_deref() != Some(result.document.document_key.as_str())
        || result.citation.document_name.as_deref() != Some(result.document.document_name.as_str())
        || result.citation.document_type.as_ref() != Some(&result.document.document_type)
    {
        return false;
    }
    let end = result
        .offset
        .checked_add(u32::try_from(utf16_len(&result.text)).unwrap_or(u32::MAX));
    match (result.next_offset.0, end) {
        (Some(next), Some(end)) => next == end,
        (None, Some(_)) => true,
        _ => false,
    }
}

fn bounded_optional_number(value: &RequiredNullable<f64>) -> bool {
    value
        .0
        .is_none_or(|number| number.is_finite() && (-1e18..=1e18).contains(&number))
}

fn validate_form4_owner(owner: &Form4Owner, filing_id: &str) -> bool {
    owner.filing_event_id == filing_id
        && owner.reporting_owner_index <= 1_000_000
        && owner.owner_cik.0.as_deref().is_none_or(|cik| {
            !cik.is_empty() && cik.len() <= 10 && cik.bytes().all(|byte| byte.is_ascii_digit())
        })
        && bounded_text(&owner.owner_name, 1, 1_000)
        && bounded_nullable_text(&owner.officer_title, 1_000)
        && valid_datetime(&owner.created_at)
        && valid_datetime(&owner.updated_at)
}

fn validate_form4_transaction(transaction: &Form4Transaction, filing_id: &str) -> bool {
    valid_uuid(&transaction.id)
        && transaction.filing_event_id == filing_id
        && transaction.reporting_owner_index <= 1_000_000
        && transaction.transaction_index <= 1_000_000
        && matches!(
            transaction.security_kind.as_str(),
            "non_derivative" | "derivative"
        )
        && bounded_text(&transaction.security_title, 1, 1_000)
        && transaction
            .transaction_date
            .0
            .as_deref()
            .is_none_or(valid_date)
        && bounded_nullable_text(&transaction.transaction_code, 16)
        && bounded_optional_number(&transaction.transaction_shares)
        && bounded_optional_number(&transaction.transaction_price_per_share)
        && transaction
            .acquired_disposed_code
            .0
            .as_deref()
            .is_none_or(|code| matches!(code, "A" | "D"))
        && bounded_optional_number(&transaction.shares_owned_following)
        && transaction
            .direct_indirect_code
            .0
            .as_deref()
            .is_none_or(|code| matches!(code, "D" | "I"))
        && bounded_nullable_text(&transaction.nature_of_ownership, 4_000)
        && bounded_optional_number(&transaction.reported_transaction_value)
        && matches!(
            transaction.reported_value_basis.as_str(),
            "p_or_s" | "not_reported"
        )
        && valid_datetime(&transaction.created_at)
        && valid_datetime(&transaction.updated_at)
}

fn validate_form4_summary(summary: &Form4Summary, filing_id: &str) -> bool {
    summary.filing_event_id == filing_id
        && summary.reporting_owner_count <= 1_000_000
        && summary.transaction_count <= 1_000_000
        && summary.reported_purchase_count <= summary.transaction_count
        && summary.reported_sale_count <= summary.transaction_count
        && summary
            .reported_purchase_count
            .checked_add(summary.reported_sale_count)
            .is_some_and(|total| total <= summary.transaction_count)
        && bounded_optional_number(&summary.reported_purchase_shares)
        && bounded_optional_number(&summary.reported_sale_shares)
        && bounded_optional_number(&summary.reported_purchase_value)
        && bounded_optional_number(&summary.reported_sale_value)
        && bounded_optional_number(&summary.net_reported_shares)
        && matches!(summary.tenb5_one_status.as_str(), "yes" | "no" | "unknown")
        && bounded_nullable_text(&summary.source_xml_version, 160)
        && valid_datetime(&summary.created_at)
        && valid_datetime(&summary.updated_at)
}

fn validate_form4_result(result: &Form4TransactionsResult) -> bool {
    let filing_id = result.filing.filing_event_id.as_str();
    if !validate_filing_metadata(&result.filing)
        || !matches!(result.filing.form_type.as_str(), "4" | "4/A")
        || result.source != "stored_structured_form4_facts"
        || result.reporting_owners.len() > 128
        || result.transactions.len() > 4_096
        || !result
            .reporting_owners
            .iter()
            .all(|owner| validate_form4_owner(owner, filing_id))
        || !result
            .transactions
            .iter()
            .all(|transaction| validate_form4_transaction(transaction, filing_id))
        || !validate_citation(&result.citation, &result.filing)
        || result.citation.extraction != "stored_structured_form4"
        || result.citation.section_key != "form4:transactions"
        || result.citation.document_key.is_some()
    {
        return false;
    }

    let owner_indices = result
        .reporting_owners
        .iter()
        .map(|owner| owner.reporting_owner_index)
        .collect::<BTreeSet<_>>();
    if owner_indices.len() != result.reporting_owners.len()
        || result
            .transactions
            .iter()
            .any(|transaction| !owner_indices.contains(&transaction.reporting_owner_index))
        || !unique_strings(
            result
                .transactions
                .iter()
                .map(|transaction| &transaction.id),
        )
        || result
            .transactions
            .iter()
            .map(|transaction| {
                (
                    transaction.reporting_owner_index,
                    transaction.transaction_index,
                )
            })
            .collect::<BTreeSet<_>>()
            .len()
            != result.transactions.len()
    {
        return false;
    }

    result.summary.0.as_ref().is_none_or(|summary| {
        validate_form4_summary(summary, filing_id)
            && usize::try_from(summary.reporting_owner_count).ok()
                == Some(result.reporting_owners.len())
            && usize::try_from(summary.transaction_count).ok() == Some(result.transactions.len())
    })
}

fn checked<T>(valid: bool, value: T, contract_id: &'static str) -> Result<T, ContractValueError> {
    valid
        .then_some(value)
        .ok_or(ContractValueError::Semantic(contract_id))
}

pub(super) fn validate_value(contract_id: &str, value: &Value) -> Result<(), ContractValueError> {
    let contract_id = contract(contract_id)
        .map(|descriptor| descriptor.id)
        .ok_or_else(|| ContractValueError::UnknownContract(contract_id.to_owned()))?;
    validate_front_bounds(contract_id, value)?;
    match contract_id {
        KRW_FEED_GET_ITEMS_INPUT_V1 => {
            let input: FeedGetItemsInput = decode(value, contract_id)?;
            checked(validate_get_feed_input(&input), (), contract_id)
        }
        KRW_FEED_LIST_ITEMS_INPUT_V1 => {
            let input: FeedListItemsInput = decode(value, contract_id)?;
            checked(validate_list_feed_input(&input), (), contract_id)
        }
        KRW_FEED_CONTEXT_INPUT_V2 => {
            let input: FeedContextInput = decode(value, contract_id)?;
            checked(validate_context_input(&input), (), contract_id)
        }
        KRW_FEED_GET_ITEMS_RESULT_V1 => {
            let result: FeedGetItemsResult = decode(value, contract_id)?;
            checked(validate_get_feed_result(&result), (), contract_id)
        }
        KRW_FEED_LIST_ITEMS_RESULT_V1 => {
            let result: FeedListItemsResult = decode(value, contract_id)?;
            checked(validate_list_feed_result(&result), (), contract_id)
        }
        KRW_FEED_CONTEXT_V2 => {
            let result: FeedContextResult = decode(value, contract_id)?;
            checked(validate_context_result(&result), (), contract_id)
        }
        KRW_FILING_GET_INPUT_V1
        | KRW_FILING_BRIEF_INPUT_V1
        | KRW_FILING_SECTIONS_INPUT_V1
        | KRW_FILING_DOCUMENTS_INPUT_V1
        | KRW_FORM4_TRANSACTIONS_INPUT_V1 => {
            let input: FilingIdInput = decode(value, contract_id)?;
            checked(validate_filing_id_input(&input), (), contract_id)
        }
        KRW_FILING_SEARCH_INPUT_V1 => {
            let input: FilingSearchInput = decode(value, contract_id)?;
            checked(validate_filing_search_input(&input), (), contract_id)
        }
        KRW_FILING_READ_SECTION_INPUT_V1 => {
            let input: FilingReadSectionInput = decode(value, contract_id)?;
            checked(validate_read_section_input(&input), (), contract_id)
        }
        KRW_FILING_READ_DOCUMENT_INPUT_V1 => {
            let input: FilingReadDocumentInput = decode(value, contract_id)?;
            checked(validate_read_document_input(&input), (), contract_id)
        }
        KRW_FILING_METADATA_V1 => {
            let metadata: FilingMetadata = decode(value, contract_id)?;
            checked(validate_filing_metadata(&metadata), (), contract_id)
        }
        KRW_FILING_BRIEF_RESULT_V1 => {
            let result: FilingBriefResult = decode(value, contract_id)?;
            checked(validate_brief_result(&result), (), contract_id)
        }
        KRW_FILING_SEARCH_RESULT_V1 => {
            let result: Vec<FilingMetadata> = decode(value, contract_id)?;
            checked(validate_search_result(&result), (), contract_id)
        }
        KRW_FILING_SECTIONS_RESULT_V1 => {
            let result: FilingSectionsResult = decode(value, contract_id)?;
            checked(validate_sections_result(&result), (), contract_id)
        }
        KRW_FILING_READ_SECTION_RESULT_V1 => {
            let result: FilingReadSectionResult = decode(value, contract_id)?;
            checked(validate_read_section_result(&result), (), contract_id)
        }
        KRW_FILING_DOCUMENTS_RESULT_V1 => {
            let result: FilingDocumentsResult = decode(value, contract_id)?;
            checked(validate_documents_result(&result), (), contract_id)
        }
        KRW_FILING_READ_DOCUMENT_RESULT_V1 => {
            let result: FilingReadDocumentResult = decode(value, contract_id)?;
            checked(validate_read_document_result(&result), (), contract_id)
        }
        KRW_FORM4_TRANSACTIONS_RESULT_V1 => {
            let result: Form4TransactionsResult = decode(value, contract_id)?;
            checked(validate_form4_result(&result), (), contract_id)
        }
        _ => Err(ContractValueError::UnknownContract(contract_id.to_owned())),
    }
}

fn deduplicated(values: &[String]) -> Vec<&str> {
    let mut seen = BTreeSet::new();
    values
        .iter()
        .map(String::as_str)
        .filter(|value| seen.insert(*value))
        .collect()
}

const FRONT_EXCHANGE: &str = "krw-front/capability-exchange";

/// Whether a filing-search result set binds to the requested listing.
///
/// The vendor catalog resolves the request through the requested company and
/// can label the returned events with that company's canonical share-class
/// listing (production 2026-09-01: a `GOOGL` request returned `GOOG`-tagged
/// events of the same CIK, and the literal-echo rule rejected the whole
/// exchange). Binding therefore holds when every row echoes the requested
/// ticker, or when the entire result carries exactly one vendor ticker that
/// differs from the request on exactly one CIK — one company, one relabel.
/// Anything mixed (two tickers, two CIKs, a partial relabel) fails closed.
pub fn filing_search_binds_requested_ticker(
    requested_ticker: &str,
    items: &[FilingMetadata],
) -> bool {
    if items.is_empty() || items.iter().all(|item| item.ticker == requested_ticker) {
        return true;
    }
    let mut vendor_ticker: Option<&str> = None;
    let mut vendor_cik: Option<&str> = None;
    for item in items {
        if item.ticker == requested_ticker {
            // A partial relabel mixes two listing labels: not a single
            // vendor-canonical projection.
            return false;
        }
        match vendor_ticker {
            None => vendor_ticker = Some(item.ticker.as_str()),
            Some(seen) if seen == item.ticker => {}
            Some(_) => return false,
        }
        match vendor_cik {
            None => vendor_cik = Some(item.cik.as_str()),
            Some(seen) if seen == item.cik => {}
            Some(_) => return false,
        }
    }
    vendor_ticker.is_some()
}

/// Validate input and output together, including request identity binding.
///
/// Standalone JSON Schema cannot prove that a returned filing is the filing
/// requested by the action. The kernel should call this after both individual
/// pins have passed and before evidence ingestion.
pub fn validate_front_exchange(
    input_contract: &str,
    input: &Value,
    output_contract: &str,
    output: &Value,
) -> Result<(), ContractValueError> {
    validate_value(input_contract, input)?;
    validate_value(output_contract, output)?;
    let valid = match (input_contract, output_contract) {
        (KRW_FEED_GET_ITEMS_INPUT_V1, KRW_FEED_GET_ITEMS_RESULT_V1) => {
            let input: FeedGetItemsInput = decode(input, KRW_FEED_GET_ITEMS_INPUT_V1)?;
            let output: FeedGetItemsResult = decode(output, KRW_FEED_GET_ITEMS_RESULT_V1)?;
            let requested = deduplicated(&input.issue_ids);
            let returned = output
                .items
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>();
            let missing = output
                .missing_issue_ids
                .iter()
                .map(String::as_str)
                .collect::<BTreeSet<_>>();
            returned
                == requested
                    .iter()
                    .copied()
                    .filter(|id| !missing.contains(id))
                    .collect::<Vec<_>>()
                && requested.iter().copied().collect::<BTreeSet<_>>()
                    == returned
                        .iter()
                        .copied()
                        .chain(missing.iter().copied())
                        .collect::<BTreeSet<_>>()
                && output.items.iter().all(|item| {
                    item.source_posts.len()
                        <= if input.include_sources.unwrap_or(false) {
                            2
                        } else {
                            0
                        }
                })
        }
        (KRW_FEED_LIST_ITEMS_INPUT_V1, KRW_FEED_LIST_ITEMS_RESULT_V1) => {
            let input: FeedListItemsInput = decode(input, KRW_FEED_LIST_ITEMS_INPUT_V1)?;
            let output: FeedListItemsResult = decode(output, KRW_FEED_LIST_ITEMS_RESULT_V1)?;
            let limit = input.limit.unwrap_or(10);
            let tickers = input
                .tickers
                .unwrap_or_default()
                .iter()
                .filter_map(|ticker| normalized_ticker(ticker, 16))
                .collect::<BTreeSet<_>>();
            output.pagination.limit == limit
                && output.items.len() <= usize::from(limit)
                && (tickers.is_empty()
                    || output.items.iter().all(|item| {
                        item.entities.iter().any(|entity| {
                            entity
                                .ticker
                                .0
                                .as_ref()
                                .is_some_and(|ticker| tickers.contains(ticker))
                        })
                    }))
        }
        (KRW_FEED_CONTEXT_INPUT_V2, KRW_FEED_CONTEXT_V2) => {
            let input: FeedContextInput = decode(input, KRW_FEED_CONTEXT_INPUT_V2)?;
            let output: FeedContextResult = decode(output, KRW_FEED_CONTEXT_V2)?;
            let requested = deduplicated(&input.issue_ids);
            let maximum_posts = usize::from(input.max_posts_per_issue.unwrap_or(2));
            let expected_ticker_prefix = input
                .tickers
                .unwrap_or_default()
                .iter()
                .filter_map(|ticker| normalized_ticker(ticker, 16))
                .fold(Vec::new(), |mut values, ticker| {
                    if !values.contains(&ticker) {
                        values.push(ticker);
                    }
                    values
                });
            output
                .requested_issue_ids
                .iter()
                .map(String::as_str)
                .eq(requested.iter().copied())
                && output.tickers.starts_with(&expected_ticker_prefix)
                && output
                    .items
                    .iter()
                    .all(|item| item.source_posts.len() <= maximum_posts)
                && output.items.iter().all(|item| {
                    output
                        .source_materials
                        .iter()
                        .filter(|material| material.issue_id == item.id)
                        .count()
                        <= maximum_posts
                })
        }
        (KRW_FILING_GET_INPUT_V1, KRW_FILING_METADATA_V1) => {
            let input: FilingIdInput = decode(input, KRW_FILING_GET_INPUT_V1)?;
            let output: FilingMetadata = decode(output, KRW_FILING_METADATA_V1)?;
            input.filing_event_id == output.filing_event_id
        }
        (KRW_FILING_BRIEF_INPUT_V1, KRW_FILING_BRIEF_RESULT_V1) => {
            let input: FilingIdInput = decode(input, KRW_FILING_BRIEF_INPUT_V1)?;
            let output: FilingBriefResult = decode(output, KRW_FILING_BRIEF_RESULT_V1)?;
            input.filing_event_id == output.filing.filing_event_id
        }
        (KRW_FILING_SEARCH_INPUT_V1, KRW_FILING_SEARCH_RESULT_V1) => {
            let input: FilingSearchInput = decode(input, KRW_FILING_SEARCH_INPUT_V1)?;
            let output: Vec<FilingMetadata> = decode(output, KRW_FILING_SEARCH_RESULT_V1)?;
            let ticker = normalized_ticker(&input.ticker, 20).expect("validated input");
            output.len() <= usize::from(input.limit.unwrap_or(10))
                && filing_search_binds_requested_ticker(&ticker, &output)
                && output.iter().all(|metadata| {
                    input
                        .form_type
                        .as_ref()
                        .is_none_or(|form| metadata.form_type == *form)
                })
        }
        (KRW_FILING_SECTIONS_INPUT_V1, KRW_FILING_SECTIONS_RESULT_V1) => {
            let input: FilingIdInput = decode(input, KRW_FILING_SECTIONS_INPUT_V1)?;
            let output: FilingSectionsResult = decode(output, KRW_FILING_SECTIONS_RESULT_V1)?;
            input.filing_event_id == output.filing.filing_event_id
        }
        (KRW_FILING_READ_SECTION_INPUT_V1, KRW_FILING_READ_SECTION_RESULT_V1) => {
            let input: FilingReadSectionInput = decode(input, KRW_FILING_READ_SECTION_INPUT_V1)?;
            let output: FilingReadSectionResult =
                decode(output, KRW_FILING_READ_SECTION_RESULT_V1)?;
            input.filing_event_id == output.filing.filing_event_id
                && input.section_key == output.section.key
                && output.offset == input.offset.unwrap_or(0)
                && utf16_len(&output.text)
                    <= usize::try_from(input.max_chars.unwrap_or(24_000)).unwrap_or(usize::MAX)
        }
        (KRW_FILING_DOCUMENTS_INPUT_V1, KRW_FILING_DOCUMENTS_RESULT_V1) => {
            let input: FilingIdInput = decode(input, KRW_FILING_DOCUMENTS_INPUT_V1)?;
            let output: FilingDocumentsResult = decode(output, KRW_FILING_DOCUMENTS_RESULT_V1)?;
            input.filing_event_id == output.filing.filing_event_id
        }
        (KRW_FILING_READ_DOCUMENT_INPUT_V1, KRW_FILING_READ_DOCUMENT_RESULT_V1) => {
            let input: FilingReadDocumentInput = decode(input, KRW_FILING_READ_DOCUMENT_INPUT_V1)?;
            let output: FilingReadDocumentResult =
                decode(output, KRW_FILING_READ_DOCUMENT_RESULT_V1)?;
            input.filing_event_id == output.filing.filing_event_id
                && input.document_key == output.document.document_key
                && output.offset == input.offset.unwrap_or(0)
                && utf16_len(&output.text)
                    <= usize::try_from(input.max_chars.unwrap_or(24_000)).unwrap_or(usize::MAX)
        }
        (KRW_FORM4_TRANSACTIONS_INPUT_V1, KRW_FORM4_TRANSACTIONS_RESULT_V1) => {
            let input: FilingIdInput = decode(input, KRW_FORM4_TRANSACTIONS_INPUT_V1)?;
            let output: Form4TransactionsResult = decode(output, KRW_FORM4_TRANSACTIONS_RESULT_V1)?;
            input.filing_event_id == output.filing.filing_event_id
        }
        _ => false,
    };
    checked(valid, (), FRONT_EXCHANGE)
}

/// Bind a read-document input to the exact prior verified list artifact.
pub fn validate_filing_document_membership(
    list_result: &Value,
    read_input: &Value,
) -> Result<(), ContractValueError> {
    validate_value(KRW_FILING_DOCUMENTS_RESULT_V1, list_result)?;
    validate_value(KRW_FILING_READ_DOCUMENT_INPUT_V1, read_input)?;
    let list: FilingDocumentsResult = decode(list_result, KRW_FILING_DOCUMENTS_RESULT_V1)?;
    let read: FilingReadDocumentInput = decode(read_input, KRW_FILING_READ_DOCUMENT_INPUT_V1)?;
    checked(
        list.filing.filing_event_id == read.filing_event_id
            && list
                .documents
                .iter()
                .any(|document| document.document_key == read.document_key && document.readable),
        (),
        FRONT_EXCHANGE,
    )
}

/// Bind a read-section input to the exact prior verified section-list artifact.
pub fn validate_filing_section_membership(
    list_result: &Value,
    read_input: &Value,
) -> Result<(), ContractValueError> {
    validate_value(KRW_FILING_SECTIONS_RESULT_V1, list_result)?;
    validate_value(KRW_FILING_READ_SECTION_INPUT_V1, read_input)?;
    let list: FilingSectionsResult = decode(list_result, KRW_FILING_SECTIONS_RESULT_V1)?;
    let read: FilingReadSectionInput = decode(read_input, KRW_FILING_READ_SECTION_INPUT_V1)?;
    checked(
        list.filing.filing_event_id == read.filing_event_id
            && list
                .sections
                .iter()
                .any(|section| section.key == read.section_key),
        (),
        FRONT_EXCHANGE,
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedFrontBundle {
    pub manifest_sha256: String,
    pub authority_sha256: String,
    pub schema_bundle_sha256: String,
    pub conformance_vectors_sha256: String,
    pub contract_count: usize,
}

fn value_object<'a>(
    value: &'a Value,
    message: &'static str,
) -> Result<&'a serde_json::Map<String, Value>, ContractArtifactError> {
    value
        .as_object()
        .ok_or(ContractArtifactError::InvalidManifest(message))
}

fn value_string<'a>(
    object: &'a serde_json::Map<String, Value>,
    field: &str,
    message: &'static str,
) -> Result<&'a str, ContractArtifactError> {
    object
        .get(field)
        .and_then(Value::as_str)
        .ok_or(ContractArtifactError::InvalidManifest(message))
}

/// Verify the complete source-pinned front contract artifact set.
pub fn verify_front_embedded() -> Result<VerifiedFrontBundle, ContractArtifactError> {
    super::verify_json("krw-front manifest", FRONT_MANIFEST_BYTES)?;
    super::verify_hash(
        "krw-front manifest",
        FRONT_MANIFEST_BYTES,
        FRONT_GENERATED_MANIFEST_SHA256,
    )?;
    super::verify_json("krw-front schema bundle", FRONT_BUNDLE_BYTES)?;
    super::verify_json("krw-front conformance vectors", FRONT_VECTOR_BYTES)?;

    let manifest_value: Value = serde_json::from_slice(FRONT_MANIFEST_BYTES)?;
    let manifest = value_object(&manifest_value, "front manifest must be an object")?;
    if value_string(manifest, "format", "front manifest format is missing")?
        != "krw-front-contract-export/v1"
    {
        return Err(ContractArtifactError::InvalidManifest(
            "unsupported front manifest format",
        ));
    }
    let authority = manifest
        .get("authority")
        .ok_or(ContractArtifactError::InvalidManifest(
            "front authority is missing",
        ))?;
    let authority_hash = value_string(
        manifest,
        "authority_sha256",
        "front authority hash is missing",
    )?;
    super::verify_hash(
        "krw-front authority",
        &serde_jcs::to_vec(authority)?,
        authority_hash,
    )?;

    let bundle_ref = value_object(
        manifest
            .get("schema_bundle")
            .ok_or(ContractArtifactError::InvalidManifest(
                "front schema bundle reference is missing",
            ))?,
        "front schema bundle reference must be an object",
    )?;
    let bundle_hash = value_string(bundle_ref, "sha256", "front schema bundle hash is missing")?;
    super::verify_hash("krw-front schema bundle", FRONT_BUNDLE_BYTES, bundle_hash)?;

    let vector_ref = value_object(
        manifest
            .get("conformance_vectors")
            .ok_or(ContractArtifactError::InvalidManifest(
                "front vector reference is missing",
            ))?,
        "front vector reference must be an object",
    )?;
    let vector_hash = value_string(vector_ref, "sha256", "front vector hash is missing")?;
    super::verify_hash(
        "krw-front conformance vectors",
        FRONT_VECTOR_BYTES,
        vector_hash,
    )?;

    let bundle_value: Value = serde_json::from_slice(FRONT_BUNDLE_BYTES)?;
    let bundle = value_object(&bundle_value, "front bundle must be an object")?;
    if value_string(bundle, "format", "front bundle format is missing")?
        != "krw-front-contract-export/v1"
        || value_string(
            bundle,
            "authority_sha256",
            "front bundle authority is missing",
        )? != authority_hash
    {
        return Err(ContractArtifactError::InvalidManifest(
            "front bundle metadata differs from manifest",
        ));
    }
    let bundled_contracts = value_object(
        bundle
            .get("contracts")
            .ok_or(ContractArtifactError::InvalidManifest(
                "front bundled contracts are missing",
            ))?,
        "front bundled contracts must be an object",
    )?;
    let manifest_contracts = value_object(
        manifest
            .get("contracts")
            .ok_or(ContractArtifactError::InvalidManifest(
                "front manifest contracts are missing",
            ))?,
        "front manifest contracts must be an object",
    )?;
    if bundled_contracts.len() != 22 || manifest_contracts.len() != 22 {
        return Err(ContractArtifactError::InvalidManifest(
            "front contract count must be exactly 22",
        ));
    }

    for descriptor in descriptors() {
        super::verify_json(descriptor.id, descriptor.schema)?;
        super::verify_hash(descriptor.id, descriptor.schema, descriptor.schema_sha256)?;
        let bundled =
            bundled_contracts
                .get(descriptor.id)
                .ok_or(ContractArtifactError::InvalidManifest(
                    "front descriptor is missing from bundle",
                ))?;
        if serde_jcs::to_vec(bundled)? != descriptor.schema {
            return Err(ContractArtifactError::BundleSchemaMismatch(descriptor.id));
        }
        let manifest_entry = value_object(
            manifest_contracts
                .get(descriptor.id)
                .ok_or(ContractArtifactError::InvalidManifest(
                    "front descriptor is missing from manifest",
                ))?,
            "front manifest contract must be an object",
        )?;
        if value_string(
            manifest_entry,
            "schema_sha256",
            "front contract schema hash is missing",
        )? != descriptor.schema_sha256
            || value_string(
                manifest_entry,
                "authority_kind",
                "front contract authority kind is missing",
            )? != "typescript_zod_and_return_type"
            || value_string(
                manifest_entry,
                "semantic_validation",
                "front semantic validator is missing",
            )? != "bounded_rust_typed_validator"
        {
            return Err(ContractArtifactError::InvalidManifest(
                "front manifest descriptor differs from binding",
            ));
        }
    }

    let vector_value: Value = serde_json::from_slice(FRONT_VECTOR_BYTES)?;
    let vectors = value_object(&vector_value, "front vectors must be an object")?;
    if value_string(vectors, "format", "front vector format is missing")?
        != "krw-front-contract-conformance/v1"
        || value_string(
            vectors,
            "authority_sha256",
            "front vector authority is missing",
        )? != authority_hash
        || !matches!(vectors.get("vectors"), Some(Value::Array(values)) if values.len() == 44)
    {
        return Err(ContractArtifactError::InvalidManifest(
            "front conformance vector metadata is invalid",
        ));
    }

    Ok(VerifiedFrontBundle {
        manifest_sha256: FRONT_GENERATED_MANIFEST_SHA256.to_owned(),
        authority_sha256: authority_hash.to_owned(),
        schema_bundle_sha256: bundle_hash.to_owned(),
        conformance_vectors_sha256: vector_hash.to_owned(),
        contract_count: descriptors().len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vectors() -> Vec<Value> {
        serde_json::from_slice::<Value>(FRONT_VECTOR_BYTES)
            .expect("vectors parse")
            .get("vectors")
            .and_then(Value::as_array)
            .expect("vectors array")
            .clone()
    }

    fn fixture(contract_id: &str) -> Value {
        vectors()
            .into_iter()
            .find(|vector| {
                vector.get("contract_id").and_then(Value::as_str) == Some(contract_id)
                    && vector.get("valid").and_then(Value::as_bool) == Some(true)
            })
            .and_then(|vector| vector.get("value").cloned())
            .expect("positive fixture")
    }

    #[test]
    fn source_pinned_bundle_and_all_conformance_vectors_verify() {
        let verified = verify_front_embedded().expect("front artifact set verifies");
        assert_eq!(verified.contract_count, 22);
        for vector in vectors() {
            let contract_id = vector
                .get("contract_id")
                .and_then(Value::as_str)
                .expect("contract id");
            let value = vector.get("value").expect("value");
            let expected = vector.get("valid").and_then(Value::as_bool).expect("valid");
            assert_eq!(
                validate_value(contract_id, value).is_ok(),
                expected,
                "conformance vector failed for {contract_id}"
            );
        }
    }

    #[test]
    fn filing_search_exchange_accepts_a_single_cik_share_class_alias() {
        // Production shape (2026-09-01): a GOOGL request resolved through
        // the company's CIK returned GOOG-tagged catalog events; the literal
        // echo rule rejected the exchange and the run lost the whole filing
        // ladder. One vendor ticker on one CIK across every row binds.
        let metadata = fixture(KRW_FILING_METADATA_V1);
        let input = serde_json::json!({"ticker": "GOOGL", "limit": 5});
        // Relabel the listing ticker; when the CIK changes, rewrite the SEC
        // URL prefixes with it so the row stays internally consistent (the
        // semantic validator checks url ≡ cik + accession).
        let relabeled = |ticker: &str, cik: Option<&str>| {
            let mut row = metadata.clone();
            static ROW: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);
            let n = ROW.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            row["filing_event_id"] = Value::String(format!("00000000-0000-4000-8000-{n:012}"));
            row["ticker"] = Value::String(ticker.into());
            if let Some(cik) = cik {
                let old_cik = row["cik"]
                    .as_str()
                    .expect("fixture cik")
                    .trim_start_matches('0')
                    .to_string();
                let new_cik = cik.trim_start_matches('0').to_string();
                row["cik"] = Value::String(cik.into());
                for key in ["filing_detail_url", "primary_document_url"] {
                    if let Some(url) = row[key].as_str() {
                        row[key] = Value::String(
                            url.replace(&format!("data/{old_cik}/"), &format!("data/{new_cik}/")),
                        );
                    }
                }
            }
            row
        };
        let alias = Value::Array((0..3).map(|_| relabeled("GOOG", None)).collect());
        validate_value(KRW_FILING_SEARCH_RESULT_V1, &alias).unwrap();
        validate_front_exchange(
            KRW_FILING_SEARCH_INPUT_V1,
            &input,
            KRW_FILING_SEARCH_RESULT_V1,
            &alias,
        )
        .unwrap();

        // Exact echo still binds.
        let echo = Value::Array((0..2).map(|_| relabeled("GOOGL", None)).collect());
        validate_front_exchange(
            KRW_FILING_SEARCH_INPUT_V1,
            &input,
            KRW_FILING_SEARCH_RESULT_V1,
            &echo,
        )
        .unwrap();

        // Empty result is vacuously bound.
        validate_front_exchange(
            KRW_FILING_SEARCH_INPUT_V1,
            &input,
            KRW_FILING_SEARCH_RESULT_V1,
            &Value::Array(Vec::new()),
        )
        .unwrap();

        // Two CIKs under one vendor label is two companies: fail closed.
        let two_companies = Value::Array(vec![
            relabeled("GOOG", None),
            relabeled("GOOG", Some("0000320193")),
        ]);
        assert!(
            validate_front_exchange(
                KRW_FILING_SEARCH_INPUT_V1,
                &input,
                KRW_FILING_SEARCH_RESULT_V1,
                &two_companies,
            )
            .is_err()
        );

        // Two vendor labels mix listing identities: fail closed.
        let two_labels = Value::Array(vec![relabeled("GOOG", None), relabeled("GOOGL", None)]);
        assert!(
            validate_front_exchange(
                KRW_FILING_SEARCH_INPUT_V1,
                &input,
                KRW_FILING_SEARCH_RESULT_V1,
                &two_labels,
            )
            .is_err()
        );

        // A wholly different single-company label is accepted BY DESIGN: the
        // binding authority for "which company" is the catalog's CIK
        // resolution of the requested ticker (server-side), not a client-side
        // relatedness table (none exists locally; maintaining one would be
        // over-engineering). This check proves the set is one coherent
        // company relabel, not zero companies or two.
        let cross_listing = Value::Array((0..2).map(|_| relabeled("MSFT", None)).collect());
        validate_front_exchange(
            KRW_FILING_SEARCH_INPUT_V1,
            &input,
            KRW_FILING_SEARCH_RESULT_V1,
            &cross_listing,
        )
        .unwrap();
    }

    #[test]
    fn exchange_rejects_cross_filing_identity_drift() {
        let input = fixture(KRW_FILING_GET_INPUT_V1);
        let output = fixture(KRW_FILING_METADATA_V1);
        validate_front_exchange(
            KRW_FILING_GET_INPUT_V1,
            &input,
            KRW_FILING_METADATA_V1,
            &output,
        )
        .unwrap();

        let mut drifted = output;
        drifted["filing_event_id"] = Value::String("33333333-3333-4333-8333-333333333333".into());
        validate_value(KRW_FILING_METADATA_V1, &drifted).unwrap();
        assert!(
            validate_front_exchange(
                KRW_FILING_GET_INPUT_V1,
                &input,
                KRW_FILING_METADATA_V1,
                &drifted,
            )
            .is_err()
        );

        let mut internally_drifted = fixture(KRW_FORM4_TRANSACTIONS_RESULT_V1);
        internally_drifted["citation"]["filing_event_id"] =
            Value::String("33333333-3333-4333-8333-333333333333".into());
        assert!(validate_value(KRW_FORM4_TRANSACTIONS_RESULT_V1, &internally_drifted).is_err());
    }

    #[test]
    fn document_and_section_reads_are_bound_to_prior_list_artifacts() {
        let documents = fixture(KRW_FILING_DOCUMENTS_RESULT_V1);
        let read_document = fixture(KRW_FILING_READ_DOCUMENT_INPUT_V1);
        validate_filing_document_membership(&documents, &read_document).unwrap();
        let mut unknown_document = read_document;
        unknown_document["document_key"] = Value::String("document:zzzzzzzzzzzzzzzzzzzzzz".into());
        assert!(validate_filing_document_membership(&documents, &unknown_document).is_err());

        let sections = fixture(KRW_FILING_SECTIONS_RESULT_V1);
        let read_section = fixture(KRW_FILING_READ_SECTION_INPUT_V1);
        validate_filing_section_membership(&sections, &read_section).unwrap();
        let mut cross_filing = read_section;
        cross_filing["filing_event_id"] =
            Value::String("33333333-3333-4333-8333-333333333333".into());
        assert!(validate_filing_section_membership(&sections, &cross_filing).is_err());
    }

    #[test]
    fn arrays_text_and_unknown_fields_fail_closed_at_consumer_bounds() {
        let mut feed = fixture(KRW_FEED_GET_ITEMS_RESULT_V1);
        let item = feed["items"][0].clone();
        feed["items"] = Value::Array(vec![item; 9]);
        assert!(validate_value(KRW_FEED_GET_ITEMS_RESULT_V1, &feed).is_err());

        let mut read = fixture(KRW_FILING_READ_SECTION_RESULT_V1);
        read["text"] = Value::String("가".repeat(120_001));
        assert!(validate_value(KRW_FILING_READ_SECTION_RESULT_V1, &read).is_err());

        let mut nested_extra = fixture(KRW_FILING_READ_DOCUMENT_RESULT_V1);
        nested_extra["document"]["raw_url"] = Value::String("https://attacker.invalid".into());
        assert!(validate_value(KRW_FILING_READ_DOCUMENT_RESULT_V1, &nested_extra).is_err());
    }
}
