//! Closed mappings and recovery-safe authorization for read-only
//! `krw-ontology-front` capabilities.

use std::collections::{BTreeMap, BTreeSet};

use krw_agent_contracts::{
    KRW_FEED_CONTEXT_INPUT_V2, KRW_FEED_CONTEXT_V2, KRW_FEED_GET_ITEMS_INPUT_V1,
    KRW_FEED_GET_ITEMS_RESULT_V1, KRW_FEED_LIST_ITEMS_INPUT_V1, KRW_FEED_LIST_ITEMS_RESULT_V1,
    KRW_FILING_BRIEF_INPUT_V1, KRW_FILING_BRIEF_RESULT_V1, KRW_FILING_DOCUMENTS_INPUT_V1,
    KRW_FILING_DOCUMENTS_RESULT_V1, KRW_FILING_GET_INPUT_V1, KRW_FILING_METADATA_V1,
    KRW_FILING_READ_DOCUMENT_INPUT_V1, KRW_FILING_READ_DOCUMENT_RESULT_V1,
    KRW_FILING_READ_SECTION_INPUT_V1, KRW_FILING_READ_SECTION_RESULT_V1,
    KRW_FILING_SEARCH_INPUT_V1, KRW_FILING_SEARCH_RESULT_V1, KRW_FILING_SECTIONS_INPUT_V1,
    KRW_FILING_SECTIONS_RESULT_V1, KRW_FORM4_TRANSACTIONS_INPUT_V1,
    KRW_FORM4_TRANSACTIONS_RESULT_V1, NORMALIZED_CAPABILITY_RESULT_V1, validate_front_exchange,
};
use krw_agent_evidence::{
    Answerability, Directness, EvidenceGrade, EvidenceRecord, EvidenceSource, NormalizedFact,
    PublicCitation,
};
use krw_agent_protocol::ContentHash;
use krw_agent_execution_contracts::DependencyFailure;
use krw_ontology_adapter::MappingContext;
use serde_json::{Map, Value, json};
use zeroize::Zeroizing;

use super::reject;

const MAX_VERIFIED_FILINGS: usize = 64;
const MAX_SECTION_MEMBERSHIPS: usize = 2_048;
const MAX_DOCUMENT_MEMBERSHIPS: usize = 1_024;
const MAX_NORMALIZED_RECORDS: usize = 256;
const TEXT_CHUNK_BYTES: usize = 24 * 1024;
const FORM4_CHUNK_BYTES: usize = 48 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FrontMapping {
    FeedListItems,
    FeedGetItems,
    FeedContext,
    FilingSearch,
    FilingMetadata,
    FilingBrief,
    FilingSections,
    FilingSectionText,
    FilingDocuments,
    FilingDocumentText,
    Form4Transactions,
}

impl FrontMapping {
    pub(crate) const fn input_contract(self) -> &'static str {
        match self {
            Self::FeedListItems => KRW_FEED_LIST_ITEMS_INPUT_V1,
            Self::FeedGetItems => KRW_FEED_GET_ITEMS_INPUT_V1,
            Self::FeedContext => KRW_FEED_CONTEXT_INPUT_V2,
            Self::FilingSearch => KRW_FILING_SEARCH_INPUT_V1,
            Self::FilingMetadata => KRW_FILING_GET_INPUT_V1,
            Self::FilingBrief => KRW_FILING_BRIEF_INPUT_V1,
            Self::FilingSections => KRW_FILING_SECTIONS_INPUT_V1,
            Self::FilingSectionText => KRW_FILING_READ_SECTION_INPUT_V1,
            Self::FilingDocuments => KRW_FILING_DOCUMENTS_INPUT_V1,
            Self::FilingDocumentText => KRW_FILING_READ_DOCUMENT_INPUT_V1,
            Self::Form4Transactions => KRW_FORM4_TRANSACTIONS_INPUT_V1,
        }
    }

    pub(crate) const fn output_contract(self) -> &'static str {
        match self {
            Self::FeedListItems => KRW_FEED_LIST_ITEMS_RESULT_V1,
            Self::FeedGetItems => KRW_FEED_GET_ITEMS_RESULT_V1,
            Self::FeedContext => KRW_FEED_CONTEXT_V2,
            Self::FilingSearch => KRW_FILING_SEARCH_RESULT_V1,
            Self::FilingMetadata => KRW_FILING_METADATA_V1,
            Self::FilingBrief => KRW_FILING_BRIEF_RESULT_V1,
            Self::FilingSections => KRW_FILING_SECTIONS_RESULT_V1,
            Self::FilingSectionText => KRW_FILING_READ_SECTION_RESULT_V1,
            Self::FilingDocuments => KRW_FILING_DOCUMENTS_RESULT_V1,
            Self::FilingDocumentText => KRW_FILING_READ_DOCUMENT_RESULT_V1,
            Self::Form4Transactions => KRW_FORM4_TRANSACTIONS_RESULT_V1,
        }
    }

    pub(crate) const fn output_contracts(self) -> &'static [&'static str] {
        match self {
            Self::FeedListItems => &[
                KRW_FEED_LIST_ITEMS_RESULT_V1,
                NORMALIZED_CAPABILITY_RESULT_V1,
            ],
            Self::FeedGetItems => &[
                KRW_FEED_GET_ITEMS_RESULT_V1,
                NORMALIZED_CAPABILITY_RESULT_V1,
            ],
            Self::FeedContext => &[KRW_FEED_CONTEXT_V2, NORMALIZED_CAPABILITY_RESULT_V1],
            Self::FilingSearch => &[KRW_FILING_SEARCH_RESULT_V1, NORMALIZED_CAPABILITY_RESULT_V1],
            Self::FilingMetadata => &[KRW_FILING_METADATA_V1, NORMALIZED_CAPABILITY_RESULT_V1],
            Self::FilingBrief => &[KRW_FILING_BRIEF_RESULT_V1, NORMALIZED_CAPABILITY_RESULT_V1],
            Self::FilingSections => &[
                KRW_FILING_SECTIONS_RESULT_V1,
                NORMALIZED_CAPABILITY_RESULT_V1,
            ],
            Self::FilingSectionText => &[
                KRW_FILING_READ_SECTION_RESULT_V1,
                NORMALIZED_CAPABILITY_RESULT_V1,
            ],
            Self::FilingDocuments => &[
                KRW_FILING_DOCUMENTS_RESULT_V1,
                NORMALIZED_CAPABILITY_RESULT_V1,
            ],
            Self::FilingDocumentText => &[
                KRW_FILING_READ_DOCUMENT_RESULT_V1,
                NORMALIZED_CAPABILITY_RESULT_V1,
            ],
            Self::Form4Transactions => &[
                KRW_FORM4_TRANSACTIONS_RESULT_V1,
                NORMALIZED_CAPABILITY_RESULT_V1,
            ],
        }
    }

    pub(crate) const fn produces_feed_evidence(self) -> bool {
        matches!(
            self,
            Self::FeedListItems | Self::FeedGetItems | Self::FeedContext
        )
    }
}

#[derive(Clone, Default)]
pub(crate) struct FrontRunState {
    /// Both map key and value are one-way hashes. Raw filing IDs, CIKs,
    /// accessions, section keys, and document keys never enter debug state.
    verified_filings: BTreeMap<String, ContentHash>,
    sections: BTreeSet<(String, String)>,
    documents: BTreeSet<(String, String)>,
}

impl std::fmt::Debug for FrontRunState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FrontRunState")
            .field("verified_filing_count", &self.verified_filings.len())
            .field("section_membership_count", &self.sections.len())
            .field("document_membership_count", &self.documents.len())
            .finish()
    }
}

impl FrontRunState {
    pub(crate) fn authorize(
        &self,
        mapping: FrontMapping,
        arguments: &Value,
    ) -> Result<(), DependencyFailure> {
        match mapping {
            FrontMapping::FeedListItems
            | FrontMapping::FeedGetItems
            | FrontMapping::FeedContext
            | FrontMapping::FilingSearch
            | FrontMapping::FilingMetadata => Ok(()),
            FrontMapping::FilingBrief
            | FrontMapping::FilingSections
            | FrontMapping::FilingDocuments
            | FrontMapping::Form4Transactions => {
                let filing = filing_key_from_input(arguments)?;
                self.require_verified(&filing)
            }
            FrontMapping::FilingSectionText => {
                let filing = filing_key_from_input(arguments)?;
                self.require_verified(&filing)?;
                let section = member_key(arguments, "section_key")?;
                if self.sections.contains(&(filing, section)) {
                    Ok(())
                } else {
                    Err(reject(
                        "filing_section_not_authorized",
                        "section was not present in the prior committed canonical list",
                    ))
                }
            }
            FrontMapping::FilingDocumentText => {
                let filing = filing_key_from_input(arguments)?;
                self.require_verified(&filing)?;
                let document = member_key(arguments, "document_key")?;
                if self.documents.contains(&(filing, document)) {
                    Ok(())
                } else {
                    Err(reject(
                        "filing_document_not_authorized",
                        "document was not readable in the prior committed canonical list",
                    ))
                }
            }
        }
    }

    pub(crate) fn validate_identity(
        &self,
        mapping: FrontMapping,
        payload: &Value,
    ) -> Result<(), DependencyFailure> {
        let Some(metadata) = filing_metadata(mapping, payload)? else {
            return Ok(());
        };
        if mapping == FrontMapping::FilingMetadata || mapping == FrontMapping::FilingSearch {
            return Ok(());
        }
        let filing = filing_key(metadata)?;
        let observed = filing_identity(metadata)?;
        match self.verified_filings.get(&filing) {
            Some(expected) if expected == &observed => Ok(()),
            Some(_) => Err(reject(
                "filing_identity_changed",
                "CIK/accession/filing identity differs from the committed metadata anchor",
            )),
            None => Err(reject(
                "filing_identity_unverified",
                "filing content requires a prior committed get_filing identity anchor",
            )),
        }
    }

    pub(crate) fn apply_committed(
        &mut self,
        mapping: FrontMapping,
        arguments: &Value,
        payload: &Value,
    ) -> Result<(), DependencyFailure> {
        self.authorize(mapping, arguments)?;
        self.validate_identity(mapping, payload)?;
        let mut next = self.clone();
        match mapping {
            FrontMapping::FilingMetadata => {
                let metadata = payload
                    .as_object()
                    .ok_or_else(|| reject("filing_metadata_shape", "metadata must be an object"))?;
                let filing = filing_key(metadata)?;
                let identity = filing_identity(metadata)?;
                if let Some(existing) = next.verified_filings.get(&filing) {
                    if existing != &identity {
                        return Err(reject(
                            "filing_identity_conflict",
                            "committed metadata conflicts with the existing filing anchor",
                        ));
                    }
                } else {
                    if next.verified_filings.len() >= MAX_VERIFIED_FILINGS {
                        return Err(reject(
                            "filing_identity_limit",
                            "run exceeded the fixed verified filing bound",
                        ));
                    }
                    next.verified_filings.insert(filing, identity);
                }
            }
            FrontMapping::FilingSections => {
                let metadata = required_object(payload, "filing")?;
                let filing = filing_key(metadata)?;
                next.sections.retain(|(owner, _)| owner != &filing);
                for section in required_array(payload, "sections")? {
                    let key = member_key(section, "key")?;
                    next.sections.insert((filing.clone(), key));
                }
                if next.sections.len() > MAX_SECTION_MEMBERSHIPS {
                    return Err(reject(
                        "filing_section_membership_limit",
                        "run exceeded the fixed section membership bound",
                    ));
                }
            }
            FrontMapping::FilingDocuments => {
                let metadata = required_object(payload, "filing")?;
                let filing = filing_key(metadata)?;
                next.documents.retain(|(owner, _)| owner != &filing);
                for document in required_array(payload, "documents")? {
                    if document.get("readable").and_then(Value::as_bool) == Some(true) {
                        let key = member_key(document, "document_key")?;
                        next.documents.insert((filing.clone(), key));
                    }
                }
                if next.documents.len() > MAX_DOCUMENT_MEMBERSHIPS {
                    return Err(reject(
                        "filing_document_membership_limit",
                        "run exceeded the fixed document membership bound",
                    ));
                }
            }
            FrontMapping::FeedListItems
            | FrontMapping::FeedGetItems
            | FrontMapping::FeedContext
            | FrontMapping::FilingSearch
            | FrontMapping::FilingBrief
            | FrontMapping::FilingSectionText
            | FrontMapping::FilingDocumentText
            | FrontMapping::Form4Transactions => {}
        }
        *self = next;
        Ok(())
    }

    fn require_verified(&self, filing: &str) -> Result<(), DependencyFailure> {
        if self.verified_filings.contains_key(filing) {
            Ok(())
        } else {
            Err(reject(
                "filing_identity_unverified",
                "operation requires a prior committed get_filing identity anchor",
            ))
        }
    }
}

pub(crate) fn validate_exchange(
    mapping: FrontMapping,
    arguments: &Value,
    payload: &Value,
) -> Result<(), DependencyFailure> {
    validate_front_exchange(
        mapping.input_contract(),
        arguments,
        mapping.output_contract(),
        payload,
    )
    .map_err(|error| reject("front_exchange_invalid", format!("{error:?}")))
}

pub(crate) fn map_evidence(
    mapping: FrontMapping,
    payload: &Value,
    context: &MappingContext,
) -> Result<(Vec<EvidenceRecord>, Option<Answerability>), DependencyFailure> {
    if mapping.produces_feed_evidence() {
        let records = map_feed(payload, context)?;
        return Ok((records, Some(Answerability::QualifiedOnly)));
    }
    if mapping == FrontMapping::FilingSectionText {
        let records = map_filing_text(payload, context, true)?;
        let answerability = if records.is_empty() {
            Answerability::QualifiedOnly
        } else {
            Answerability::StrongAllowed
        };
        return Ok((records, Some(answerability)));
    }
    if mapping == FrontMapping::FilingDocumentText {
        let records = map_filing_text(payload, context, false)?;
        let answerability = if records.is_empty() {
            Answerability::QualifiedOnly
        } else {
            Answerability::StrongAllowed
        };
        return Ok((records, Some(answerability)));
    }
    if mapping == FrontMapping::Form4Transactions {
        let records = map_form4(payload, context)?;
        let answerability = if records.is_empty() {
            Answerability::QualifiedOnly
        } else {
            Answerability::StrongAllowed
        };
        return Ok((records, Some(answerability)));
    }
    Ok((Vec::new(), None))
}

fn map_feed(
    payload: &Value,
    context: &MappingContext,
) -> Result<Vec<EvidenceRecord>, DependencyFailure> {
    let items = required_array(payload, "items")?;
    let mut records = Vec::with_capacity(items.len());
    for item in items {
        let object = item
            .as_object()
            .ok_or_else(|| reject("front_feed_shape", "feed item must be an object"))?;
        let source_count = object
            .get("source_count")
            .and_then(Value::as_u64)
            .ok_or_else(|| reject("front_feed_shape", "feed source count is missing"))?;
        let title = public_line(required_str(object, "title_ko")?, 512, "Stored feed event");
        let entity = object
            .get("entities")
            .and_then(Value::as_array)
            .and_then(|entities| {
                entities.iter().find_map(|entity| {
                    entity
                        .get("ticker")
                        .and_then(Value::as_str)
                        .map(|ticker| public_line(ticker, 128, "company"))
                })
            });
        let fact_value = json!({
            "title": object.get("title_ko").cloned().unwrap_or(Value::Null),
            "summary": object.get("summary_ko").cloned().unwrap_or(Value::Null),
            "impact_path": object.get("impact_path").cloned().unwrap_or_else(|| json!([])),
            "confirmed_facts": object.get("confirmed_facts").cloned().unwrap_or_else(|| json!([])),
            "unconfirmed_facts": object.get("unconfirmed_facts").cloned().unwrap_or_else(|| json!([])),
            "status_labels": object.get("status_labels").cloned().unwrap_or_else(|| json!([])),
            "risk_level": object.get("risk_level").cloned().unwrap_or(Value::Null),
            "source_count": source_count,
        });
        ensure_fact_bound(&fact_value)?;
        let content_hash = canonical_hash(item)?;
        records.push(evidence_record(
            context,
            content_hash,
            RecordMetadata {
                entity: entity.clone(),
                period: None,
                as_of: optional_map_string(object, "published_at")
                    .or_else(|| optional_map_string(object, "updated_at")),
                directness: if source_count == 0 {
                    Directness::Unverified
                } else {
                    Directness::Related
                },
                grade: if source_count == 0 {
                    EvidenceGrade::Unverified
                } else {
                    EvidenceGrade::Medium
                },
                strong_claim_allowed: false,
                title,
                document_type: Some("stored_feed_event".into()),
            },
            NormalizedFact {
                subject: entity.unwrap_or_else(|| "company".into()),
                predicate: "feed_event_context".into(),
                value: fact_value,
                unit: None,
                period: None,
            },
        ));
    }
    ensure_record_count(&records)?;
    Ok(records)
}

fn map_filing_text(
    payload: &Value,
    context: &MappingContext,
    section: bool,
) -> Result<Vec<EvidenceRecord>, DependencyFailure> {
    let filing = required_object(payload, "filing")?;
    let ticker = public_line(required_str(filing, "ticker")?, 128, "company");
    let form = public_line(required_str(filing, "form_type")?, 128, "SEC filing");
    let period = optional_map_string(filing, "report_date")
        .or_else(|| optional_map_string(filing, "filing_date"));
    let identity = filing_identity(filing)?;
    let text = payload
        .get("text")
        .and_then(Value::as_str)
        .ok_or_else(|| reject("filing_text_shape", "filing text is missing"))?;
    if text.is_empty() {
        return Ok(Vec::new());
    }
    let descriptor_field = if section { "section" } else { "document" };
    let descriptor = required_object(payload, descriptor_field)?;
    let label_field = if section { "heading" } else { "document_name" };
    let label = public_line(
        required_str(descriptor, label_field)?,
        360,
        "verified filing content",
    );
    let title = public_line(
        &format!("{ticker} {form} — {label}"),
        512,
        "Verified SEC filing content",
    );
    let as_of = payload
        .get("citation")
        .and_then(|citation| optional_string(citation, "fetched_at"))
        .or_else(|| optional_map_string(filing, "accepted_at"));
    let mut records = Vec::new();
    for (index, chunk) in utf8_chunks(text, TEXT_CHUNK_BYTES).into_iter().enumerate() {
        let fact_value = Value::String(chunk.to_owned());
        ensure_fact_bound(&fact_value)?;
        let semantic = json!({
            "kind": if section { "filing_section" } else { "filing_document" },
            "filing_identity": identity,
            "part": index,
            "text": chunk,
        });
        records.push(evidence_record(
            context,
            canonical_hash(&semantic)?,
            RecordMetadata {
                entity: Some(ticker.clone()),
                period: period.clone(),
                as_of: as_of.clone(),
                directness: Directness::Direct,
                grade: EvidenceGrade::Strong,
                strong_claim_allowed: true,
                title: title.clone(),
                document_type: Some(form.clone()),
            },
            NormalizedFact {
                subject: ticker.clone(),
                predicate: if section {
                    "filing_section_text".into()
                } else {
                    "filing_document_text".into()
                },
                value: fact_value,
                unit: None,
                period: period.clone(),
            },
        ));
    }
    ensure_record_count(&records)?;
    Ok(records)
}

fn map_form4(
    payload: &Value,
    context: &MappingContext,
) -> Result<Vec<EvidenceRecord>, DependencyFailure> {
    let filing = required_object(payload, "filing")?;
    let ticker = public_line(required_str(filing, "ticker")?, 128, "company");
    let form = public_line(required_str(filing, "form_type")?, 128, "Form 4");
    let period = optional_map_string(filing, "filing_date");
    let as_of = payload
        .get("citation")
        .and_then(|citation| optional_string(citation, "fetched_at"));
    let identity = filing_identity(filing)?;
    let title = public_line(
        &format!("{ticker} verified {form} ownership filing"),
        512,
        "Verified Form 4 filing",
    );
    let mut groups = Vec::<(&str, Vec<Value>)>::new();
    if let Some(summary) = payload.get("summary").filter(|value| !value.is_null()) {
        groups.push((
            "form4_summary",
            vec![sanitize_form4(summary, Form4Kind::Summary)?],
        ));
    }
    groups.push((
        "form4_reporting_owners",
        required_array(payload, "reporting_owners")?
            .iter()
            .map(|value| sanitize_form4(value, Form4Kind::Owner))
            .collect::<Result<Vec<_>, _>>()?,
    ));
    groups.push((
        "form4_transactions",
        required_array(payload, "transactions")?
            .iter()
            .map(|value| sanitize_form4(value, Form4Kind::Transaction))
            .collect::<Result<Vec<_>, _>>()?,
    ));

    let mut records = Vec::new();
    for (predicate, values) in groups {
        for (index, chunk) in pack_values(values, FORM4_CHUNK_BYTES)?
            .into_iter()
            .enumerate()
        {
            if chunk.is_empty() {
                continue;
            }
            let fact_value = Value::Array(chunk);
            ensure_fact_bound(&fact_value)?;
            let semantic = json!({
                "kind": predicate,
                "filing_identity": identity,
                "part": index,
                "value": fact_value,
            });
            records.push(evidence_record(
                context,
                canonical_hash(&semantic)?,
                RecordMetadata {
                    entity: Some(ticker.clone()),
                    period: period.clone(),
                    as_of: as_of.clone(),
                    directness: Directness::Direct,
                    grade: EvidenceGrade::Strong,
                    strong_claim_allowed: true,
                    title: title.clone(),
                    document_type: Some(form.clone()),
                },
                NormalizedFact {
                    subject: ticker.clone(),
                    predicate: predicate.into(),
                    value: semantic
                        .get("value")
                        .cloned()
                        .ok_or_else(|| reject("form4_mapping", "normalized value is missing"))?,
                    unit: None,
                    period: period.clone(),
                },
            ));
        }
    }
    ensure_record_count(&records)?;
    Ok(records)
}

#[derive(Clone, Copy)]
enum Form4Kind {
    Summary,
    Owner,
    Transaction,
}

fn sanitize_form4(value: &Value, kind: Form4Kind) -> Result<Value, DependencyFailure> {
    let object = value
        .as_object()
        .ok_or_else(|| reject("form4_mapping", "Form 4 item must be an object"))?;
    let fields: &[&str] = match kind {
        Form4Kind::Summary => &[
            "reporting_owner_count",
            "transaction_count",
            "reported_purchase_count",
            "reported_sale_count",
            "reported_purchase_shares",
            "reported_sale_shares",
            "reported_purchase_value",
            "reported_sale_value",
            "net_reported_shares",
            "tenb5_one_status",
            "has_reported_open_market_trade",
            "source_xml_version",
        ],
        Form4Kind::Owner => &[
            "owner_name",
            "is_director",
            "is_officer",
            "is_ten_percent_owner",
            "is_other",
            "officer_title",
        ],
        Form4Kind::Transaction => &[
            "security_kind",
            "security_title",
            "transaction_date",
            "transaction_code",
            "equity_swap_involved",
            "transaction_shares",
            "transaction_price_per_share",
            "acquired_disposed_code",
            "shares_owned_following",
            "direct_indirect_code",
            "nature_of_ownership",
            "reported_transaction_value",
            "reported_value_basis",
        ],
    };
    let mut normalized = Map::new();
    for field in fields {
        let field_value = object
            .get(*field)
            .ok_or_else(|| reject("form4_mapping", "Form 4 canonical field is missing"))?;
        normalized.insert((*field).to_owned(), field_value.clone());
    }
    Ok(Value::Object(normalized))
}

fn pack_values(
    values: Vec<Value>,
    maximum_bytes: usize,
) -> Result<Vec<Vec<Value>>, DependencyFailure> {
    let mut chunks = Vec::new();
    let mut current = Vec::new();
    // A canonical JSON array is exactly `[` + canonical elements separated by
    // commas + `]`; tracking lengths avoids repeatedly cloning/serializing an
    // ever-growing Form 4 chunk.
    let mut current_bytes = 2_usize;
    for value in values {
        let value_bytes = canonical_len(&value)?;
        if value_bytes.saturating_add(2) > maximum_bytes {
            return Err(reject(
                "normalized_fact_limit",
                "one normalized Form 4 item exceeds the fixed fact bound",
            ));
        }
        let separator_bytes = usize::from(!current.is_empty());
        if current_bytes
            .saturating_add(separator_bytes)
            .saturating_add(value_bytes)
            > maximum_bytes
        {
            chunks.push(std::mem::take(&mut current));
            current_bytes = 2;
        }
        current_bytes = current_bytes
            .saturating_add(usize::from(!current.is_empty()))
            .saturating_add(value_bytes);
        current.push(value);
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    Ok(chunks)
}

struct RecordMetadata {
    entity: Option<String>,
    period: Option<String>,
    as_of: Option<String>,
    directness: Directness,
    grade: EvidenceGrade,
    strong_claim_allowed: bool,
    title: String,
    document_type: Option<String>,
}

fn evidence_record(
    context: &MappingContext,
    content_hash: ContentHash,
    metadata: RecordMetadata,
    fact: NormalizedFact,
) -> EvidenceRecord {
    let digest = content_hash
        .as_str()
        .strip_prefix("sha256:")
        .unwrap_or(content_hash.as_str());
    EvidenceRecord {
        evidence_id: format!("front:{digest}"),
        content_hash,
        source: EvidenceSource {
            capability_id: context.capability_id.clone(),
            action_key: context.action_key.clone(),
            server_build: context.server_build.clone(),
            normalized_contract_hash: context.normalized_contract_hash.clone(),
            server_schema_bundle_hash: context.server_schema_bundle_hash.clone(),
            data_release_hash: context.data_release_hash.clone(),
        },
        scope: context.scope.clone(),
        entity: metadata.entity,
        period: metadata.period,
        as_of: metadata.as_of,
        directness: metadata.directness,
        grade: metadata.grade,
        strong_claim_allowed: metadata.strong_claim_allowed,
        payload_ref: context.payload_ref.clone(),
        citation: PublicCitation {
            title: metadata.title,
            document_type: metadata.document_type,
            period: fact.period.clone(),
        },
        facts: vec![fact],
        supports: Vec::new(),
        refutes: Vec::new(),
        qualifies: Vec::new(),
        source_object_ids: Vec::new(),
    }
}

fn filing_metadata(
    mapping: FrontMapping,
    payload: &Value,
) -> Result<Option<&Map<String, Value>>, DependencyFailure> {
    match mapping {
        FrontMapping::FilingMetadata => {
            Ok(Some(payload.as_object().ok_or_else(|| {
                reject("filing_metadata_shape", "metadata must be an object")
            })?))
        }
        FrontMapping::FilingSearch => Ok(None),
        FrontMapping::FilingBrief
        | FrontMapping::FilingSections
        | FrontMapping::FilingSectionText
        | FrontMapping::FilingDocuments
        | FrontMapping::FilingDocumentText
        | FrontMapping::Form4Transactions => Ok(Some(required_object(payload, "filing")?)),
        FrontMapping::FeedListItems | FrontMapping::FeedGetItems | FrontMapping::FeedContext => {
            Ok(None)
        }
    }
}

fn filing_identity(metadata: &Map<String, Value>) -> Result<ContentHash, DependencyFailure> {
    let identity = json!({
        "filing_event_id": required_str(metadata, "filing_event_id")?,
        "ticker": required_str(metadata, "ticker")?,
        "cik": required_str(metadata, "cik")?,
        "accession_number": required_str(metadata, "accession_number")?,
        "form_type": required_str(metadata, "form_type")?,
        "filing_date": required_str(metadata, "filing_date")?,
        "report_date": metadata.get("report_date").cloned().unwrap_or(Value::Null),
        "filing_detail_url": required_str(metadata, "filing_detail_url")?,
        "primary_document_url": metadata.get("primary_document_url").cloned().unwrap_or(Value::Null),
    });
    canonical_hash(&identity)
}

fn filing_key(metadata: &Map<String, Value>) -> Result<String, DependencyFailure> {
    Ok(
        ContentHash::sha256(required_str(metadata, "filing_event_id")?)
            .as_str()
            .to_owned(),
    )
}

fn filing_key_from_input(arguments: &Value) -> Result<String, DependencyFailure> {
    let object = arguments
        .as_object()
        .ok_or_else(|| reject("filing_input_shape", "filing arguments must be an object"))?;
    filing_key(object)
}

fn member_key(value: &Value, field: &str) -> Result<String, DependencyFailure> {
    let object = value
        .as_object()
        .ok_or_else(|| reject("filing_member_shape", "filing member must be an object"))?;
    Ok(ContentHash::sha256(required_str(object, field)?)
        .as_str()
        .to_owned())
}

fn required_object<'a>(
    value: &'a Value,
    field: &str,
) -> Result<&'a Map<String, Value>, DependencyFailure> {
    value
        .get(field)
        .and_then(Value::as_object)
        .ok_or_else(|| reject("front_output_shape", "canonical object field is missing"))
}

fn required_array<'a>(value: &'a Value, field: &str) -> Result<&'a [Value], DependencyFailure> {
    value
        .get(field)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or_else(|| reject("front_output_shape", "canonical array field is missing"))
}

fn required_str<'a>(
    object: &'a Map<String, Value>,
    field: &str,
) -> Result<&'a str, DependencyFailure> {
    object
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| reject("front_output_shape", "canonical string field is missing"))
}

fn optional_string(value: &Value, field: &str) -> Option<String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(|value| public_line(value, 128, "unknown"))
}

fn optional_map_string(value: &Map<String, Value>, field: &str) -> Option<String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(|value| public_line(value, 128, "unknown"))
}

fn canonical_hash(value: &Value) -> Result<ContentHash, DependencyFailure> {
    let bytes = Zeroizing::new(
        serde_jcs::to_vec(value)
            .map_err(|error| reject("front_canonicalization", format!("{error:?}")))?,
    );
    Ok(ContentHash::sha256(bytes.as_slice()))
}

fn canonical_len(value: &Value) -> Result<usize, DependencyFailure> {
    Ok(Zeroizing::new(
        serde_jcs::to_vec(value)
            .map_err(|error| reject("front_canonicalization", format!("{error:?}")))?,
    )
    .len())
}

fn ensure_fact_bound(value: &Value) -> Result<(), DependencyFailure> {
    if canonical_len(value)? > 60 * 1024 {
        Err(reject(
            "normalized_fact_limit",
            "normalized fact exceeds the fixed value bound",
        ))
    } else {
        Ok(())
    }
}

fn ensure_record_count(records: &[EvidenceRecord]) -> Result<(), DependencyFailure> {
    if records.len() > MAX_NORMALIZED_RECORDS {
        Err(reject(
            "front_evidence_limit",
            "front projection exceeds the fixed evidence record bound",
        ))
    } else {
        Ok(())
    }
}

fn public_line(value: &str, maximum_bytes: usize, fallback: &str) -> String {
    let mut output = String::new();
    let mut pending_space = false;
    for character in value.chars() {
        if character.is_control() || character.is_whitespace() {
            pending_space = !output.is_empty();
            continue;
        }
        if pending_space && output.len() < maximum_bytes {
            output.push(' ');
        }
        pending_space = false;
        if output.len() + character.len_utf8() > maximum_bytes {
            break;
        }
        output.push(character);
    }
    if output.trim().is_empty() {
        fallback.to_owned()
    } else {
        output
    }
}

fn utf8_chunks(value: &str, maximum_bytes: usize) -> Vec<&str> {
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < value.len() {
        let mut end = (start + maximum_bytes).min(value.len());
        while end > start && !value.is_char_boundary(end) {
            end -= 1;
        }
        if end == start {
            end = value[start..]
                .char_indices()
                .nth(1)
                .map_or(value.len(), |(offset, _)| start + offset);
        }
        chunks.push(&value[start..end]);
        start = end;
    }
    chunks
}
