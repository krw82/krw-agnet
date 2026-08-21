//! Immutable evidence ledger, typed answer IR, and deterministic verification.

use std::collections::{BTreeMap, BTreeSet};

use krw_agent_protocol::{AuthScope, ContentHash};
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};
use serde_json::Value;
use thiserror::Error;

const MAX_EVIDENCE_RECORD_BYTES: usize = 256 * 1024;
const MAX_FACTS_PER_RECORD: usize = 128;
const MAX_RELATIONS_PER_RECORD: usize = 128;
const MAX_CALCULATION_INPUTS: usize = 128;

/// Bounded answer-shape limits shared with the run-engine answer sanitizer.
/// The sanitizer must truncate to exactly these limits, so they are public
/// while every other bound stays crate-private.
pub const MAX_ANSWER_SECTIONS: usize = 16;
pub const MAX_ANSWER_CLAIMS: usize = 64;
pub const MAX_ANSWER_CALCULATIONS: usize = 64;
pub const MAX_CLAIMS_PER_SECTION: usize = 64;
const MAX_EVIDENCE_PER_CLAIM: usize = 64;
const MAX_GOALS_PER_CLAIM: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Directness {
    Unverified,
    Related,
    MetricLineage,
    Direct,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceGrade {
    Unverified,
    Weak,
    Medium,
    Strong,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Answerability {
    NotAnswerable,
    #[default]
    QualifiedOnly,
    StrongAllowed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceSource {
    pub capability_id: String,
    pub action_key: String,
    pub server_build: String,
    /// Hash of the normalized kernel output contract used for this record.
    pub normalized_contract_hash: ContentHash,
    /// Independent readiness fingerprint of the remote MCP schema bundle.
    pub server_schema_bundle_hash: ContentHash,
    pub data_release_hash: ContentHash,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceScope {
    pub auth_scope: AuthScope,
    /// A pseudonymous scope key. Never a raw tenant or principal identifier.
    pub scope_hash: ContentHash,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicCitation {
    pub title: String,
    pub document_type: Option<String>,
    pub period: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormalizedFact {
    pub subject: String,
    pub predicate: String,
    pub value: Value,
    pub unit: Option<String>,
    pub period: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceRecord {
    pub evidence_id: String,
    pub content_hash: ContentHash,
    pub source: EvidenceSource,
    pub scope: EvidenceScope,
    pub entity: Option<String>,
    pub period: Option<String>,
    pub as_of: Option<String>,
    pub directness: Directness,
    pub grade: EvidenceGrade,
    pub strong_claim_allowed: bool,
    pub payload_ref: ContentHash,
    pub citation: PublicCitation,
    pub facts: Vec<NormalizedFact>,
    #[serde(default)]
    pub supports: Vec<String>,
    #[serde(default)]
    pub refutes: Vec<String>,
    #[serde(default)]
    pub qualifies: Vec<String>,
    #[serde(default)]
    pub source_object_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
#[allow(clippy::struct_field_names)]
struct EvidenceIdentity {
    content_hash: ContentHash,
    data_release_hash: ContentHash,
    scope_hash: ContentHash,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceLedger {
    records: BTreeMap<String, EvidenceRecord>,
    calculations: BTreeMap<String, Calculation>,
    superseded_by: BTreeMap<String, String>,
    answerability: Answerability,
    #[serde(skip)]
    identities: BTreeMap<EvidenceIdentity, String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EvidenceLedgerWire {
    records: BTreeMap<String, EvidenceRecord>,
    calculations: BTreeMap<String, Calculation>,
    superseded_by: BTreeMap<String, String>,
    answerability: Answerability,
}

impl<'de> Deserialize<'de> for EvidenceLedger {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = EvidenceLedgerWire::deserialize(deserializer)?;
        let mut ledger = Self::default();

        for (expected_id, record) in wire.records {
            if expected_id != record.evidence_id {
                return Err(D::Error::custom(format!(
                    "evidence map key {expected_id} does not match record id {}",
                    record.evidence_id
                )));
            }
            match ledger.append(record).map_err(D::Error::custom)? {
                AppendOutcome::Inserted(_) => {}
                AppendOutcome::Deduplicated(existing_id) => {
                    return Err(D::Error::custom(format!(
                        "duplicate evidence identity already stored as {existing_id}"
                    )));
                }
            }
        }

        for (expected_id, calculation) in wire.calculations {
            if expected_id != calculation.calculation_id {
                return Err(D::Error::custom(format!(
                    "calculation map key {expected_id} does not match calculation id {}",
                    calculation.calculation_id
                )));
            }
            ledger
                .append_calculation(calculation)
                .map_err(D::Error::custom)?;
        }

        for (older, newer) in wire.superseded_by {
            ledger
                .mark_superseded(&older, &newer)
                .map_err(D::Error::custom)?;
        }
        ledger.answerability = wire.answerability;
        Ok(ledger)
    }
}

impl EvidenceLedger {
    pub fn from_records(
        records: impl IntoIterator<Item = EvidenceRecord>,
    ) -> Result<Self, EvidenceError> {
        let mut ledger = Self::default();
        for record in records {
            ledger.append(record)?;
        }
        Ok(ledger)
    }

    pub fn append(&mut self, record: EvidenceRecord) -> Result<AppendOutcome, EvidenceError> {
        if record.evidence_id.trim().is_empty() {
            return Err(EvidenceError::EmptyEvidenceId);
        }
        if !valid_identifier(&record.evidence_id)
            || !safe_bounded_text(&record.citation.title, 512, false)
            || !safe_bounded_optional(record.citation.document_type.as_deref(), 128)
            || !safe_bounded_optional(record.citation.period.as_deref(), 128)
            || !safe_bounded_optional(record.entity.as_deref(), 128)
            || !safe_bounded_optional(record.period.as_deref(), 128)
            || !safe_bounded_optional(record.as_of.as_deref(), 128)
            || record.facts.len() > MAX_FACTS_PER_RECORD
            || record.supports.len() > MAX_RELATIONS_PER_RECORD
            || record.refutes.len() > MAX_RELATIONS_PER_RECORD
            || record.qualifies.len() > MAX_RELATIONS_PER_RECORD
            || record.source_object_ids.len() > MAX_RELATIONS_PER_RECORD
            || record
                .supports
                .iter()
                .chain(&record.refutes)
                .chain(&record.qualifies)
                .any(|value| !valid_identifier(value))
            || record.facts.iter().any(|fact| {
                !safe_bounded_text(&fact.subject, 256, false)
                    || !safe_bounded_text(&fact.predicate, 128, false)
                    || !safe_bounded_optional(fact.unit.as_deref(), 64)
                    || !safe_bounded_optional(fact.period.as_deref(), 128)
                    || serde_json::to_vec(&fact.value).map_or(true, |value| value.len() > 64 * 1024)
            })
            || serde_json::to_vec(&record)
                .map_or(true, |bytes| bytes.len() > MAX_EVIDENCE_RECORD_BYTES)
        {
            return Err(EvidenceError::InvalidEvidenceMetadata(record.evidence_id));
        }
        let identity = EvidenceIdentity {
            content_hash: record.content_hash.clone(),
            data_release_hash: record.source.data_release_hash.clone(),
            scope_hash: record.scope.scope_hash.clone(),
        };
        if let Some(existing_id) = self.identities.get(&identity) {
            return Ok(AppendOutcome::Deduplicated(existing_id.clone()));
        }
        if self.records.contains_key(&record.evidence_id) {
            // The model sometimes reuses an evidence_id for non-identical
            // content within the same run. Rather than failing the run
            // terminally, mint a deterministic suffix from the content hash
            // so the record is still inserted under a unique key.
            let suffix = record.content_hash.as_str();
            let suffix_short = &suffix[suffix.len().saturating_sub(12)..];
            let resolved_id = format!("{}_{}", record.evidence_id, suffix_short);
            let mut resolved = record;
            resolved.evidence_id = resolved_id;
            let evidence_id = resolved.evidence_id.clone();
            self.records.insert(evidence_id.clone(), resolved);
            self.identities.insert(identity, evidence_id.clone());
            return Ok(AppendOutcome::Inserted(evidence_id));
        }
        let evidence_id = record.evidence_id.clone();
        self.records.insert(evidence_id.clone(), record);
        self.identities.insert(identity, evidence_id.clone());
        Ok(AppendOutcome::Inserted(evidence_id))
    }

    pub fn set_answerability(&mut self, answerability: Answerability) {
        self.answerability = answerability;
    }

    /// A precise supplemental read can recover a directly supported fact
    /// after an earlier broad plan reported that it could not yet support a
    /// conclusion. Preserve the conservative boundary: this only reopens the
    /// ledger for qualified claims and can never grant strong-claim permission.
    ///
    /// Callers use this only for supplemental evidence mappings whose result
    /// does not carry a newer, authoritative plan-level answerability verdict.
    pub fn reopen_qualified_after_substantive_supplement<'a>(
        &mut self,
        records: impl IntoIterator<Item = &'a EvidenceRecord>,
    ) {
        if self.answerability != Answerability::NotAnswerable {
            return;
        }
        let has_substantive_fact = records.into_iter().any(|record| {
            matches!(
                record.directness,
                Directness::Direct | Directness::MetricLineage
            ) && !record.facts.is_empty()
        });
        if has_substantive_fact {
            self.answerability = Answerability::QualifiedOnly;
        }
    }

    pub fn append_calculation(&mut self, calculation: Calculation) -> Result<bool, EvidenceError> {
        if calculation.calculation_id.trim().is_empty() {
            return Err(EvidenceError::EmptyCalculationId);
        }
        if !valid_identifier(&calculation.calculation_id)
            || !safe_bounded_text(&calculation.expression, 256, false)
            || calculation.input_evidence_ids.len() > MAX_CALCULATION_INPUTS
            || calculation
                .input_evidence_ids
                .iter()
                .any(|value| !valid_identifier(value))
            || !safe_bounded_optional(calculation.unit.as_deref(), 64)
            || !safe_bounded_optional(calculation.rounding.as_deref(), 64)
            || !safe_bounded_optional(calculation.subject.as_deref(), 256)
            || !safe_bounded_optional(calculation.metric.as_deref(), 128)
            || !safe_bounded_optional(calculation.period.as_deref(), 128)
            || !safe_bounded_optional(calculation.currency.as_deref(), 16)
            || serde_json::to_vec(&calculation.output).map_or(true, |bytes| bytes.len() > 64 * 1024)
        {
            return Err(EvidenceError::InvalidCalculationMetadata(
                calculation.calculation_id,
            ));
        }
        if calculation.input_evidence_ids.is_empty() {
            return Err(EvidenceError::CalculationWithoutEvidence(
                calculation.calculation_id,
            ));
        }
        for evidence_id in &calculation.input_evidence_ids {
            if self.active(evidence_id).is_none() {
                return Err(EvidenceError::UnknownEvidence(evidence_id.clone()));
            }
        }
        if let Some(existing) = self.calculations.get(&calculation.calculation_id) {
            if existing == &calculation {
                return Ok(false);
            }
            return Err(EvidenceError::CalculationConflict(
                calculation.calculation_id,
            ));
        }
        self.calculations
            .insert(calculation.calculation_id.clone(), calculation);
        Ok(true)
    }

    pub fn extend_calculations(
        &mut self,
        calculations: impl IntoIterator<Item = Calculation>,
    ) -> Result<(), EvidenceError> {
        for calculation in calculations {
            self.append_calculation(calculation)?;
        }
        Ok(())
    }

    pub fn calculation(&self, calculation_id: &str) -> Option<&Calculation> {
        self.calculations.get(calculation_id)
    }

    pub fn answerability(&self) -> Answerability {
        self.answerability
    }

    pub fn mark_superseded(&mut self, older: &str, newer: &str) -> Result<(), EvidenceError> {
        if older == newer {
            return Err(EvidenceError::SupersessionCycle(older.to_owned()));
        }
        if !self.records.contains_key(older) {
            return Err(EvidenceError::UnknownEvidence(older.to_owned()));
        }
        if !self.records.contains_key(newer) {
            return Err(EvidenceError::UnknownEvidence(newer.to_owned()));
        }
        let mut cursor = newer;
        while let Some(next) = self.superseded_by.get(cursor) {
            if next == older {
                return Err(EvidenceError::SupersessionCycle(older.to_owned()));
            }
            cursor = next;
        }
        self.superseded_by
            .insert(older.to_owned(), newer.to_owned());
        Ok(())
    }

    pub fn get(&self, evidence_id: &str) -> Option<&EvidenceRecord> {
        self.records.get(evidence_id)
    }

    pub fn active(&self, evidence_id: &str) -> Option<&EvidenceRecord> {
        let mut current = evidence_id;
        let mut visited = BTreeSet::new();
        while let Some(next) = self.superseded_by.get(current) {
            if !visited.insert(current) {
                return None;
            }
            current = next;
        }
        self.records.get(current)
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &EvidenceRecord)> {
        self.records
            .iter()
            .map(|(id, record)| (id.as_str(), record))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppendOutcome {
    Inserted(String),
    Deduplicated(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimKind {
    Fact,
    Number,
    Interpretation,
    Uncertainty,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimStrength {
    Qualified,
    Strong,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Claim {
    pub claim_id: String,
    pub kind: ClaimKind,
    pub strength: ClaimStrength,
    pub text: String,
    #[serde(default)]
    pub goal_ids: Vec<String>,
    #[serde(default)]
    pub evidence_ids: Vec<String>,
    #[serde(default)]
    pub counter_evidence_ids: Vec<String>,
    #[serde(default)]
    pub calculation_ids: Vec<String>,
    pub subject: Option<String>,
    pub predicate: Option<String>,
    pub value: Option<Value>,
    pub unit: Option<String>,
    pub period: Option<String>,
    pub comparison_basis: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Calculation {
    pub calculation_id: String,
    pub expression: String,
    // Human-facing identity ("COIN revenue [Other] share of total CY2025").
    // The expression stays the machine kind; without the label two
    // dimensioned shares of one metric are indistinguishable to the writer.
    #[serde(default)]
    pub label: Option<String>,
    pub input_evidence_ids: Vec<String>,
    pub output: Value,
    pub unit: Option<String>,
    pub rounding: Option<String>,
    #[serde(default)]
    pub subject: Option<String>,
    #[serde(default)]
    pub metric: Option<String>,
    #[serde(default)]
    pub period: Option<String>,
    #[serde(default)]
    pub currency: Option<String>,
}

/// One bounded judgment note the analyst hands the answer writer at the
/// evidence-sufficient boundary (release B). It is advisory: it can frame
/// the composition, but every number in the final answer must still trace to
/// retained facts and calculations — a note alone upgrades nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnalystJudgmentNote {
    /// The formed position, in investor language (≤600 chars, enforced by
    /// the transition parser).
    pub position: String,
    /// Which admitted material carries the position (≤600 chars).
    pub basis: String,
    /// "high" | "medium" | "low".
    pub confidence: String,
    /// The strongest reading that disagrees, if any (≤600 chars).
    #[serde(default)]
    pub competing_reading: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnswerSection {
    pub section_id: String,
    pub heading: String,
    pub intent: String,
    pub claim_ids: Vec<String>,
    pub disclosed_uncertainty: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnswerIr {
    pub schema_version: u16,
    pub locale: String,
    pub sections: Vec<AnswerSection>,
    pub claims: Vec<Claim>,
    #[serde(default)]
    pub calculations: Vec<Calculation>,
    pub follow_up_questions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub struct AnswerPolicy {
    pub forbidden_terms: Vec<String>,
    pub require_direct_strong_claims: bool,
    pub require_period_for_numbers: bool,
    pub require_unit_for_numbers: bool,
    pub require_counter_signal_for_interpretation: bool,
    pub exact_follow_up_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationIssue {
    pub code: &'static str,
    pub claim_id: Option<String>,
    pub detail: String,
}

pub fn validate_answer(
    answer: &AnswerIr,
    ledger: &EvidenceLedger,
    policy: &AnswerPolicy,
) -> Result<(), Vec<ValidationIssue>> {
    let mut issues = Vec::new();
    if answer.schema_version != 1 {
        issues.push(ValidationIssue {
            code: "unsupported_answer_schema",
            claim_id: None,
            detail: format!(
                "expected AnswerIR schema 1, observed {}",
                answer.schema_version
            ),
        });
    }
    if answer.locale != "ko-KR" {
        issues.push(ValidationIssue {
            code: "answer_locale_mismatch",
            claim_id: None,
            detail: format!("expected ko-KR, observed {}", answer.locale),
        });
    }
    for (observed, limit, code, resource) in [
        (
            answer.sections.len(),
            MAX_ANSWER_SECTIONS,
            "too_many_sections",
            "sections",
        ),
        (
            answer.claims.len(),
            MAX_ANSWER_CLAIMS,
            "too_many_claims",
            "claims",
        ),
        (
            answer.calculations.len(),
            MAX_ANSWER_CALCULATIONS,
            "too_many_calculations",
            "calculations",
        ),
    ] {
        if observed > limit {
            issues.push(ValidationIssue {
                code,
                claim_id: None,
                detail: format!("{resource} exceeds bounded limit: {observed} > {limit}"),
            });
        }
    }
    let mut claim_ids = BTreeSet::new();
    let mut calculation_ids = BTreeMap::new();
    for calculation in &answer.calculations {
        if calculation.calculation_id.trim().is_empty() {
            issues.push(ValidationIssue {
                code: "empty_calculation_id",
                claim_id: None,
                detail: "calculation_id must not be empty".into(),
            });
        }
        if calculation_ids
            .insert(calculation.calculation_id.as_str(), calculation)
            .is_some()
        {
            issues.push(ValidationIssue {
                code: "duplicate_calculation_id",
                claim_id: None,
                detail: format!("duplicate calculation_id {}", calculation.calculation_id),
            });
        }
        match ledger.calculation(&calculation.calculation_id) {
            None => issues.push(ValidationIssue {
                code: "untrusted_calculation",
                claim_id: None,
                detail: format!(
                    "calculation {} was not produced by committed evidence",
                    calculation.calculation_id
                ),
            }),
            Some(trusted) if trusted != calculation => issues.push(ValidationIssue {
                code: "calculation_mismatch",
                claim_id: None,
                detail: format!(
                    "calculation {} differs from committed lineage",
                    calculation.calculation_id
                ),
            }),
            Some(_) => {}
        }
        for evidence_id in &calculation.input_evidence_ids {
            if ledger.active(evidence_id).is_none() {
                issues.push(ValidationIssue {
                    code: "calculation_unknown_evidence",
                    claim_id: None,
                    detail: format!(
                        "calculation {} references unknown evidence_id {evidence_id}",
                        calculation.calculation_id
                    ),
                });
            }
        }
    }
    for claim in &answer.claims {
        if claim.claim_id.trim().is_empty() {
            issues.push(issue("empty_claim_id", claim, "claim_id must not be empty"));
        }
        if !claim_ids.insert(claim.claim_id.as_str()) {
            issues.push(issue(
                "duplicate_claim_id",
                claim,
                "claim_id must be unique",
            ));
        }
        if !valid_identifier(&claim.claim_id)
            || claim.goal_ids.len() > MAX_GOALS_PER_CLAIM
            || claim.evidence_ids.len() > MAX_EVIDENCE_PER_CLAIM
            || claim.counter_evidence_ids.len() > MAX_EVIDENCE_PER_CLAIM
            || claim.calculation_ids.len() > MAX_ANSWER_CALCULATIONS
            || claim
                .goal_ids
                .iter()
                .chain(&claim.evidence_ids)
                .chain(&claim.counter_evidence_ids)
                .chain(&claim.calculation_ids)
                .any(|value| !valid_identifier(value))
        {
            issues.push(issue(
                "invalid_claim_shape",
                claim,
                "claim identifiers or bounded collections are invalid",
            ));
        }
        if !matches!(claim.kind, ClaimKind::Uncertainty) && claim.evidence_ids.is_empty() {
            issues.push(issue(
                "claim_has_no_evidence",
                claim,
                "grounded claims require evidence",
            ));
        }
        let evidence = claim
            .evidence_ids
            .iter()
            .filter_map(|id| {
                if let Some(record) = ledger.active(id) {
                    Some(record)
                } else {
                    issues.push(issue(
                        "unknown_evidence",
                        claim,
                        format!("unknown evidence_id {id}"),
                    ));
                    None
                }
            })
            .collect::<Vec<_>>();
        for record in &evidence {
            validate_public_text(
                &mut issues,
                "invalid_citation_text",
                Some(&claim.claim_id),
                &record.citation.title,
                512,
                &policy.forbidden_terms,
                false,
            );
            for value in [
                record.citation.document_type.as_deref(),
                record.citation.period.as_deref(),
            ]
            .into_iter()
            .flatten()
            {
                validate_public_text(
                    &mut issues,
                    "invalid_citation_text",
                    Some(&claim.claim_id),
                    value,
                    128,
                    &policy.forbidden_terms,
                    false,
                );
            }
        }
        if claim.strength == ClaimStrength::Strong {
            if ledger.answerability() != Answerability::StrongAllowed {
                issues.push(issue(
                    "global_strong_claim_not_allowed",
                    claim,
                    "ResearchState does not allow strong claims",
                ));
            }
            if evidence.iter().any(|record| !record.strong_claim_allowed) {
                issues.push(issue(
                    "strong_claim_not_allowed",
                    claim,
                    "one or more evidence records disallow strong claims",
                ));
            }
            if policy.require_direct_strong_claims {
                let required_directness = if claim.kind == ClaimKind::Number {
                    Directness::MetricLineage
                } else {
                    Directness::Direct
                };
                if !evidence
                    .iter()
                    .any(|record| record.directness == required_directness)
                {
                    issues.push(issue(
                        "strong_claim_without_required_directness",
                        claim,
                        format!(
                            "strong {:?} claim requires {required_directness:?}",
                            claim.kind
                        ),
                    ));
                }
            }
        }
        if claim.kind == ClaimKind::Number {
            if claim.value.is_none() {
                issues.push(issue(
                    "number_without_value",
                    claim,
                    "numeric claims require a typed value",
                ));
            }
            if claim.subject.is_none() || claim.predicate.is_none() {
                issues.push(issue(
                    "number_without_identity",
                    claim,
                    "numeric claims require subject and metric predicate",
                ));
            }
            if policy.require_unit_for_numbers && claim.unit.is_none() {
                issues.push(issue(
                    "number_without_unit",
                    claim,
                    "numeric claims require a unit",
                ));
            }
            if policy.require_period_for_numbers && claim.period.is_none() {
                issues.push(issue(
                    "number_without_period",
                    claim,
                    "numeric claims require a period",
                ));
            }
            if claim.calculation_ids.is_empty() {
                issues.push(issue(
                    "number_without_calculation",
                    claim,
                    "numeric claims require calculation lineage",
                ));
            }
            for calculation_id in &claim.calculation_ids {
                if !calculation_ids.contains_key(calculation_id.as_str()) {
                    issues.push(issue(
                        "unknown_calculation",
                        claim,
                        format!("unknown calculation_id {calculation_id}"),
                    ));
                }
            }
            let referenced_calculations = claim
                .calculation_ids
                .iter()
                .filter_map(|id| ledger.calculation(id))
                .collect::<Vec<_>>();
            if !referenced_calculations.iter().any(|calculation| {
                calculation.output == claim.value.clone().unwrap_or(Value::Null)
                    && calculation.unit == claim.unit
                    && calculation.period == claim.period
                    && calculation.subject == claim.subject
                    && calculation.metric == claim.predicate
            }) {
                issues.push(issue(
                    "number_not_equal_to_calculation",
                    claim,
                    "numeric value or its unit/period/subject/metric differs from committed calculation lineage",
                ));
            }
            for calculation in referenced_calculations {
                if calculation
                    .input_evidence_ids
                    .iter()
                    .any(|id| !claim.evidence_ids.contains(id))
                {
                    issues.push(issue(
                        "calculation_evidence_not_cited",
                        claim,
                        format!(
                            "claim omits input evidence for calculation {}",
                            calculation.calculation_id
                        ),
                    ));
                }
            }
        }
        if claim.kind == ClaimKind::Interpretation
            && policy.require_counter_signal_for_interpretation
            && claim.counter_evidence_ids.is_empty()
        {
            issues.push(issue(
                "interpretation_without_counter_signal",
                claim,
                "investment interpretations require at least one counter-signal",
            ));
        }
        for evidence_id in &claim.counter_evidence_ids {
            if ledger.active(evidence_id).is_none() {
                issues.push(issue(
                    "unknown_counter_evidence",
                    claim,
                    format!("unknown counter evidence_id {evidence_id}"),
                ));
            }
        }
        for term in &policy.forbidden_terms {
            if claim.text.to_lowercase().contains(&term.to_lowercase()) {
                issues.push(issue(
                    "internal_term_exposed",
                    claim,
                    format!("user-facing claim contains forbidden internal term {term}"),
                ));
            }
        }
        validate_public_text(
            &mut issues,
            "invalid_claim_text",
            Some(&claim.claim_id),
            &claim.text,
            2_000,
            &policy.forbidden_terms,
            false,
        );
    }

    let mut section_ids = BTreeSet::new();
    let mut rendered_claim_ids = BTreeSet::new();
    for section in &answer.sections {
        if section.section_id.trim().is_empty() || !section_ids.insert(section.section_id.as_str())
        {
            issues.push(ValidationIssue {
                code: "invalid_section_id",
                claim_id: None,
                detail: format!(
                    "section_id must be non-empty and unique: {}",
                    section.section_id
                ),
            });
        }
        if !valid_identifier(&section.section_id) {
            issues.push(ValidationIssue {
                code: "invalid_section_id",
                claim_id: None,
                detail: "section_id contains unsupported characters".into(),
            });
        }
        if section.claim_ids.len() > MAX_CLAIMS_PER_SECTION {
            issues.push(ValidationIssue {
                code: "too_many_section_claims",
                claim_id: None,
                detail: format!(
                    "section {} exceeds its claim limit: {} > {}",
                    section.section_id,
                    section.claim_ids.len(),
                    MAX_CLAIMS_PER_SECTION
                ),
            });
        }
        validate_public_text(
            &mut issues,
            "invalid_section_heading",
            None,
            &section.heading,
            80,
            &policy.forbidden_terms,
            false,
        );
        if section.disclosed_uncertainty.is_some() {
            issues.push(ValidationIssue {
                code: "free_uncertainty_text_forbidden",
                claim_id: None,
                detail: "uncertainty must be represented as a typed claim".into(),
            });
        }
        let mut local_claim_ids = BTreeSet::new();
        for claim_id in &section.claim_ids {
            if !local_claim_ids.insert(claim_id.as_str()) {
                issues.push(ValidationIssue {
                    code: "duplicate_section_claim",
                    claim_id: Some(claim_id.clone()),
                    detail: format!("section {} repeats a claim", section.section_id),
                });
            }
            rendered_claim_ids.insert(claim_id.as_str());
            if !claim_ids.contains(claim_id.as_str()) {
                issues.push(ValidationIssue {
                    code: "section_unknown_claim",
                    claim_id: Some(claim_id.clone()),
                    detail: format!("section {} references an unknown claim", section.section_id),
                });
            }
        }
    }
    for claim_id in claim_ids.difference(&rendered_claim_ids) {
        issues.push(ValidationIssue {
            code: "unrendered_claim",
            claim_id: Some((*claim_id).to_owned()),
            detail: "every claim must appear in at least one answer section".into(),
        });
    }

    if answer.follow_up_questions.len() != policy.exact_follow_up_count {
        issues.push(ValidationIssue {
            code: "follow_up_count_mismatch",
            claim_id: None,
            detail: format!(
                "expected {} follow-up questions, observed {}",
                policy.exact_follow_up_count,
                answer.follow_up_questions.len()
            ),
        });
    }
    for question in &answer.follow_up_questions {
        validate_public_text(
            &mut issues,
            "invalid_follow_up_question",
            None,
            question,
            300,
            &policy.forbidden_terms,
            true,
        );
    }

    if issues.is_empty() {
        Ok(())
    } else {
        Err(issues)
    }
}

fn validate_public_text(
    issues: &mut Vec<ValidationIssue>,
    code: &'static str,
    claim_id: Option<&str>,
    text: &str,
    max_bytes: usize,
    forbidden_terms: &[String],
    require_question: bool,
) {
    let trimmed = text.trim();
    let invalid_shape = trimmed.is_empty()
        || text.len() > max_bytes
        || text.chars().any(|character| {
            character == '\0'
                || character == '\n'
                || character == '\r'
                || (character.is_control() && character != '\t')
        })
        || (require_question && !trimmed.ends_with('?'));
    let lowercase = text.to_lowercase();
    let contains_internal = forbidden_terms
        .iter()
        .any(|term| lowercase.contains(&term.to_lowercase()));
    if invalid_shape || contains_internal {
        issues.push(ValidationIssue {
            code,
            claim_id: claim_id.map(str::to_owned),
            detail: "public text violates shape or internal-term policy".into(),
        });
    }
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
}

fn safe_bounded_optional(value: Option<&str>, max_bytes: usize) -> bool {
    value.is_none_or(|value| safe_bounded_text(value, max_bytes, true))
}

fn safe_bounded_text(value: &str, max_bytes: usize, allow_empty: bool) -> bool {
    (allow_empty || !value.trim().is_empty())
        && value.len() <= max_bytes
        && !value.chars().any(|character| {
            character == '\0' || character == '\n' || character == '\r' || character.is_control()
        })
}

fn issue(code: &'static str, claim: &Claim, detail: impl Into<String>) -> ValidationIssue {
    ValidationIssue {
        code,
        claim_id: Some(claim.claim_id.clone()),
        detail: detail.into(),
    }
}

pub fn render_markdown(
    answer: &AnswerIr,
    ledger: &EvidenceLedger,
) -> Result<String, EvidenceError> {
    let claims = answer
        .claims
        .iter()
        .map(|claim| (claim.claim_id.as_str(), claim))
        .collect::<BTreeMap<_, _>>();
    let mut output = String::new();
    let mut citation_numbers = BTreeMap::<String, usize>::new();
    let mut citation_order = Vec::<String>::new();
    for section in &answer.sections {
        if !output.is_empty() {
            output.push('\n');
        }
        output.push_str("## ");
        push_inline(&mut output, &section.heading)?;
        output.push_str("\n\n");
        for claim_id in &section.claim_ids {
            let claim = claims
                .get(claim_id.as_str())
                .ok_or_else(|| EvidenceError::UnknownClaim(claim_id.clone()))?;
            output.push_str("- ");
            push_inline(&mut output, &claim.text)?;
            for evidence_id in &claim.evidence_ids {
                let number = if let Some(number) = citation_numbers.get(evidence_id) {
                    *number
                } else {
                    if ledger.active(evidence_id).is_none() {
                        return Err(EvidenceError::UnknownEvidence(evidence_id.clone()));
                    }
                    let number = citation_order.len() + 1;
                    citation_order.push(evidence_id.clone());
                    citation_numbers.insert(evidence_id.clone(), number);
                    number
                };
                output.push_str(" [^");
                output.push_str(&number.to_string());
                output.push(']');
            }
            output.push('\n');
        }
        if section.disclosed_uncertainty.is_some() {
            return Err(EvidenceError::FreeUncertaintyText);
        }
    }
    if !answer.follow_up_questions.is_empty() {
        output.push_str("\n### 이어서 볼 질문\n\n");
        for (index, question) in answer.follow_up_questions.iter().enumerate() {
            output.push_str(&(index + 1).to_string());
            output.push_str(". ");
            push_inline(&mut output, question)?;
            output.push('\n');
        }
    }
    if !citation_order.is_empty() {
        output.push_str("\n### 출처\n\n");
        for (index, evidence_id) in citation_order.iter().enumerate() {
            let evidence = ledger
                .active(evidence_id)
                .ok_or_else(|| EvidenceError::UnknownEvidence(evidence_id.clone()))?;
            output.push_str("[^");
            output.push_str(&(index + 1).to_string());
            output.push_str("]: ");
            push_inline(&mut output, &evidence.citation.title)?;
            if let Some(document_type) = &evidence.citation.document_type {
                output.push_str(" · ");
                push_inline(&mut output, document_type)?;
            }
            if let Some(period) = &evidence.citation.period {
                output.push_str(" · ");
                push_inline(&mut output, period)?;
            }
            output.push('\n');
        }
    }
    Ok(output)
}

fn push_inline(output: &mut String, value: &str) -> Result<(), EvidenceError> {
    if value.chars().any(|character| {
        character == '\0' || character == '\n' || character == '\r' || character.is_control()
    }) {
        return Err(EvidenceError::UnsafePublicText);
    }
    for character in value.chars() {
        if matches!(
            character,
            '\\' | '`' | '*' | '_' | '[' | ']' | '<' | '>' | '#' | '|'
        ) {
            output.push('\\');
        }
        output.push(character);
    }
    Ok(())
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum EvidenceError {
    #[error("evidence_id must not be empty")]
    EmptyEvidenceId,
    #[error("duplicate evidence_id: {0}")]
    DuplicateEvidenceId(String),
    #[error("invalid evidence metadata: {0}")]
    InvalidEvidenceMetadata(String),
    #[error("unknown evidence_id: {0}")]
    UnknownEvidence(String),
    #[error("supersession cycle involving evidence_id: {0}")]
    SupersessionCycle(String),
    #[error("unknown claim_id: {0}")]
    UnknownClaim(String),
    #[error("calculation_id must not be empty")]
    EmptyCalculationId,
    #[error("calculation has no input evidence: {0}")]
    CalculationWithoutEvidence(String),
    #[error("calculation conflicts with committed lineage: {0}")]
    CalculationConflict(String),
    #[error("invalid calculation metadata: {0}")]
    InvalidCalculationMetadata(String),
    #[error("unsafe control or multiline text reached the public renderer")]
    UnsafePublicText,
    #[error("free uncertainty text reached the renderer instead of a typed claim")]
    FreeUncertaintyText,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash(label: &str) -> ContentHash {
        ContentHash::sha256(label)
    }

    fn evidence(id: &str, scope: &str) -> EvidenceRecord {
        EvidenceRecord {
            evidence_id: id.into(),
            content_hash: hash("payload"),
            source: EvidenceSource {
                capability_id: "ontology.query_context".into(),
                action_key: "action-1".into(),
                server_build: "fixture".into(),
                normalized_contract_hash: hash("normalized-contract"),
                server_schema_bundle_hash: hash("server-schema-bundle"),
                data_release_hash: hash("release"),
            },
            scope: EvidenceScope {
                auth_scope: AuthScope::Tenant,
                scope_hash: hash(scope),
            },
            entity: Some("AAPL".into()),
            period: Some("FY2025".into()),
            as_of: Some("2026-08-02".into()),
            directness: Directness::Direct,
            grade: EvidenceGrade::Strong,
            strong_claim_allowed: true,
            payload_ref: hash("payload"),
            citation: PublicCitation {
                title: "AAPL 2025 Form 10-K".into(),
                document_type: Some("10-K".into()),
                period: Some("FY2025".into()),
            },
            facts: vec![],
            supports: vec![],
            refutes: vec![],
            qualifies: vec![],
            source_object_ids: vec![],
        }
    }

    #[test]
    fn dedup_never_crosses_principal_scope() {
        let mut ledger = EvidenceLedger::default();
        assert!(matches!(
            ledger.append(evidence("e1", "tenant-a")).unwrap(),
            AppendOutcome::Inserted(_)
        ));
        assert!(matches!(
            ledger.append(evidence("e2", "tenant-a")).unwrap(),
            AppendOutcome::Deduplicated(id) if id == "e1"
        ));
        assert!(matches!(
            ledger.append(evidence("e3", "tenant-b")).unwrap(),
            AppendOutcome::Inserted(_)
        ));
        assert_eq!(ledger.len(), 2);
    }

    #[test]
    fn durable_round_trip_rebuilds_dedup_identity_index() {
        let ledger = EvidenceLedger::from_records([evidence("e1", "tenant-a")]).unwrap();
        let encoded = serde_json::to_vec(&ledger).unwrap();
        let mut restored: EvidenceLedger = serde_json::from_slice(&encoded).unwrap();

        assert!(matches!(
            restored.append(evidence("e2", "tenant-a")).unwrap(),
            AppendOutcome::Deduplicated(id) if id == "e1"
        ));
        assert_eq!(restored.len(), 1);
    }

    #[test]
    fn durable_restore_rejects_duplicate_evidence_identity() {
        let ledger = EvidenceLedger::from_records([evidence("e1", "tenant-a")]).unwrap();
        let mut encoded = serde_json::to_value(&ledger).unwrap();
        let duplicate = encoded["records"]["e1"].clone();
        encoded["records"]["e2"] = duplicate;
        encoded["records"]["e2"]["evidence_id"] = serde_json::json!("e2");

        let error = serde_json::from_value::<EvidenceLedger>(encoded).unwrap_err();
        assert!(error.to_string().contains("duplicate evidence identity"));
    }

    #[test]
    fn substantive_supplemental_evidence_reopens_not_answerable_to_qualified_only() {
        let mut ledger = EvidenceLedger::default();
        ledger.set_answerability(Answerability::NotAnswerable);
        let mut targeted = evidence("e-targeted", "tenant-a");
        targeted.strong_claim_allowed = false;
        targeted.facts = vec![NormalizedFact {
            subject: "AAPL".into(),
            predicate: "services_revenue".into(),
            value: serde_json::json!(30976),
            unit: Some("USD millions".into()),
            period: Some("2026-03-28 종료 분기".into()),
        }];

        ledger.reopen_qualified_after_substantive_supplement([&targeted]);

        assert_eq!(ledger.answerability(), Answerability::QualifiedOnly);
    }

    #[test]
    fn related_or_empty_supplemental_evidence_does_not_reopen_not_answerable() {
        let mut ledger = EvidenceLedger::default();
        ledger.set_answerability(Answerability::NotAnswerable);
        let mut related = evidence("e-related", "tenant-a");
        related.directness = Directness::Related;
        related.facts = vec![NormalizedFact {
            subject: "AAPL".into(),
            predicate: "ontology_reference".into(),
            value: serde_json::json!("related object"),
            unit: None,
            period: None,
        }];

        ledger.reopen_qualified_after_substantive_supplement([&related]);

        assert_eq!(ledger.answerability(), Answerability::NotAnswerable);
    }

    #[test]
    fn oversized_evidence_record_fails_before_entering_ledger() {
        let mut record = evidence("e1", "tenant-a");
        record.facts = (0..=MAX_FACTS_PER_RECORD)
            .map(|index| NormalizedFact {
                subject: "AAPL".into(),
                predicate: format!("metric_{index}"),
                value: serde_json::json!(index),
                unit: None,
                period: Some("FY2025".into()),
            })
            .collect();
        assert!(matches!(
            EvidenceLedger::from_records([record]),
            Err(EvidenceError::InvalidEvidenceMetadata(id)) if id == "e1"
        ));
    }

    #[test]
    fn oversized_answer_collections_fail_deterministically() {
        let ledger = EvidenceLedger::default();
        let answer = AnswerIr {
            schema_version: 1,
            locale: "ko-KR".into(),
            sections: Vec::new(),
            claims: (0..=MAX_ANSWER_CLAIMS)
                .map(|index| Claim {
                    claim_id: format!("claim-{index}"),
                    kind: ClaimKind::Uncertainty,
                    strength: ClaimStrength::Qualified,
                    text: "확인할 근거가 부족합니다.".into(),
                    goal_ids: Vec::new(),
                    evidence_ids: Vec::new(),
                    counter_evidence_ids: Vec::new(),
                    calculation_ids: Vec::new(),
                    subject: None,
                    predicate: None,
                    value: None,
                    unit: None,
                    period: None,
                    comparison_basis: None,
                })
                .collect(),
            calculations: Vec::new(),
            follow_up_questions: vec!["질문 1?".into(), "질문 2?".into(), "질문 3?".into()],
        };
        let issues = validate_answer(
            &answer,
            &ledger,
            &AnswerPolicy {
                forbidden_terms: Vec::new(),
                require_direct_strong_claims: true,
                require_period_for_numbers: true,
                require_unit_for_numbers: true,
                require_counter_signal_for_interpretation: true,
                exact_follow_up_count: 3,
            },
        )
        .unwrap_err();
        assert!(issues.iter().any(|issue| issue.code == "too_many_claims"));
    }

    #[test]
    fn strong_claim_requires_direct_allowed_evidence() {
        let mut ledger = EvidenceLedger::from_records([evidence("e1", "tenant-a")]).unwrap();
        ledger.set_answerability(Answerability::StrongAllowed);
        let answer = AnswerIr {
            schema_version: 1,
            locale: "ko-KR".into(),
            sections: vec![AnswerSection {
                section_id: "summary".into(),
                heading: "핵심".into(),
                intent: "answer".into(),
                claim_ids: vec!["c1".into()],
                disclosed_uncertainty: None,
            }],
            claims: vec![Claim {
                claim_id: "c1".into(),
                kind: ClaimKind::Fact,
                strength: ClaimStrength::Strong,
                text: "회사는 현금을 창출했습니다.".into(),
                goal_ids: vec!["g1".into()],
                evidence_ids: vec!["e1".into()],
                counter_evidence_ids: vec![],
                calculation_ids: vec![],
                subject: Some("AAPL".into()),
                predicate: Some("generated_cash".into()),
                value: None,
                unit: None,
                period: Some("FY2025".into()),
                comparison_basis: None,
            }],
            calculations: vec![],
            follow_up_questions: vec!["질문 1?".into(), "질문 2?".into(), "질문 3?".into()],
        };
        let policy = AnswerPolicy {
            forbidden_terms: vec!["research_state".into()],
            require_direct_strong_claims: true,
            require_period_for_numbers: true,
            require_unit_for_numbers: true,
            require_counter_signal_for_interpretation: true,
            exact_follow_up_count: 3,
        };
        assert!(validate_answer(&answer, &ledger, &policy).is_ok());
        let rendered = render_markdown(&answer, &ledger).unwrap();
        assert!(rendered.contains("[^1]"));
        assert!(!rendered.contains("[^e1]"));
        assert!(rendered.contains("AAPL 2025 Form 10-K"));
    }

    #[test]
    fn partial_global_answerability_blocks_strong_claim() {
        let ledger = EvidenceLedger::from_records([evidence("e1", "tenant-a")]).unwrap();
        let answer = AnswerIr {
            schema_version: 1,
            locale: "ko-KR".into(),
            sections: vec![],
            claims: vec![Claim {
                claim_id: "c1".into(),
                kind: ClaimKind::Fact,
                strength: ClaimStrength::Strong,
                text: "강한 결론".into(),
                goal_ids: vec![],
                evidence_ids: vec!["e1".into()],
                counter_evidence_ids: vec![],
                calculation_ids: vec![],
                subject: None,
                predicate: None,
                value: None,
                unit: None,
                period: None,
                comparison_basis: None,
            }],
            calculations: vec![],
            follow_up_questions: vec!["질문 1?".into(), "질문 2?".into(), "질문 3?".into()],
        };
        let policy = AnswerPolicy {
            forbidden_terms: vec![],
            require_direct_strong_claims: true,
            require_period_for_numbers: true,
            require_unit_for_numbers: true,
            require_counter_signal_for_interpretation: true,
            exact_follow_up_count: 3,
        };
        let issues = validate_answer(&answer, &ledger, &policy).unwrap_err();
        assert!(
            issues
                .iter()
                .any(|issue| issue.code == "global_strong_claim_not_allowed")
        );
    }

    #[test]
    fn numeric_claim_cannot_change_a_committed_calculation() {
        let mut ledger = EvidenceLedger::from_records([evidence("e1", "tenant-a")]).unwrap();
        let calculation = Calculation {
            calculation_id: "calc-1".into(),
            expression: "reported_value".into(),
            label: None,
            input_evidence_ids: vec!["e1".into()],
            output: serde_json::json!(10),
            unit: Some("USD".into()),
            rounding: None,
            subject: Some("AAPL".into()),
            metric: Some("operating_cash_flow".into()),
            period: Some("FY2025".into()),
            currency: Some("USD".into()),
        };
        ledger.append_calculation(calculation.clone()).unwrap();
        let answer = AnswerIr {
            schema_version: 1,
            locale: "ko-KR".into(),
            sections: vec![AnswerSection {
                section_id: "summary".into(),
                heading: "핵심".into(),
                intent: "answer".into(),
                claim_ids: vec!["c1".into()],
                disclosed_uncertainty: None,
            }],
            claims: vec![Claim {
                claim_id: "c1".into(),
                kind: ClaimKind::Number,
                strength: ClaimStrength::Qualified,
                text: "영업현금흐름은 11달러입니다.".into(),
                goal_ids: vec![],
                evidence_ids: vec!["e1".into()],
                counter_evidence_ids: vec![],
                calculation_ids: vec!["calc-1".into()],
                subject: Some("AAPL".into()),
                predicate: Some("operating_cash_flow".into()),
                value: Some(serde_json::json!(11)),
                unit: Some("USD".into()),
                period: Some("FY2025".into()),
                comparison_basis: None,
            }],
            calculations: vec![calculation],
            follow_up_questions: vec!["질문 1?".into(), "질문 2?".into(), "질문 3?".into()],
        };
        let issues = validate_answer(
            &answer,
            &ledger,
            &AnswerPolicy {
                forbidden_terms: vec![],
                require_direct_strong_claims: true,
                require_period_for_numbers: true,
                require_unit_for_numbers: true,
                require_counter_signal_for_interpretation: true,
                exact_follow_up_count: 3,
            },
        )
        .unwrap_err();
        assert!(
            issues
                .iter()
                .any(|issue| issue.code == "number_not_equal_to_calculation")
        );
    }
}
