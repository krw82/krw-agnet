//! Claim-time reconstruction of context-only session memory.
//!
//! `PostgreSQL` owns the append-only delta log. This module treats every page as
//! untrusted transport data: it revalidates ownership metadata, canonical
//! delta hashes, the frontier chain, and contiguous ordering before producing
//! the only bounded carrier that may enter a provider prompt.

use std::fmt;

use krw_agent_persistence::agent_v1::ReadSessionMemoryResponse;
use krw_agent_protocol::{ContentHash, SessionMemoryCarrierV3};
use krw_session_memory::{
    MAX_SESSION_MEMORY_SNAPSHOT_BYTES, MAX_SESSION_MEMORY_VIEW_BYTES, SessionMemoryCatalogV3,
    SessionMemoryDeltaV3, SessionMemorySnapshotV3, SessionViewQuery, empty_frontier_hash,
    empty_source_lineage_hash,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

const MAX_MEMORY_PAGE_DELTAS: usize = 8;
const SNAPSHOT_CHECKPOINT_TAIL_THRESHOLD: u64 = 16;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryResolutionReceipt {
    pub schema_version: u16,
    pub run_id_hash: ContentHash,
    pub session_id_hash: ContentHash,
    pub source_revision: u64,
    pub source_frontier_hash: ContentHash,
    pub source_lineage_hash: ContentHash,
    pub view_hash: Option<ContentHash>,
    pub base_snapshot_revision: u64,
    pub checkpoint_required: bool,
    pub page_count: u64,
    pub delta_count: u64,
}

impl MemoryResolutionReceipt {
    pub fn content_hash(&self) -> Result<ContentHash, MemoryResolutionError> {
        Ok(ContentHash::sha256(serde_jcs::to_vec(self)?))
    }
}

#[derive(Clone, PartialEq)]
pub struct ResolvedSessionMemory {
    pub carrier: Option<SessionMemoryCarrierV3>,
    pub receipt: MemoryResolutionReceipt,
    pub checkpoint_snapshot: Option<SessionMemorySnapshotV3>,
}

impl fmt::Debug for ResolvedSessionMemory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResolvedSessionMemory")
            .field("carrier", &self.carrier)
            .field("receipt", &self.receipt)
            .field(
                "checkpoint_snapshot_revision",
                &self
                    .checkpoint_snapshot
                    .as_ref()
                    .map(|snapshot| snapshot.revision),
            )
            .finish()
    }
}

pub struct SessionMemoryPageAccumulator {
    run_id: String,
    session_id: String,
    fencing_token: u64,
    session_id_hash: ContentHash,
    catalog: SessionMemoryCatalogV3,
    declared_revision: Option<u64>,
    declared_frontier: Option<ContentHash>,
    declared_source_lineage: Option<ContentHash>,
    next_after_revision: u64,
    page_count: u64,
    delta_count: u64,
    deltas_since_rebase: u64,
    base_snapshot_revision: u64,
    complete: bool,
}

impl fmt::Debug for SessionMemoryPageAccumulator {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SessionMemoryPageAccumulator")
            .field("run_id_hash", &ContentHash::sha256(&self.run_id))
            .field("fencing_token", &self.fencing_token)
            .field("session_id_hash", &self.session_id_hash)
            .field(
                "session_id_hash_from_text",
                &ContentHash::sha256(&self.session_id),
            )
            .field("catalog", &self.catalog)
            .field("declared_revision", &self.declared_revision)
            .field("declared_frontier", &self.declared_frontier)
            .field("declared_source_lineage", &self.declared_source_lineage)
            .field("next_after_revision", &self.next_after_revision)
            .field("page_count", &self.page_count)
            .field("delta_count", &self.delta_count)
            .field("deltas_since_rebase", &self.deltas_since_rebase)
            .field("base_snapshot_revision", &self.base_snapshot_revision)
            .field("complete", &self.complete)
            .finish()
    }
}

impl SessionMemoryPageAccumulator {
    pub fn new(
        run_id: impl Into<String>,
        session_id: &str,
        fencing_token: u64,
    ) -> Result<Self, MemoryResolutionError> {
        let run_id = run_id.into();
        if run_id.is_empty() || run_id.len() > 256 || run_id.contains('\0') || fencing_token == 0 {
            return Err(MemoryResolutionError::InvalidScope);
        }
        Ok(Self {
            run_id,
            session_id: session_id.to_owned(),
            fencing_token,
            session_id_hash: ContentHash::sha256(session_id),
            catalog: SessionMemoryCatalogV3::new(session_id)
                .map_err(|_| MemoryResolutionError::InvalidScope)?,
            declared_revision: None,
            declared_frontier: None,
            declared_source_lineage: None,
            next_after_revision: 0,
            page_count: 0,
            delta_count: 0,
            deltas_since_rebase: 0,
            base_snapshot_revision: 0,
            complete: false,
        })
    }

    pub const fn next_after_revision(&self) -> u64 {
        self.next_after_revision
    }

    pub fn push_page(
        &mut self,
        page: &ReadSessionMemoryResponse,
    ) -> Result<(), MemoryResolutionError> {
        if self.complete
            || page.run_id != self.run_id
            || page.fencing_token != self.fencing_token
            || page.deltas.len() > MAX_MEMORY_PAGE_DELTAS
        {
            return Err(MemoryResolutionError::InvalidPage);
        }
        match (
            &self.declared_revision,
            &self.declared_frontier,
            &self.declared_source_lineage,
        ) {
            (None, None, None) => {
                self.declared_revision = Some(page.memory_revision);
                self.declared_frontier = Some(page.memory_frontier_hash.clone());
                self.declared_source_lineage = Some(page.memory_source_lineage_hash.clone());
            }
            (Some(revision), Some(frontier), Some(source_lineage))
                if *revision == page.memory_revision
                    && *frontier == page.memory_frontier_hash
                    && *source_lineage == page.memory_source_lineage_hash => {}
            _ => return Err(MemoryResolutionError::SnapshotDrift),
        }

        if self.page_count == 0 {
            if let Some(receipt) = &page.snapshot {
                let snapshot: SessionMemorySnapshotV3 =
                    serde_json::from_value(receipt.snapshot.clone())?;
                let canonical = snapshot
                    .canonical_bytes()
                    .map_err(|_| MemoryResolutionError::InvalidSnapshot)?;
                if canonical.len() > MAX_SESSION_MEMORY_SNAPSHOT_BYTES
                    || u64::try_from(canonical.len()).ok() != Some(receipt.snapshot_size_bytes)
                    || ContentHash::sha256(&canonical) != receipt.snapshot_hash
                    || snapshot.revision != receipt.revision
                    || snapshot.frontier_hash != receipt.frontier_hash
                    || snapshot.source_lineage_hash != receipt.source_lineage_hash
                    || snapshot.session_id_hash != self.session_id_hash
                    || snapshot.revision > page.memory_revision
                    || page.memory_revision - snapshot.revision > 32
                {
                    return Err(MemoryResolutionError::InvalidSnapshot);
                }
                self.catalog = SessionMemoryCatalogV3::from_snapshot(&self.session_id, &snapshot)
                    .map_err(|_| MemoryResolutionError::InvalidSnapshot)?;
                self.next_after_revision = snapshot.revision;
                self.base_snapshot_revision = snapshot.revision;
            }
        } else if page.snapshot.is_some() {
            return Err(MemoryResolutionError::InvalidSnapshot);
        }

        let mut expected_revision = self
            .next_after_revision
            .checked_add(1)
            .ok_or(MemoryResolutionError::Overflow)?;
        for receipt in &page.deltas {
            let delta: SessionMemoryDeltaV3 = serde_json::from_value(receipt.delta.clone())?;
            let delta_hash = delta
                .content_hash()
                .map_err(|_| MemoryResolutionError::InvalidDelta)?;
            let next_frontier = delta
                .next_frontier_hash()
                .map_err(|_| MemoryResolutionError::InvalidDelta)?;
            if receipt.revision != expected_revision
                || delta.revision != receipt.revision
                || receipt.parent_frontier_hash != delta.parent_frontier_hash
                || receipt.delta_hash != delta_hash
                || receipt.next_frontier_hash != next_frontier
                || receipt.source_run_id != delta.source.run_id
                || delta.session_id_hash != self.session_id_hash
            {
                return Err(MemoryResolutionError::InvalidDelta);
            }
            self.catalog
                .apply_delta(&delta)
                .map_err(|_| MemoryResolutionError::InvalidDelta)?;
            if self.catalog.source_lineage_hash() != &receipt.source_lineage_hash {
                return Err(MemoryResolutionError::InvalidDelta);
            }
            self.deltas_since_rebase = self
                .deltas_since_rebase
                .checked_add(1)
                .ok_or(MemoryResolutionError::Overflow)?;
            if self.deltas_since_rebase >= SNAPSHOT_CHECKPOINT_TAIL_THRESHOLD {
                let snapshot = self
                    .catalog
                    .snapshot()
                    .map_err(|_| MemoryResolutionError::InvalidSnapshot)?;
                self.catalog = SessionMemoryCatalogV3::from_snapshot(&self.session_id, &snapshot)
                    .map_err(|_| MemoryResolutionError::InvalidSnapshot)?;
                self.deltas_since_rebase = 0;
            }
            expected_revision = expected_revision
                .checked_add(1)
                .ok_or(MemoryResolutionError::Overflow)?;
        }
        let consumed =
            u64::try_from(page.deltas.len()).map_err(|_| MemoryResolutionError::Overflow)?;
        self.delta_count = self
            .delta_count
            .checked_add(consumed)
            .ok_or(MemoryResolutionError::Overflow)?;
        self.page_count = self
            .page_count
            .checked_add(1)
            .ok_or(MemoryResolutionError::Overflow)?;
        if page.next_after_revision != self.catalog.revision()
            || page.next_after_revision < self.next_after_revision
            || (page.has_more
                && (page.deltas.is_empty() || page.next_after_revision >= page.memory_revision))
            || (!page.has_more && page.next_after_revision != page.memory_revision)
        {
            return Err(MemoryResolutionError::InvalidPage);
        }
        self.next_after_revision = page.next_after_revision;
        self.complete = !page.has_more;
        Ok(())
    }

    pub fn finish(self, question: &str) -> Result<ResolvedSessionMemory, MemoryResolutionError> {
        let query = SessionViewQuery {
            question,
            trusted_tickers: &[],
        };
        self.finish_with_query(&query)
    }

    pub fn finish_with_query(
        self,
        query: &SessionViewQuery<'_>,
    ) -> Result<ResolvedSessionMemory, MemoryResolutionError> {
        if !self.complete {
            return Err(MemoryResolutionError::Incomplete);
        }
        let revision = self
            .declared_revision
            .ok_or(MemoryResolutionError::Incomplete)?;
        let frontier = self
            .declared_frontier
            .clone()
            .ok_or(MemoryResolutionError::Incomplete)?;
        let source_lineage = self
            .declared_source_lineage
            .clone()
            .ok_or(MemoryResolutionError::Incomplete)?;
        if self.catalog.revision() != revision
            || self.catalog.frontier_hash() != &frontier
            || self.catalog.source_lineage_hash() != &source_lineage
        {
            return Err(MemoryResolutionError::SnapshotDrift);
        }
        let checkpoint_required = revision > 0
            && (self.base_snapshot_revision == 0
                || revision.saturating_sub(self.base_snapshot_revision)
                    >= SNAPSHOT_CHECKPOINT_TAIL_THRESHOLD);
        let checkpoint_snapshot = checkpoint_required
            .then(|| self.catalog.snapshot())
            .transpose()
            .map_err(|_| MemoryResolutionError::InvalidSnapshot)?;
        let provider_catalog = checkpoint_snapshot
            .as_ref()
            .map(|snapshot| SessionMemoryCatalogV3::from_snapshot(&self.session_id, snapshot))
            .transpose()
            .map_err(|_| MemoryResolutionError::InvalidSnapshot)?;
        let provider_catalog = provider_catalog.as_ref().unwrap_or(&self.catalog);
        let carrier = if revision == 0 {
            if frontier != empty_frontier_hash() || source_lineage != empty_source_lineage_hash() {
                return Err(MemoryResolutionError::SnapshotDrift);
            }
            None
        } else {
            let view = provider_catalog
                .select_view_with_query(query, MAX_SESSION_MEMORY_VIEW_BYTES)
                .map_err(|_| MemoryResolutionError::InvalidView)?;
            let canonical_view = serde_json::to_value(&view)?;
            let view_hash = ContentHash::sha256(serde_jcs::to_vec(&canonical_view)?);
            let carrier = SessionMemoryCarrierV3 {
                schema_version: 3,
                view_hash,
                source_frontier_hash: frontier.clone(),
                source_revision: revision,
                canonical_view,
            };
            carrier
                .validate_carrier()
                .map_err(|_| MemoryResolutionError::InvalidView)?;
            Some(carrier)
        };
        let receipt = MemoryResolutionReceipt {
            schema_version: 3,
            run_id_hash: ContentHash::sha256(&self.run_id),
            session_id_hash: self.session_id_hash,
            source_revision: revision,
            source_frontier_hash: frontier,
            source_lineage_hash: source_lineage,
            view_hash: carrier.as_ref().map(|carrier| carrier.view_hash.clone()),
            base_snapshot_revision: self.base_snapshot_revision,
            checkpoint_required,
            page_count: self.page_count,
            delta_count: self.delta_count,
        };
        receipt.content_hash()?;
        Ok(ResolvedSessionMemory {
            carrier,
            receipt,
            checkpoint_snapshot,
        })
    }
}

#[derive(Debug, Error)]
pub enum MemoryResolutionError {
    #[error("session memory scope is invalid")]
    InvalidScope,
    #[error("session memory page envelope is invalid")]
    InvalidPage,
    #[error("session memory snapshot changed during a fenced read")]
    SnapshotDrift,
    #[error("session memory delta chain is invalid")]
    InvalidDelta,
    #[error("session memory snapshot is invalid")]
    InvalidSnapshot,
    #[error("session memory read did not reach a terminal page")]
    Incomplete,
    #[error("session memory view is invalid")]
    InvalidView,
    #[error("session memory counter overflow")]
    Overflow,
    #[error("session memory JSON is invalid")]
    Json(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use krw_agent_evidence::{AnswerIr, AnswerSection, Claim, ClaimKind, ClaimStrength};
    use krw_agent_persistence::agent_v1::{
        SessionMemoryDeltaReceipt, SessionMemorySnapshotReceipt,
    };
    use krw_session_memory::{
        CompletedTurnInputV3, SessionMemoryCatalogV3, completed_turn_delta,
        next_source_lineage_hash,
    };

    use super::*;

    const RUN_ID: &str = "run:memory-reader:1";
    const SESSION_ID: &str = "session:memory:1";

    fn answer_ir() -> AnswerIr {
        AnswerIr {
            schema_version: 1,
            locale: "ko-KR".into(),
            sections: vec![AnswerSection {
                section_id: "summary".into(),
                heading: "요약".into(),
                intent: "answer".into(),
                claim_ids: vec!["revenue".into()],
                disclosed_uncertainty: None,
            }],
            claims: vec![Claim {
                claim_id: "revenue".into(),
                kind: ClaimKind::Number,
                strength: ClaimStrength::Qualified,
                text: "FY2026 매출은 11% 증가했습니다.".into(),
                goal_ids: Vec::new(),
                evidence_ids: vec!["ev-revenue".into()],
                counter_evidence_ids: Vec::new(),
                calculation_ids: vec!["calc-growth".into()],
                subject: Some("AAPL".into()),
                predicate: Some("revenue_growth".into()),
                value: Some(serde_json::json!(11)),
                unit: Some("percent".into()),
                period: Some("FY2026".into()),
                comparison_basis: Some("FY2025".into()),
            }],
            calculations: Vec::new(),
            follow_up_questions: Vec::new(),
        }
    }

    fn one_delta_page() -> ReadSessionMemoryResponse {
        let delta = completed_turn_delta(CompletedTurnInputV3 {
            session_id: SESSION_ID,
            parent_frontier_hash: empty_frontier_hash(),
            revision: 1,
            run_id: "run:source:1",
            final_commit_intent_hash: ContentHash::sha256("intent"),
            answer_bundle_hash: ContentHash::sha256("bundle"),
            user_content: "AAPL 매출을 알려줘.",
            rendered_answer: "FY2026 매출은 11% 증가했습니다.",
            answer_ir: &answer_ir(),
            tickers: &["AAPL".into()],
            constraints: &[],
            supersessions: &[],
            resolved_goals: &[],
        })
        .unwrap();
        let delta_hash = delta.content_hash().unwrap();
        let next_frontier_hash = delta.next_frontier_hash().unwrap();
        let source_lineage_hash = krw_session_memory::next_source_lineage_hash(
            &empty_source_lineage_hash(),
            1,
            &ContentHash::sha256("run:source:1"),
        );
        ReadSessionMemoryResponse {
            run_id: RUN_ID.into(),
            fencing_token: 9,
            run_version: 3,
            memory_revision: 1,
            memory_frontier_hash: next_frontier_hash.clone(),
            memory_source_lineage_hash: source_lineage_hash.clone(),
            snapshot: None,
            deltas: vec![SessionMemoryDeltaReceipt {
                revision: 1,
                parent_frontier_hash: empty_frontier_hash(),
                delta_hash,
                next_frontier_hash,
                source_lineage_hash,
                source_run_id: "run:source:1".into(),
                delta: serde_json::to_value(&delta).unwrap(),
            }],
            next_after_revision: 1,
            has_more: false,
        }
    }

    #[test]
    fn reconstructs_hash_bound_question_conditioned_carrier() {
        let page = one_delta_page();
        let source_lineage_hash = page.memory_source_lineage_hash.clone();
        let mut accumulator = SessionMemoryPageAccumulator::new(RUN_ID, SESSION_ID, 9).unwrap();
        accumulator.push_page(&page).unwrap();
        let resolved = accumulator.finish("매출 성장률을 다시 확인해줘").unwrap();
        let carrier = resolved.carrier.unwrap();
        assert_eq!(carrier.schema_version, 3);
        assert_eq!(carrier.source_revision, 1);
        carrier.validate_carrier().unwrap();
        assert_eq!(resolved.receipt.delta_count, 1);
        assert_eq!(resolved.receipt.source_lineage_hash, source_lineage_hash);
        resolved.receipt.content_hash().unwrap();
    }

    #[test]
    fn empty_memory_is_none_and_tampered_delta_fails_closed() {
        let mut empty = SessionMemoryPageAccumulator::new(RUN_ID, SESSION_ID, 9).unwrap();
        empty
            .push_page(&ReadSessionMemoryResponse {
                run_id: RUN_ID.into(),
                fencing_token: 9,
                run_version: 3,
                memory_revision: 0,
                memory_frontier_hash: empty_frontier_hash(),
                memory_source_lineage_hash: empty_source_lineage_hash(),
                snapshot: None,
                deltas: Vec::new(),
                next_after_revision: 0,
                has_more: false,
            })
            .unwrap();
        assert!(empty.finish("새 질문").unwrap().carrier.is_none());

        let mut page = one_delta_page();
        page.deltas[0].delta_hash = ContentHash::sha256("tampered");
        let mut tampered = SessionMemoryPageAccumulator::new(RUN_ID, SESSION_ID, 9).unwrap();
        assert!(matches!(
            tampered.push_page(&page),
            Err(MemoryResolutionError::InvalidDelta)
        ));
    }

    #[test]
    fn snapshot_plus_tail_restores_and_lineage_tamper_fails_closed() {
        let first_page = one_delta_page();
        let first_delta: SessionMemoryDeltaV3 =
            serde_json::from_value(first_page.deltas[0].delta.clone()).unwrap();
        let mut catalog = SessionMemoryCatalogV3::new(SESSION_ID).unwrap();
        catalog.apply_delta(&first_delta).unwrap();
        let snapshot = catalog.snapshot().unwrap();
        let snapshot_bytes = snapshot.canonical_bytes().unwrap();
        let snapshot_receipt = SessionMemorySnapshotReceipt {
            revision: snapshot.revision,
            frontier_hash: snapshot.frontier_hash.clone(),
            source_lineage_hash: snapshot.source_lineage_hash.clone(),
            snapshot_hash: ContentHash::sha256(&snapshot_bytes),
            snapshot_size_bytes: snapshot_bytes.len() as u64,
            snapshot: serde_json::to_value(&snapshot).unwrap(),
        };
        let second_delta = completed_turn_delta(CompletedTurnInputV3 {
            session_id: SESSION_ID,
            parent_frontier_hash: snapshot.frontier_hash.clone(),
            revision: 2,
            run_id: "run:source:2",
            final_commit_intent_hash: ContentHash::sha256("intent-2"),
            answer_bundle_hash: ContentHash::sha256("bundle-2"),
            user_content: "AAPL 현금흐름도 알려줘.",
            rendered_answer: "현금흐름은 추가 확인이 필요합니다.",
            answer_ir: &answer_ir(),
            tickers: &["AAPL".into()],
            constraints: &[],
            supersessions: &[],
            resolved_goals: &[],
        })
        .unwrap();
        let delta_hash = second_delta.content_hash().unwrap();
        let next_frontier_hash = second_delta.next_frontier_hash().unwrap();
        let source_lineage_hash = next_source_lineage_hash(
            &snapshot.source_lineage_hash,
            2,
            &ContentHash::sha256("run:source:2"),
        );
        let page = ReadSessionMemoryResponse {
            run_id: RUN_ID.into(),
            fencing_token: 9,
            run_version: 3,
            memory_revision: 2,
            memory_frontier_hash: next_frontier_hash.clone(),
            memory_source_lineage_hash: source_lineage_hash.clone(),
            snapshot: Some(snapshot_receipt),
            deltas: vec![SessionMemoryDeltaReceipt {
                revision: 2,
                parent_frontier_hash: snapshot.frontier_hash.clone(),
                delta_hash,
                next_frontier_hash,
                source_lineage_hash: source_lineage_hash.clone(),
                source_run_id: "run:source:2".into(),
                delta: serde_json::to_value(&second_delta).unwrap(),
            }],
            next_after_revision: 2,
            has_more: false,
        };

        let mut accumulator = SessionMemoryPageAccumulator::new(RUN_ID, SESSION_ID, 9).unwrap();
        accumulator.push_page(&page).unwrap();
        let resolved = accumulator.finish("AAPL 매출과 현금흐름").unwrap();
        assert_eq!(resolved.receipt.base_snapshot_revision, 1);
        assert_eq!(resolved.receipt.source_lineage_hash, source_lineage_hash);
        assert!(!resolved.receipt.checkpoint_required);

        let mut tampered_tail = page.clone();
        tampered_tail.deltas[0].source_lineage_hash = ContentHash::sha256("tampered-lineage");
        let mut accumulator = SessionMemoryPageAccumulator::new(RUN_ID, SESSION_ID, 9).unwrap();
        assert!(matches!(
            accumulator.push_page(&tampered_tail),
            Err(MemoryResolutionError::InvalidDelta)
        ));

        let mut tampered_snapshot = page;
        tampered_snapshot
            .snapshot
            .as_mut()
            .unwrap()
            .source_lineage_hash = ContentHash::sha256("tampered-snapshot-lineage");
        let mut accumulator = SessionMemoryPageAccumulator::new(RUN_ID, SESSION_ID, 9).unwrap();
        assert!(matches!(
            accumulator.push_page(&tampered_snapshot),
            Err(MemoryResolutionError::InvalidSnapshot)
        ));
    }
}
