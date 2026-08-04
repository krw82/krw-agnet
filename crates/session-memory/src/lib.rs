//! Typed, evidence-linked cross-run session memory.
//!
//! The provider never writes this store directly. A completed typed output
//! produces an append-only delta; the product database applies that delta
//! under the same session/tenant fence as the final answer. A later run
//! receives only a small question-conditioned view. Memory is context only:
//! its evidence identifiers are lineage references and never grant evidence
//! authority to the current run. Direct Markdown turns retain continuity only
//! and never manufacture claims from prose.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use krw_agent_evidence::{AnswerIr, Claim, ClaimKind};
use krw_agent_protocol::{ContentHash, is_canonical_ticker};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use zeroize::Zeroize;

pub const SESSION_MEMORY_SCHEMA_VERSION: u16 = 3;

const MAX_SOURCE_ID_BYTES: usize = 128;
const MAX_TEXT_BYTES: usize = 64 * 1024;
const MAX_RECENT_TURN_EXCERPT_BYTES: usize = 32 * 1024;
const MAX_COMPLETED_TURN_PROJECTION_BYTES: usize = 768 * 1024;
const MAX_CONSTRAINTS: usize = 32;
const MAX_CLAIMS_PER_DELTA: usize = 64;
const MAX_GOALS_PER_DELTA: usize = 64;
const MAX_TICKERS: usize = 32;
const MAX_EVIDENCE_REFS: usize = 64;
const MAX_CATALOG_SOURCES: usize = 4_096;
const MAX_CATALOG_CLAIMS: usize = 16_384;
const MAX_CATALOG_GOALS: usize = 4_096;
const MAX_RECENT_TURNS: usize = 16;
const MAX_RECENT_SOURCE_INDEX: usize = 64;
const MAX_CATALOG_TAIL_DELTAS: usize = 64;
const MAX_VIEW_CLAIMS: usize = 24;
const MAX_VIEW_GOALS: usize = 12;
const MAX_VIEW_TURNS: usize = 6;
pub const MAX_SESSION_MEMORY_VIEW_BYTES: usize = 256 * 1024;
pub const MAX_SESSION_MEMORY_SNAPSHOT_BYTES: usize = 4 * 1024 * 1024;
/// `PostgreSQL` stores the durable revision in a signed `bigint`. Rust keeps the
/// wire representation unsigned, but values outside the database domain are
/// rejected before they can enter a delta, snapshot, or frontier calculation.
pub const MAX_SESSION_MEMORY_REVISION: u64 = i64::MAX as u64;
const MIN_VIEW_BYTES: usize = 4 * 1024;
const SNAPSHOT_VISIBLE_BUDGET_BYTES: usize = 768 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryAuthority {
    ContextOnly,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemorySourceV3 {
    pub run_id: String,
    pub revision: u64,
    pub user_message_id: String,
    pub assistant_message_id: String,
    pub final_commit_intent_hash: ContentHash,
    pub answer_bundle_hash: ContentHash,
    pub final_output_hash: ContentHash,
}

impl fmt::Debug for MemorySourceV3 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MemorySourceV3")
            .field("run_id_hash", &ContentHash::sha256(&self.run_id))
            .field("revision", &self.revision)
            .field(
                "user_message_id_hash",
                &ContentHash::sha256(&self.user_message_id),
            )
            .field(
                "assistant_message_id_hash",
                &ContentHash::sha256(&self.assistant_message_id),
            )
            .field("final_commit_intent_hash", &self.final_commit_intent_hash)
            .field("answer_bundle_hash", &self.answer_bundle_hash)
            .field("final_output_hash", &self.final_output_hash)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserConstraintV3 {
    pub constraint_id: String,
    pub text: String,
    pub source_message_id: String,
    pub source_message_hash: ContentHash,
}

impl fmt::Debug for UserConstraintV3 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UserConstraintV3")
            .field("constraint_id", &self.constraint_id)
            .field("text", &"[REDACTED]")
            .field(
                "source_message_id_hash",
                &ContentHash::sha256(&self.source_message_id),
            )
            .field("source_message_hash", &self.source_message_hash)
            .finish()
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryClaimV3 {
    pub memory_id: String,
    pub source_run_id: String,
    pub claim: Claim,
    pub superseded_by: Option<String>,
}

impl fmt::Debug for MemoryClaimV3 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MemoryClaimV3")
            .field("memory_id", &self.memory_id)
            .field(
                "source_run_id_hash",
                &ContentHash::sha256(&self.source_run_id),
            )
            .field("claim_id", &self.claim.claim_id)
            .field("kind", &self.claim.kind)
            .field("strength", &self.claim.strength)
            .field("text", &"[REDACTED]")
            .field("superseded_by", &self.superseded_by)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryGoalV3 {
    pub memory_id: String,
    pub source_run_id: String,
    pub source_claim_id: Option<String>,
    pub text: String,
    pub evidence_ids: Vec<String>,
    pub resolved_by: Option<String>,
}

impl fmt::Debug for MemoryGoalV3 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MemoryGoalV3")
            .field("memory_id", &self.memory_id)
            .field(
                "source_run_id_hash",
                &ContentHash::sha256(&self.source_run_id),
            )
            .field("source_claim_id", &self.source_claim_id)
            .field("text", &"[REDACTED]")
            .field("evidence_count", &self.evidence_ids.len())
            .field("resolved_by", &self.resolved_by)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecentTurnV3 {
    pub source_run_id: String,
    pub user_content_hash: ContentHash,
    pub user_content: String,
    pub user_content_original_bytes: u64,
    pub user_content_truncated: bool,
    pub answer_content_hash: ContentHash,
    pub answer_content: String,
    pub answer_content_original_bytes: u64,
    pub answer_content_truncated: bool,
}

impl fmt::Debug for RecentTurnV3 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RecentTurnV3")
            .field(
                "source_run_id_hash",
                &ContentHash::sha256(&self.source_run_id),
            )
            .field("user_content_hash", &self.user_content_hash)
            .field("user_content", &"[REDACTED]")
            .field(
                "user_content_original_bytes",
                &self.user_content_original_bytes,
            )
            .field("user_content_truncated", &self.user_content_truncated)
            .field("answer_content_hash", &self.answer_content_hash)
            .field("answer_content", &"[REDACTED]")
            .field(
                "answer_content_original_bytes",
                &self.answer_content_original_bytes,
            )
            .field("answer_content_truncated", &self.answer_content_truncated)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemorySupersessionV3 {
    pub older_memory_id: String,
    pub newer_memory_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryGoalResolutionV3 {
    pub goal_memory_id: String,
    pub resolving_memory_id: String,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionMemoryDeltaV3 {
    pub schema_version: u16,
    pub authority: MemoryAuthority,
    pub session_id_hash: ContentHash,
    pub parent_frontier_hash: ContentHash,
    pub revision: u64,
    pub source: MemorySourceV3,
    pub tickers: Vec<String>,
    pub constraints: Vec<UserConstraintV3>,
    pub claims: Vec<MemoryClaimV3>,
    pub unresolved_goals: Vec<MemoryGoalV3>,
    pub supersessions: Vec<MemorySupersessionV3>,
    pub resolved_goals: Vec<MemoryGoalResolutionV3>,
    pub recent_turn: RecentTurnV3,
}

impl fmt::Debug for SessionMemoryDeltaV3 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SessionMemoryDeltaV3")
            .field("schema_version", &self.schema_version)
            .field("authority", &self.authority)
            .field("session_id_hash", &self.session_id_hash)
            .field("parent_frontier_hash", &self.parent_frontier_hash)
            .field("revision", &self.revision)
            .field("source", &self.source)
            .field("ticker_count", &self.tickers.len())
            .field("constraint_count", &self.constraints.len())
            .field("claim_count", &self.claims.len())
            .field("goal_count", &self.unresolved_goals.len())
            .field("supersession_count", &self.supersessions.len())
            .field("resolved_goal_count", &self.resolved_goals.len())
            .field("recent_turn", &self.recent_turn)
            .finish()
    }
}

impl SessionMemoryDeltaV3 {
    pub fn content_hash(&self) -> Result<ContentHash, MemoryError> {
        self.validate()?;
        Ok(ContentHash::sha256(serde_jcs::to_vec(self)?))
    }

    pub fn validate(&self) -> Result<(), MemoryError> {
        if self.schema_version != SESSION_MEMORY_SCHEMA_VERSION
            || self.authority != MemoryAuthority::ContextOnly
            || !(1..=MAX_SESSION_MEMORY_REVISION).contains(&self.revision)
        {
            return Err(MemoryError::InvalidEnvelope);
        }
        validate_source(&self.source)?;
        validate_tickers(&self.tickers)?;
        if self.constraints.len() > MAX_CONSTRAINTS
            || self.claims.len() > MAX_CLAIMS_PER_DELTA
            || self.unresolved_goals.len() > MAX_GOALS_PER_DELTA
            || self.supersessions.len() > MAX_CLAIMS_PER_DELTA
            || self.resolved_goals.len() > MAX_GOALS_PER_DELTA
        {
            return Err(MemoryError::Limit("delta items"));
        }
        let mut ids = BTreeSet::new();
        for constraint in &self.constraints {
            validate_constraint(constraint)?;
            if !ids.insert(constraint.constraint_id.as_str()) {
                return Err(MemoryError::DuplicateId);
            }
        }
        ids.clear();
        for claim in &self.claims {
            validate_memory_claim(claim)?;
            if claim.source_run_id != self.source.run_id || !ids.insert(claim.memory_id.as_str()) {
                return Err(MemoryError::InvalidLineage);
            }
            let expected = memory_claim_id(
                &self.source.run_id,
                &self.source.final_output_hash,
                &claim.claim.claim_id,
            )?;
            if claim.memory_id != expected || claim.superseded_by.is_some() {
                return Err(MemoryError::InvalidLineage);
            }
        }
        ids.clear();
        for goal in &self.unresolved_goals {
            validate_goal(goal)?;
            if goal.source_run_id != self.source.run_id
                || !ids.insert(goal.memory_id.as_str())
                || goal.resolved_by.is_some()
            {
                return Err(MemoryError::InvalidLineage);
            }
        }
        ids.clear();
        for supersession in &self.supersessions {
            if !bounded_id(&supersession.older_memory_id)
                || !bounded_id(&supersession.newer_memory_id)
                || supersession.older_memory_id == supersession.newer_memory_id
                || !ids.insert(supersession.older_memory_id.as_str())
            {
                return Err(MemoryError::InvalidSupersession);
            }
        }
        ids.clear();
        for resolution in &self.resolved_goals {
            if !bounded_id(&resolution.goal_memory_id)
                || !bounded_id(&resolution.resolving_memory_id)
                || !ids.insert(resolution.goal_memory_id.as_str())
            {
                return Err(MemoryError::InvalidLineage);
            }
        }
        validate_recent_turn(&self.recent_turn)?;
        if self.source.revision != self.revision
            || self.recent_turn.source_run_id != self.source.run_id
        {
            return Err(MemoryError::InvalidLineage);
        }
        ensure_canonical_size(self, MAX_SESSION_MEMORY_VIEW_BYTES * 4, "delta bytes")
    }

    pub fn next_frontier_hash(&self) -> Result<ContentHash, MemoryError> {
        let delta_hash = self.content_hash()?;
        Ok(next_frontier_hash(
            &self.parent_frontier_hash,
            self.revision,
            &delta_hash,
        ))
    }

    pub fn zeroize_sensitive(&mut self) {
        self.source.run_id.zeroize();
        self.source.user_message_id.zeroize();
        self.source.assistant_message_id.zeroize();
        for constraint in &mut self.constraints {
            constraint.text.zeroize();
            constraint.source_message_id.zeroize();
        }
        for claim in &mut self.claims {
            claim.source_run_id.zeroize();
            claim.claim.text.zeroize();
            scrub_optional(&mut claim.claim.subject);
            scrub_optional(&mut claim.claim.predicate);
            scrub_json_option(&mut claim.claim.value);
            scrub_optional(&mut claim.claim.unit);
            scrub_optional(&mut claim.claim.period);
            scrub_optional(&mut claim.claim.comparison_basis);
        }
        for goal in &mut self.unresolved_goals {
            goal.source_run_id.zeroize();
            goal.text.zeroize();
        }
        self.recent_turn.source_run_id.zeroize();
        self.recent_turn.user_content.zeroize();
        self.recent_turn.answer_content.zeroize();
    }
}

impl Drop for SessionMemoryDeltaV3 {
    fn drop(&mut self) {
        self.zeroize_sensitive();
    }
}

/// Canonical, bounded projection checkpoint for restoring a long-running
/// session without replaying its full append-only delta audit. The compact
/// indexes preserve duplicate, supersession, resolution, and constraint
/// lineage even when old bodies are no longer exposed as context.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionMemorySnapshotV3 {
    pub schema_version: u16,
    pub authority: MemoryAuthority,
    pub session_id_hash: ContentHash,
    pub revision: u64,
    pub frontier_hash: ContentHash,
    pub source_lineage_hash: ContentHash,
    pub recent_source_revision_index: BTreeMap<String, u64>,
    pub constraint_hashes: BTreeMap<String, ContentHash>,
    pub active_claim_fingerprints: BTreeMap<String, ContentHash>,
    pub unresolved_goal_ids: Vec<String>,
    pub tickers: Vec<String>,
    pub constraints: Vec<UserConstraintV3>,
    pub sources: BTreeMap<String, MemorySourceV3>,
    pub claims: Vec<MemoryClaimV3>,
    pub unresolved_goals: Vec<MemoryGoalV3>,
    pub recent_turns: Vec<RecentTurnV3>,
}

impl fmt::Debug for SessionMemorySnapshotV3 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SessionMemorySnapshotV3")
            .field("schema_version", &self.schema_version)
            .field("authority", &self.authority)
            .field("session_id_hash", &self.session_id_hash)
            .field("revision", &self.revision)
            .field("frontier_hash", &self.frontier_hash)
            .field("source_lineage_hash", &self.source_lineage_hash)
            .field(
                "recent_source_index_count",
                &self.recent_source_revision_index.len(),
            )
            .field("constraint_index_count", &self.constraint_hashes.len())
            .field(
                "active_claim_index_count",
                &self.active_claim_fingerprints.len(),
            )
            .field(
                "unresolved_goal_index_count",
                &self.unresolved_goal_ids.len(),
            )
            .field("ticker_count", &self.tickers.len())
            .field("visible_constraint_count", &self.constraints.len())
            .field("visible_source_count", &self.sources.len())
            .field("visible_claim_count", &self.claims.len())
            .field("visible_goal_count", &self.unresolved_goals.len())
            .field("recent_turn_count", &self.recent_turns.len())
            .finish()
    }
}

impl SessionMemorySnapshotV3 {
    pub fn content_hash(&self) -> Result<ContentHash, MemoryError> {
        self.validate()?;
        Ok(ContentHash::sha256(serde_jcs::to_vec(self)?))
    }

    pub fn canonical_bytes(&self) -> Result<Vec<u8>, MemoryError> {
        self.validate()?;
        Ok(serde_jcs::to_vec(self)?)
    }

    pub fn validate(&self) -> Result<(), MemoryError> {
        if self.schema_version != SESSION_MEMORY_SCHEMA_VERSION
            || self.authority != MemoryAuthority::ContextOnly
            || !(1..=MAX_SESSION_MEMORY_REVISION).contains(&self.revision)
            || self.recent_source_revision_index.len() > MAX_RECENT_SOURCE_INDEX
            || self.constraint_hashes.len() > MAX_CONSTRAINTS
            || self.active_claim_fingerprints.len() > MAX_CATALOG_CLAIMS
            || self.unresolved_goal_ids.len() > MAX_CATALOG_GOALS
            || self.sources.len() > MAX_CATALOG_SOURCES
            || self.claims.len() > MAX_CATALOG_CLAIMS
            || self.unresolved_goals.len() > MAX_CATALOG_GOALS
            || self.recent_turns.len() > MAX_RECENT_TURNS
        {
            return Err(MemoryError::InvalidEnvelope);
        }
        validate_tickers(&self.tickers)?;
        validate_recent_source_index(&self.recent_source_revision_index, self.revision)?;
        if self.constraint_hashes.len() != self.constraints.len()
            || self.active_claim_fingerprints.len() != self.claims.len()
            || self.unresolved_goal_ids.len() != self.unresolved_goals.len()
        {
            return Err(MemoryError::InvalidLineage);
        }
        let recent_start = self
            .revision
            .checked_sub(self.recent_source_revision_index.len() as u64)
            .and_then(|value| value.checked_add(1))
            .ok_or(MemoryError::Overflow)?;
        let mut visible_source_revisions = BTreeSet::new();
        for (run_id, source) in &self.sources {
            validate_source(source)?;
            if run_id != &source.run_id
                || source.revision > self.revision
                || !visible_source_revisions.insert(source.revision)
                || (source.revision >= recent_start
                    && self
                        .recent_source_revision_index
                        .get(ContentHash::sha256(run_id).as_str())
                        != Some(&source.revision))
            {
                return Err(MemoryError::InvalidLineage);
            }
        }
        for (constraint_id, hash) in &self.constraint_hashes {
            if !bounded_id(constraint_id) || hash.as_str().is_empty() {
                return Err(MemoryError::InvalidLineage);
            }
        }
        let mut visible_constraint_ids = BTreeSet::new();
        for constraint in &self.constraints {
            validate_constraint(constraint)?;
            if !visible_constraint_ids.insert(constraint.constraint_id.as_str())
                || self.constraint_hashes.get(&constraint.constraint_id)
                    != Some(&constraint_fingerprint(constraint)?)
            {
                return Err(MemoryError::InvalidLineage);
            }
        }
        if self
            .active_claim_fingerprints
            .keys()
            .any(|memory_id| ContentHash::parse(memory_id.clone()).is_err())
        {
            return Err(MemoryError::InvalidLineage);
        }
        let mut visible_claim_ids = BTreeSet::new();
        for claim in &self.claims {
            validate_memory_claim(claim)?;
            if claim.superseded_by.is_some()
                || !visible_claim_ids.insert(claim.memory_id.as_str())
                || self.active_claim_fingerprints.get(&claim.memory_id)
                    != Some(&claim_fingerprint(claim)?)
                || !self.sources.contains_key(&claim.source_run_id)
            {
                return Err(MemoryError::InvalidLineage);
            }
        }
        let unresolved_ids = self
            .unresolved_goal_ids
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        if unresolved_ids.len() != self.unresolved_goal_ids.len()
            || self
                .unresolved_goal_ids
                .iter()
                .any(|memory_id| ContentHash::parse(memory_id.clone()).is_err())
        {
            return Err(MemoryError::InvalidLineage);
        }
        let mut visible_goal_ids = BTreeSet::new();
        for goal in &self.unresolved_goals {
            validate_goal(goal)?;
            if goal.resolved_by.is_some()
                || !visible_goal_ids.insert(goal.memory_id.as_str())
                || !unresolved_ids.contains(goal.memory_id.as_str())
                || !self.sources.contains_key(&goal.source_run_id)
            {
                return Err(MemoryError::InvalidLineage);
            }
        }
        let mut previous_turn_revision = 0_u64;
        for turn in &self.recent_turns {
            validate_recent_turn(turn)?;
            let source = self
                .sources
                .get(&turn.source_run_id)
                .ok_or(MemoryError::InvalidLineage)?;
            if source.revision <= previous_turn_revision {
                return Err(MemoryError::InvalidLineage);
            }
            previous_turn_revision = source.revision;
        }
        ensure_canonical_size(self, MAX_SESSION_MEMORY_SNAPSHOT_BYTES, "snapshot bytes")
    }

    pub fn zeroize_sensitive(&mut self) {
        for constraint in &mut self.constraints {
            constraint.text.zeroize();
            constraint.source_message_id.zeroize();
        }
        for source in self.sources.values_mut() {
            source.run_id.zeroize();
            source.user_message_id.zeroize();
            source.assistant_message_id.zeroize();
        }
        for claim in &mut self.claims {
            claim.source_run_id.zeroize();
            claim.claim.text.zeroize();
            scrub_optional(&mut claim.claim.subject);
            scrub_optional(&mut claim.claim.predicate);
            scrub_json_option(&mut claim.claim.value);
            scrub_optional(&mut claim.claim.unit);
            scrub_optional(&mut claim.claim.period);
            scrub_optional(&mut claim.claim.comparison_basis);
        }
        for goal in &mut self.unresolved_goals {
            goal.source_run_id.zeroize();
            goal.text.zeroize();
        }
        for turn in &mut self.recent_turns {
            turn.source_run_id.zeroize();
            turn.user_content.zeroize();
            turn.answer_content.zeroize();
        }
    }
}

impl Drop for SessionMemorySnapshotV3 {
    fn drop(&mut self) {
        self.zeroize_sensitive();
    }
}

/// Append-only storage model used by deterministic tests and small local
/// deployments. Production storage may normalize the same records into rows;
/// it must preserve the frontier compare-and-swap semantics.
#[derive(Clone, PartialEq)]
pub struct SessionMemoryCatalogV3 {
    schema_version: u16,
    authority: MemoryAuthority,
    session_id_hash: ContentHash,
    revision: u64,
    frontier_hash: ContentHash,
    anchor_revision: u64,
    anchor_frontier_hash: ContentHash,
    anchor_source_lineage_hash: ContentHash,
    source_lineage_hash: ContentHash,
    delta_hashes: Vec<ContentHash>,
    tail_source_run_hashes: Vec<ContentHash>,
    recent_source_revision_index: BTreeMap<String, u64>,
    constraint_hashes: BTreeMap<String, ContentHash>,
    active_claim_fingerprints: BTreeMap<String, ContentHash>,
    unresolved_goal_ids: BTreeSet<String>,
    sources: BTreeMap<String, MemorySourceV3>,
    tickers: BTreeSet<String>,
    constraints: BTreeMap<String, UserConstraintV3>,
    claims: BTreeMap<String, MemoryClaimV3>,
    unresolved_goals: BTreeMap<String, MemoryGoalV3>,
    recent_turns: Vec<RecentTurnV3>,
}

impl fmt::Debug for SessionMemoryCatalogV3 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SessionMemoryCatalogV3")
            .field("schema_version", &self.schema_version)
            .field("authority", &self.authority)
            .field("session_id_hash", &self.session_id_hash)
            .field("revision", &self.revision)
            .field("frontier_hash", &self.frontier_hash)
            .field("anchor_revision", &self.anchor_revision)
            .field("anchor_frontier_hash", &self.anchor_frontier_hash)
            .field(
                "anchor_source_lineage_hash",
                &self.anchor_source_lineage_hash,
            )
            .field("source_lineage_hash", &self.source_lineage_hash)
            .field("delta_count", &self.delta_hashes.len())
            .field(
                "tail_source_run_hash_count",
                &self.tail_source_run_hashes.len(),
            )
            .field(
                "recent_source_index_count",
                &self.recent_source_revision_index.len(),
            )
            .field("constraint_index_count", &self.constraint_hashes.len())
            .field(
                "active_claim_index_count",
                &self.active_claim_fingerprints.len(),
            )
            .field(
                "unresolved_goal_index_count",
                &self.unresolved_goal_ids.len(),
            )
            .field("source_count", &self.sources.len())
            .field("ticker_count", &self.tickers.len())
            .field("constraint_count", &self.constraints.len())
            .field("claim_count", &self.claims.len())
            .field("goal_count", &self.unresolved_goals.len())
            .field("recent_turn_count", &self.recent_turns.len())
            .finish()
    }
}

impl SessionMemoryCatalogV3 {
    pub fn new(session_id: &str) -> Result<Self, MemoryError> {
        if !bounded_id(session_id) {
            return Err(MemoryError::InvalidId("session_id"));
        }
        Ok(Self {
            schema_version: SESSION_MEMORY_SCHEMA_VERSION,
            authority: MemoryAuthority::ContextOnly,
            session_id_hash: ContentHash::sha256(session_id),
            revision: 0,
            frontier_hash: empty_frontier_hash(),
            anchor_revision: 0,
            anchor_frontier_hash: empty_frontier_hash(),
            anchor_source_lineage_hash: empty_source_lineage_hash(),
            source_lineage_hash: empty_source_lineage_hash(),
            delta_hashes: Vec::new(),
            tail_source_run_hashes: Vec::new(),
            recent_source_revision_index: BTreeMap::new(),
            constraint_hashes: BTreeMap::new(),
            active_claim_fingerprints: BTreeMap::new(),
            unresolved_goal_ids: BTreeSet::new(),
            sources: BTreeMap::new(),
            tickers: BTreeSet::new(),
            constraints: BTreeMap::new(),
            claims: BTreeMap::new(),
            unresolved_goals: BTreeMap::new(),
            recent_turns: Vec::new(),
        })
    }

    pub const fn revision(&self) -> u64 {
        self.revision
    }

    pub const fn authority(&self) -> MemoryAuthority {
        self.authority
    }

    pub fn frontier_hash(&self) -> &ContentHash {
        &self.frontier_hash
    }

    pub fn source_lineage_hash(&self) -> &ContentHash {
        &self.source_lineage_hash
    }

    pub fn claims(&self) -> &BTreeMap<String, MemoryClaimV3> {
        &self.claims
    }

    pub fn recent_turns(&self) -> &[RecentTurnV3] {
        &self.recent_turns
    }

    pub fn apply_delta(&mut self, delta: &SessionMemoryDeltaV3) -> Result<(), MemoryError> {
        self.validate()?;
        delta.validate()?;
        if delta.session_id_hash != self.session_id_hash
            || delta.parent_frontier_hash != self.frontier_hash
            || delta.revision != self.revision.checked_add(1).ok_or(MemoryError::Overflow)?
            || delta.revision > MAX_SESSION_MEMORY_REVISION
        {
            return Err(MemoryError::FrontierMismatch);
        }
        let source_run_hash = ContentHash::sha256(&delta.source.run_id);
        if self
            .recent_source_revision_index
            .contains_key(source_run_hash.as_str())
            || self.sources.contains_key(&delta.source.run_id)
        {
            return Err(MemoryError::FrontierMismatch);
        }
        if self.delta_hashes.len() >= MAX_CATALOG_TAIL_DELTAS
            || self.sources.len() >= MAX_CATALOG_SOURCES
            || self.claims.len().saturating_add(delta.claims.len()) > MAX_CATALOG_CLAIMS
            || self
                .unresolved_goals
                .len()
                .saturating_add(delta.unresolved_goals.len())
                > MAX_CATALOG_GOALS
        {
            return Err(MemoryError::Limit("catalog items"));
        }

        let delta_hash = delta.content_hash()?;
        let expected_frontier =
            next_frontier_hash(&self.frontier_hash, delta.revision, &delta_hash);
        let expected_source_lineage =
            next_source_lineage_hash(&self.source_lineage_hash, delta.revision, &source_run_hash);
        let mut candidate = self.clone();
        candidate
            .sources
            .insert(delta.source.run_id.clone(), delta.source.clone());
        candidate
            .recent_source_revision_index
            .insert(source_run_hash.to_string(), delta.revision);
        prune_recent_source_index(&mut candidate.recent_source_revision_index);
        candidate.tickers.extend(delta.tickers.iter().cloned());
        for constraint in &delta.constraints {
            let fingerprint = constraint_fingerprint(constraint)?;
            match candidate.constraint_hashes.get(&constraint.constraint_id) {
                Some(existing) if existing != &fingerprint => {
                    return Err(MemoryError::ConflictingId);
                }
                Some(_) => {
                    candidate
                        .constraints
                        .insert(constraint.constraint_id.clone(), constraint.clone());
                }
                None => {
                    candidate
                        .constraint_hashes
                        .insert(constraint.constraint_id.clone(), fingerprint);
                    candidate
                        .constraints
                        .insert(constraint.constraint_id.clone(), constraint.clone());
                }
            }
        }
        for claim in &delta.claims {
            if candidate
                .active_claim_fingerprints
                .contains_key(&claim.memory_id)
                || candidate
                    .claims
                    .insert(claim.memory_id.clone(), claim.clone())
                    .is_some()
            {
                return Err(MemoryError::ConflictingId);
            }
            candidate
                .active_claim_fingerprints
                .insert(claim.memory_id.clone(), claim_fingerprint(claim)?);
        }
        for goal in &delta.unresolved_goals {
            if candidate.unresolved_goal_ids.contains(&goal.memory_id)
                || candidate
                    .unresolved_goals
                    .insert(goal.memory_id.clone(), goal.clone())
                    .is_some()
            {
                return Err(MemoryError::ConflictingId);
            }
            candidate.unresolved_goal_ids.insert(goal.memory_id.clone());
        }
        for supersession in &delta.supersessions {
            apply_supersession(
                &mut candidate.claims,
                &mut candidate.active_claim_fingerprints,
                supersession,
            )?;
        }
        for resolution in &delta.resolved_goals {
            if !candidate
                .active_claim_fingerprints
                .contains_key(&resolution.resolving_memory_id)
            {
                return Err(MemoryError::UnknownMemory);
            }
            if !candidate
                .unresolved_goal_ids
                .remove(&resolution.goal_memory_id)
            {
                return Err(MemoryError::UnknownMemory);
            }
            if let Some(goal) = candidate
                .unresolved_goals
                .get_mut(&resolution.goal_memory_id)
            {
                if goal.resolved_by.is_some() {
                    return Err(MemoryError::ConflictingId);
                }
                goal.resolved_by = Some(resolution.resolving_memory_id.clone());
            }
        }
        candidate.recent_turns.push(delta.recent_turn.clone());
        if candidate.recent_turns.len() > MAX_RECENT_TURNS {
            let discard = candidate.recent_turns.len() - MAX_RECENT_TURNS;
            candidate.recent_turns.drain(..discard);
        }
        candidate.revision = delta.revision;
        candidate.delta_hashes.push(delta_hash);
        candidate.tail_source_run_hashes.push(source_run_hash);
        candidate.frontier_hash = expected_frontier;
        candidate.source_lineage_hash = expected_source_lineage;
        candidate.validate()?;
        *self = candidate;
        Ok(())
    }

    pub fn from_snapshot(
        session_id: &str,
        snapshot: &SessionMemorySnapshotV3,
    ) -> Result<Self, MemoryError> {
        snapshot.validate()?;
        if !bounded_id(session_id) || snapshot.session_id_hash != ContentHash::sha256(session_id) {
            return Err(MemoryError::InvalidEnvelope);
        }
        let constraints = snapshot
            .constraints
            .iter()
            .cloned()
            .map(|value| (value.constraint_id.clone(), value))
            .collect::<BTreeMap<_, _>>();
        let claims = snapshot
            .claims
            .iter()
            .cloned()
            .map(|value| (value.memory_id.clone(), value))
            .collect::<BTreeMap<_, _>>();
        let unresolved_goals = snapshot
            .unresolved_goals
            .iter()
            .cloned()
            .map(|value| (value.memory_id.clone(), value))
            .collect::<BTreeMap<_, _>>();
        let catalog = Self {
            schema_version: snapshot.schema_version,
            authority: snapshot.authority,
            session_id_hash: snapshot.session_id_hash.clone(),
            revision: snapshot.revision,
            frontier_hash: snapshot.frontier_hash.clone(),
            anchor_revision: snapshot.revision,
            anchor_frontier_hash: snapshot.frontier_hash.clone(),
            anchor_source_lineage_hash: snapshot.source_lineage_hash.clone(),
            source_lineage_hash: snapshot.source_lineage_hash.clone(),
            delta_hashes: Vec::new(),
            tail_source_run_hashes: Vec::new(),
            recent_source_revision_index: snapshot.recent_source_revision_index.clone(),
            constraint_hashes: snapshot.constraint_hashes.clone(),
            active_claim_fingerprints: snapshot.active_claim_fingerprints.clone(),
            unresolved_goal_ids: snapshot.unresolved_goal_ids.iter().cloned().collect(),
            sources: snapshot.sources.clone(),
            tickers: snapshot.tickers.iter().cloned().collect(),
            constraints,
            claims,
            unresolved_goals,
            recent_turns: snapshot.recent_turns.clone(),
        };
        catalog.validate()?;
        Ok(catalog)
    }

    pub fn snapshot(&self) -> Result<SessionMemorySnapshotV3, MemoryError> {
        self.validate()?;
        if self.revision == 0 {
            return Err(MemoryError::InvalidEnvelope);
        }
        let mut snapshot = SessionMemorySnapshotV3 {
            schema_version: self.schema_version,
            authority: self.authority,
            session_id_hash: self.session_id_hash.clone(),
            revision: self.revision,
            frontier_hash: self.frontier_hash.clone(),
            source_lineage_hash: self.source_lineage_hash.clone(),
            recent_source_revision_index: self.recent_source_revision_index.clone(),
            constraint_hashes: BTreeMap::new(),
            active_claim_fingerprints: BTreeMap::new(),
            unresolved_goal_ids: Vec::new(),
            tickers: self.tickers.iter().cloned().collect(),
            constraints: Vec::new(),
            sources: BTreeMap::new(),
            claims: Vec::new(),
            unresolved_goals: Vec::new(),
            recent_turns: Vec::new(),
        };
        let base_bytes = serde_jcs::to_vec(&snapshot)?.len();
        let reserve = 16 * 1024;
        let mut remaining = MAX_SESSION_MEMORY_SNAPSHOT_BYTES
            .checked_sub(base_bytes.saturating_add(reserve))
            .ok_or(MemoryError::Limit("snapshot lineage bytes"))?
            .min(SNAPSHOT_VISIBLE_BUDGET_BYTES);

        for constraint in self.constraints.values() {
            let cost = canonical_item_cost(constraint)?;
            if cost <= remaining {
                snapshot.constraints.push(constraint.clone());
                snapshot.constraint_hashes.insert(
                    constraint.constraint_id.clone(),
                    constraint_fingerprint(constraint)?,
                );
                remaining -= cost;
            }
        }

        let mut recent_turns = Vec::new();
        for turn in self.recent_turns.iter().rev() {
            let source = self
                .sources
                .get(&turn.source_run_id)
                .ok_or(MemoryError::InvalidLineage)?;
            let source_cost = if snapshot.sources.contains_key(&turn.source_run_id) {
                0
            } else {
                canonical_item_cost(source)?.saturating_add(turn.source_run_id.len() + 32)
            };
            let cost = canonical_item_cost(turn)?.saturating_add(source_cost);
            if cost <= remaining {
                snapshot
                    .sources
                    .insert(turn.source_run_id.clone(), source.clone());
                recent_turns.push(turn.clone());
                remaining -= cost;
            }
        }
        recent_turns.reverse();
        snapshot.recent_turns = recent_turns;

        let mut claims = self
            .claims
            .values()
            .filter(|claim| {
                claim.superseded_by.is_none()
                    && self
                        .active_claim_fingerprints
                        .contains_key(&claim.memory_id)
            })
            .collect::<Vec<_>>();
        claims.sort_by_key(|claim| {
            (
                Reverse(
                    self.sources
                        .get(&claim.source_run_id)
                        .map_or(0, |source| source.revision),
                ),
                claim.memory_id.as_str(),
            )
        });
        for claim in claims {
            let source = self
                .sources
                .get(&claim.source_run_id)
                .ok_or(MemoryError::InvalidLineage)?;
            let source_cost = if snapshot.sources.contains_key(&claim.source_run_id) {
                0
            } else {
                canonical_item_cost(source)?.saturating_add(claim.source_run_id.len() + 32)
            };
            let cost = canonical_item_cost(claim)?.saturating_add(source_cost);
            if cost <= remaining {
                snapshot
                    .sources
                    .insert(claim.source_run_id.clone(), source.clone());
                snapshot.claims.push(claim.clone());
                snapshot
                    .active_claim_fingerprints
                    .insert(claim.memory_id.clone(), claim_fingerprint(claim)?);
                remaining -= cost;
            }
        }

        let mut goals = self
            .unresolved_goals
            .values()
            .filter(|goal| {
                goal.resolved_by.is_none() && self.unresolved_goal_ids.contains(&goal.memory_id)
            })
            .collect::<Vec<_>>();
        goals.sort_by_key(|goal| {
            (
                Reverse(
                    self.sources
                        .get(&goal.source_run_id)
                        .map_or(0, |source| source.revision),
                ),
                goal.memory_id.as_str(),
            )
        });
        for goal in goals {
            let source = self
                .sources
                .get(&goal.source_run_id)
                .ok_or(MemoryError::InvalidLineage)?;
            let source_cost = if snapshot.sources.contains_key(&goal.source_run_id) {
                0
            } else {
                canonical_item_cost(source)?.saturating_add(goal.source_run_id.len() + 32)
            };
            let cost = canonical_item_cost(goal)?.saturating_add(source_cost);
            if cost <= remaining {
                snapshot
                    .sources
                    .insert(goal.source_run_id.clone(), source.clone());
                snapshot.unresolved_goals.push(goal.clone());
                snapshot.unresolved_goal_ids.push(goal.memory_id.clone());
                remaining -= cost;
            }
        }
        snapshot.validate()?;
        Ok(snapshot)
    }

    pub fn select_view(
        &self,
        question: &str,
        max_bytes: usize,
    ) -> Result<SessionMemoryViewV3, MemoryError> {
        self.validate()?;
        if !bounded_text(question, MAX_TEXT_BYTES)
            || !(MIN_VIEW_BYTES..=MAX_SESSION_MEMORY_VIEW_BYTES).contains(&max_bytes)
        {
            return Err(MemoryError::Limit("view request"));
        }
        let query_terms = terms(question);
        let source_recency = self
            .sources
            .iter()
            .map(|(run_id, source)| {
                (
                    run_id.as_str(),
                    usize::try_from(source.revision).unwrap_or(usize::MAX),
                )
            })
            .collect::<BTreeMap<_, _>>();

        let mut claims = self
            .claims
            .values()
            .filter(|claim| claim.superseded_by.is_none())
            .map(|claim| {
                let text = claim_search_text(claim);
                let recency = source_recency
                    .get(claim.source_run_id.as_str())
                    .copied()
                    .unwrap_or_default();
                (score(&query_terms, &terms(&text), recency), claim)
            })
            .collect::<Vec<_>>();
        claims.sort_by_key(|(score, claim)| (Reverse(*score), claim.memory_id.as_str()));

        let mut goals = self
            .unresolved_goals
            .values()
            .filter(|goal| goal.resolved_by.is_none())
            .map(|goal| {
                let recency = source_recency
                    .get(goal.source_run_id.as_str())
                    .copied()
                    .unwrap_or_default();
                (
                    score(&query_terms, &terms(&goal.text), recency).saturating_add(8),
                    goal,
                )
            })
            .collect::<Vec<_>>();
        goals.sort_by_key(|(score, goal)| (Reverse(*score), goal.memory_id.as_str()));

        let mut turns = self
            .recent_turns
            .iter()
            .enumerate()
            .map(|(index, turn)| {
                let searchable = format!("{} {}", turn.user_content, turn.answer_content);
                (score(&query_terms, &terms(&searchable), index), turn)
            })
            .collect::<Vec<_>>();
        turns.sort_by_key(|(score, turn)| (Reverse(*score), turn.source_run_id.as_str()));

        let selected_claims = claims
            .into_iter()
            .take(MAX_VIEW_CLAIMS)
            .map(|(_, claim)| claim.clone())
            .collect::<Vec<_>>();
        let selected_goals = goals
            .into_iter()
            .take(MAX_VIEW_GOALS)
            .map(|(_, goal)| goal.clone())
            .collect::<Vec<_>>();
        let selected_turns = turns
            .into_iter()
            .take(MAX_VIEW_TURNS)
            .map(|(_, turn)| turn.clone())
            .collect::<Vec<_>>();
        let source_ids = selected_claims
            .iter()
            .map(|claim| claim.source_run_id.as_str())
            .chain(
                selected_goals
                    .iter()
                    .map(|goal| goal.source_run_id.as_str()),
            )
            .chain(
                selected_turns
                    .iter()
                    .map(|turn| turn.source_run_id.as_str()),
            )
            .collect::<BTreeSet<_>>();
        let sources = source_ids
            .into_iter()
            .map(|run_id| {
                self.sources
                    .get(run_id)
                    .cloned()
                    .map(|source| (run_id.to_owned(), source))
                    .ok_or(MemoryError::InvalidLineage)
            })
            .collect::<Result<BTreeMap<_, _>, _>>()?;
        let mut view = SessionMemoryViewV3 {
            schema_version: SESSION_MEMORY_SCHEMA_VERSION,
            authority: MemoryAuthority::ContextOnly,
            session_id_hash: self.session_id_hash.clone(),
            source_frontier_hash: self.frontier_hash.clone(),
            source_revision: self.revision,
            view_hash: empty_frontier_hash(),
            tickers: self.tickers.iter().cloned().collect(),
            constraints: self.constraints.values().cloned().collect(),
            sources,
            claims: selected_claims,
            unresolved_goals: selected_goals,
            recent_turns: selected_turns,
        };
        shrink_view_to_fit(&mut view, max_bytes)?;
        view.view_hash = view.compute_view_hash()?;
        view.validate(max_bytes)?;
        Ok(view)
    }

    pub fn validate(&self) -> Result<(), MemoryError> {
        let tail_revision_count = self
            .revision
            .checked_sub(self.anchor_revision)
            .ok_or(MemoryError::InvalidEnvelope)?;
        if self.schema_version != SESSION_MEMORY_SCHEMA_VERSION
            || self.authority != MemoryAuthority::ContextOnly
            || self.revision > MAX_SESSION_MEMORY_REVISION
            || self.anchor_revision > self.revision
            || (self.anchor_revision == 0 && self.anchor_frontier_hash != empty_frontier_hash())
            || (self.anchor_revision == 0
                && self.anchor_source_lineage_hash != empty_source_lineage_hash())
            || self.sources.len() > MAX_CATALOG_SOURCES
            || self.recent_source_revision_index.len() > MAX_RECENT_SOURCE_INDEX
            || self.claims.len() > MAX_CATALOG_CLAIMS
            || self.active_claim_fingerprints.len() > MAX_CATALOG_CLAIMS
            || self.unresolved_goals.len() > MAX_CATALOG_GOALS
            || self.unresolved_goal_ids.len() > MAX_CATALOG_GOALS
            || self.recent_turns.len() > MAX_RECENT_TURNS
            || self.constraints.len() > MAX_CONSTRAINTS
            || self.constraint_hashes.len() > MAX_CONSTRAINTS
            || self.tickers.len() > MAX_TICKERS
            || self.delta_hashes.len() > MAX_CATALOG_TAIL_DELTAS
            || self.delta_hashes.len() != self.tail_source_run_hashes.len()
            || usize::try_from(tail_revision_count).ok() != Some(self.delta_hashes.len())
        {
            return Err(MemoryError::InvalidEnvelope);
        }
        if self.revision == 0 {
            if !self.recent_source_revision_index.is_empty()
                || self.frontier_hash != empty_frontier_hash()
                || self.source_lineage_hash != empty_source_lineage_hash()
            {
                return Err(MemoryError::InvalidLineage);
            }
        } else {
            validate_recent_source_index(&self.recent_source_revision_index, self.revision)?;
        }
        let recent_start = self
            .revision
            .checked_sub(self.recent_source_revision_index.len() as u64)
            .and_then(|value| value.checked_add(1))
            .unwrap_or(1);
        let mut visible_source_revisions = BTreeSet::new();
        for (run_id, source) in &self.sources {
            validate_source(source)?;
            if run_id != &source.run_id
                || source.revision > self.revision
                || !visible_source_revisions.insert(source.revision)
                || (source.revision >= recent_start
                    && self
                        .recent_source_revision_index
                        .get(ContentHash::sha256(run_id).as_str())
                        != Some(&source.revision))
            {
                return Err(MemoryError::InvalidLineage);
            }
        }
        for (id, hash) in &self.constraint_hashes {
            if !bounded_id(id) || hash.as_str().is_empty() {
                return Err(MemoryError::InvalidLineage);
            }
        }
        for (id, constraint) in &self.constraints {
            validate_constraint(constraint)?;
            if id != &constraint.constraint_id
                || self.constraint_hashes.get(id) != Some(&constraint_fingerprint(constraint)?)
            {
                return Err(MemoryError::InvalidLineage);
            }
        }
        if self
            .active_claim_fingerprints
            .keys()
            .any(|id| ContentHash::parse(id.clone()).is_err())
        {
            return Err(MemoryError::InvalidLineage);
        }
        for (id, claim) in &self.claims {
            validate_memory_claim(claim)?;
            if id != &claim.memory_id || !self.sources.contains_key(&claim.source_run_id) {
                return Err(MemoryError::InvalidLineage);
            }
            let fingerprint = claim_fingerprint(claim)?;
            if let Some(newer_id) = &claim.superseded_by {
                let newer_fingerprint = self
                    .active_claim_fingerprints
                    .get(newer_id)
                    .cloned()
                    .or_else(|| {
                        self.claims
                            .get(newer_id)
                            .and_then(|newer| claim_fingerprint(newer).ok())
                    })
                    .ok_or(MemoryError::InvalidLineage)?;
                if self.active_claim_fingerprints.contains_key(id)
                    || fingerprint != newer_fingerprint
                {
                    return Err(MemoryError::InvalidSupersession);
                }
            } else if self.active_claim_fingerprints.get(id) != Some(&fingerprint) {
                return Err(MemoryError::InvalidLineage);
            }
        }
        if self
            .unresolved_goal_ids
            .iter()
            .any(|id| ContentHash::parse(id.clone()).is_err())
        {
            return Err(MemoryError::InvalidLineage);
        }
        for (id, goal) in &self.unresolved_goals {
            validate_goal(goal)?;
            if id != &goal.memory_id || !self.sources.contains_key(&goal.source_run_id) {
                return Err(MemoryError::InvalidLineage);
            }
            if let Some(resolving) = &goal.resolved_by {
                if self.unresolved_goal_ids.contains(id)
                    || (!self.active_claim_fingerprints.contains_key(resolving)
                        && !self.claims.contains_key(resolving))
                {
                    return Err(MemoryError::InvalidLineage);
                }
            } else if !self.unresolved_goal_ids.contains(id) {
                return Err(MemoryError::InvalidLineage);
            }
        }
        for turn in &self.recent_turns {
            validate_recent_turn(turn)?;
            if !self.sources.contains_key(&turn.source_run_id) {
                return Err(MemoryError::InvalidLineage);
            }
        }
        validate_tickers(&self.tickers.iter().cloned().collect::<Vec<_>>())?;
        if self.compute_frontier_hash() != self.frontier_hash
            || self.compute_source_lineage_hash() != self.source_lineage_hash
        {
            return Err(MemoryError::FrontierMismatch);
        }
        Ok(())
    }

    fn compute_frontier_hash(&self) -> ContentHash {
        self.delta_hashes.iter().enumerate().fold(
            self.anchor_frontier_hash.clone(),
            |parent, (index, delta_hash)| {
                next_frontier_hash(
                    &parent,
                    self.anchor_revision
                        .saturating_add(u64::try_from(index).unwrap_or(u64::MAX))
                        .saturating_add(1),
                    delta_hash,
                )
            },
        )
    }

    fn compute_source_lineage_hash(&self) -> ContentHash {
        self.tail_source_run_hashes.iter().enumerate().fold(
            self.anchor_source_lineage_hash.clone(),
            |parent, (index, source_run_hash)| {
                next_source_lineage_hash(
                    &parent,
                    self.anchor_revision
                        .saturating_add(u64::try_from(index).unwrap_or(u64::MAX))
                        .saturating_add(1),
                    source_run_hash,
                )
            },
        )
    }

    fn zeroize_sensitive(&mut self) {
        for source in self.sources.values_mut() {
            source.run_id.zeroize();
            source.user_message_id.zeroize();
            source.assistant_message_id.zeroize();
        }
        for constraint in self.constraints.values_mut() {
            constraint.text.zeroize();
            constraint.source_message_id.zeroize();
        }
        for claim in self.claims.values_mut() {
            claim.source_run_id.zeroize();
            claim.claim.text.zeroize();
            scrub_optional(&mut claim.claim.subject);
            scrub_optional(&mut claim.claim.predicate);
            scrub_json_option(&mut claim.claim.value);
            scrub_optional(&mut claim.claim.unit);
            scrub_optional(&mut claim.claim.period);
            scrub_optional(&mut claim.claim.comparison_basis);
        }
        for goal in self.unresolved_goals.values_mut() {
            goal.source_run_id.zeroize();
            goal.text.zeroize();
        }
        for turn in &mut self.recent_turns {
            turn.source_run_id.zeroize();
            turn.user_content.zeroize();
            turn.answer_content.zeroize();
        }
    }
}

impl Drop for SessionMemoryCatalogV3 {
    fn drop(&mut self) {
        self.zeroize_sensitive();
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionMemoryViewV3 {
    pub schema_version: u16,
    pub authority: MemoryAuthority,
    pub session_id_hash: ContentHash,
    pub source_frontier_hash: ContentHash,
    pub source_revision: u64,
    pub view_hash: ContentHash,
    pub tickers: Vec<String>,
    pub constraints: Vec<UserConstraintV3>,
    pub sources: BTreeMap<String, MemorySourceV3>,
    pub claims: Vec<MemoryClaimV3>,
    pub unresolved_goals: Vec<MemoryGoalV3>,
    pub recent_turns: Vec<RecentTurnV3>,
}

impl fmt::Debug for SessionMemoryViewV3 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SessionMemoryViewV3")
            .field("schema_version", &self.schema_version)
            .field("authority", &self.authority)
            .field("session_id_hash", &self.session_id_hash)
            .field("source_frontier_hash", &self.source_frontier_hash)
            .field("source_revision", &self.source_revision)
            .field("view_hash", &self.view_hash)
            .field("ticker_count", &self.tickers.len())
            .field("constraint_count", &self.constraints.len())
            .field("source_count", &self.sources.len())
            .field("claim_count", &self.claims.len())
            .field("goal_count", &self.unresolved_goals.len())
            .field("turn_count", &self.recent_turns.len())
            .finish()
    }
}

impl SessionMemoryViewV3 {
    pub fn validate(&self, max_bytes: usize) -> Result<(), MemoryError> {
        if self.schema_version != SESSION_MEMORY_SCHEMA_VERSION
            || self.authority != MemoryAuthority::ContextOnly
            || self.claims.len() > MAX_VIEW_CLAIMS
            || self.unresolved_goals.len() > MAX_VIEW_GOALS
            || self.recent_turns.len() > MAX_VIEW_TURNS
            || self.constraints.len() > MAX_CONSTRAINTS
            || self.source_revision == 0
        {
            return Err(MemoryError::InvalidEnvelope);
        }
        validate_tickers(&self.tickers)?;
        for (run_id, source) in &self.sources {
            validate_source(source)?;
            if run_id != &source.run_id || source.revision > self.source_revision {
                return Err(MemoryError::InvalidLineage);
            }
        }
        for constraint in &self.constraints {
            validate_constraint(constraint)?;
        }
        for claim in &self.claims {
            validate_memory_claim(claim)?;
            if !self.sources.contains_key(&claim.source_run_id) {
                return Err(MemoryError::InvalidLineage);
            }
        }
        for goal in &self.unresolved_goals {
            validate_goal(goal)?;
            if !self.sources.contains_key(&goal.source_run_id) {
                return Err(MemoryError::InvalidLineage);
            }
        }
        for turn in &self.recent_turns {
            validate_recent_turn(turn)?;
            if !self.sources.contains_key(&turn.source_run_id) {
                return Err(MemoryError::InvalidLineage);
            }
        }
        ensure_canonical_size(self, max_bytes, "view bytes")?;
        if self.compute_view_hash()? != self.view_hash {
            return Err(MemoryError::ViewHashMismatch);
        }
        Ok(())
    }

    pub fn canonical_context(&self) -> Result<Vec<u8>, MemoryError> {
        self.validate(MAX_SESSION_MEMORY_VIEW_BYTES)?;
        Ok(serde_jcs::to_vec(self)?)
    }

    fn compute_view_hash(&self) -> Result<ContentHash, MemoryError> {
        #[derive(Serialize)]
        struct ViewHashInput<'a> {
            schema_version: u16,
            authority: MemoryAuthority,
            session_id_hash: &'a ContentHash,
            source_frontier_hash: &'a ContentHash,
            source_revision: u64,
            tickers: &'a [String],
            constraints: &'a [UserConstraintV3],
            sources: &'a BTreeMap<String, MemorySourceV3>,
            claims: &'a [MemoryClaimV3],
            unresolved_goals: &'a [MemoryGoalV3],
            recent_turns: &'a [RecentTurnV3],
        }
        let input = ViewHashInput {
            schema_version: self.schema_version,
            authority: self.authority,
            session_id_hash: &self.session_id_hash,
            source_frontier_hash: &self.source_frontier_hash,
            source_revision: self.source_revision,
            tickers: &self.tickers,
            constraints: &self.constraints,
            sources: &self.sources,
            claims: &self.claims,
            unresolved_goals: &self.unresolved_goals,
            recent_turns: &self.recent_turns,
        };
        Ok(ContentHash::sha256(serde_jcs::to_vec(&input)?))
    }

    pub fn zeroize_sensitive(&mut self) {
        for constraint in &mut self.constraints {
            constraint.text.zeroize();
            constraint.source_message_id.zeroize();
        }
        for source in self.sources.values_mut() {
            source.run_id.zeroize();
            source.user_message_id.zeroize();
            source.assistant_message_id.zeroize();
        }
        for claim in &mut self.claims {
            claim.source_run_id.zeroize();
            claim.claim.text.zeroize();
            scrub_optional(&mut claim.claim.subject);
            scrub_optional(&mut claim.claim.predicate);
            scrub_json_option(&mut claim.claim.value);
            scrub_optional(&mut claim.claim.unit);
            scrub_optional(&mut claim.claim.period);
            scrub_optional(&mut claim.claim.comparison_basis);
        }
        for goal in &mut self.unresolved_goals {
            goal.source_run_id.zeroize();
            goal.text.zeroize();
        }
        for turn in &mut self.recent_turns {
            turn.source_run_id.zeroize();
            turn.user_content.zeroize();
            turn.answer_content.zeroize();
        }
    }
}

impl Drop for SessionMemoryViewV3 {
    fn drop(&mut self) {
        self.zeroize_sensitive();
    }
}

pub struct CompletedTurnInputV3<'a> {
    pub session_id: &'a str,
    pub parent_frontier_hash: ContentHash,
    pub revision: u64,
    pub run_id: &'a str,
    pub final_commit_intent_hash: ContentHash,
    pub answer_bundle_hash: ContentHash,
    pub user_content: &'a str,
    pub rendered_answer: &'a str,
    pub answer_ir: &'a AnswerIr,
    pub tickers: &'a [String],
    pub constraints: &'a [UserConstraintV3],
    pub supersessions: &'a [MemorySupersessionV3],
    pub resolved_goals: &'a [MemoryGoalResolutionV3],
}

/// A completed direct-Markdown turn. Unlike [`CompletedTurnInputV3`], this
/// does not pretend that free prose contains kernel-validated claims. It
/// preserves only bounded conversational continuity (question, answer,
/// ticker scope, and user constraints); grounded claims remain in the
/// durable per-run EvidenceLedger.
pub struct CompletedMarkdownTurnInputV3<'a> {
    pub session_id: &'a str,
    pub parent_frontier_hash: ContentHash,
    pub revision: u64,
    pub run_id: &'a str,
    pub final_commit_intent_hash: ContentHash,
    pub answer_bundle_hash: ContentHash,
    pub final_output_hash: ContentHash,
    pub user_content: &'a str,
    pub rendered_answer: &'a str,
    pub tickers: &'a [String],
    pub constraints: &'a [UserConstraintV3],
    pub supersessions: &'a [MemorySupersessionV3],
    pub resolved_goals: &'a [MemoryGoalResolutionV3],
}

impl fmt::Debug for CompletedMarkdownTurnInputV3<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CompletedMarkdownTurnInputV3")
            .field("session_id_hash", &ContentHash::sha256(self.session_id))
            .field("revision", &self.revision)
            .field("run_id_hash", &ContentHash::sha256(self.run_id))
            .field("user_content", &"[REDACTED]")
            .field("rendered_answer", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for CompletedTurnInputV3<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CompletedTurnInputV3")
            .field("session_id_hash", &ContentHash::sha256(self.session_id))
            .field("revision", &self.revision)
            .field("run_id_hash", &ContentHash::sha256(self.run_id))
            .field("user_content", &"[REDACTED]")
            .field("rendered_answer", &"[REDACTED]")
            .field("answer_claim_count", &self.answer_ir.claims.len())
            .finish_non_exhaustive()
    }
}

pub fn completed_turn_delta(
    input: CompletedTurnInputV3<'_>,
) -> Result<SessionMemoryDeltaV3, MemoryError> {
    if !bounded_id(input.session_id) || !bounded_id(input.run_id) || input.revision == 0 {
        return Err(MemoryError::InvalidEnvelope);
    }
    let answer_ir_bytes = serde_jcs::to_vec(input.answer_ir)?;
    let final_output_hash = ContentHash::sha256(answer_ir_bytes);
    let source = MemorySourceV3 {
        run_id: input.run_id.to_owned(),
        revision: input.revision,
        user_message_id: deterministic_message_id(input.run_id, "user"),
        assistant_message_id: deterministic_message_id(input.run_id, "assistant"),
        final_commit_intent_hash: input.final_commit_intent_hash,
        answer_bundle_hash: input.answer_bundle_hash,
        final_output_hash: final_output_hash.clone(),
    };
    let (user_content, user_content_original_bytes, user_content_truncated) =
        utf8_excerpt(input.user_content, MAX_RECENT_TURN_EXCERPT_BYTES)?;
    let (answer_content, answer_content_original_bytes, answer_content_truncated) =
        utf8_excerpt(input.rendered_answer, MAX_RECENT_TURN_EXCERPT_BYTES)?;
    let mut projection_bytes = 0_usize;
    let mut claims = Vec::new();
    for claim in input.answer_ir.claims.iter().take(MAX_CLAIMS_PER_DELTA) {
        let projected = MemoryClaimV3 {
            memory_id: memory_claim_id(input.run_id, &final_output_hash, &claim.claim_id)?,
            source_run_id: input.run_id.to_owned(),
            claim: claim.clone(),
            superseded_by: None,
        };
        if validate_memory_claim(&projected).is_ok() {
            let cost = canonical_item_cost(&projected)?;
            if projection_bytes.saturating_add(cost) <= MAX_COMPLETED_TURN_PROJECTION_BYTES {
                claims.push(projected);
                projection_bytes += cost;
            }
        }
    }

    let mut goals = BTreeMap::new();
    for (index, text) in input
        .answer_ir
        .follow_up_questions
        .iter()
        .take(MAX_GOALS_PER_DELTA)
        .enumerate()
    {
        if let Ok(goal) = memory_goal(
            input.run_id,
            &final_output_hash,
            None,
            index,
            text,
            Vec::new(),
        ) {
            let cost = canonical_item_cost(&goal)?;
            if projection_bytes.saturating_add(cost) <= MAX_COMPLETED_TURN_PROJECTION_BYTES {
                projection_bytes += cost;
                goals.insert(goal.memory_id.clone(), goal);
            }
        }
    }
    for (index, claim) in input
        .answer_ir
        .claims
        .iter()
        .filter(|claim| claim.kind == ClaimKind::Uncertainty)
        .take(MAX_GOALS_PER_DELTA.saturating_sub(goals.len()))
        .enumerate()
    {
        let Ok(goal) = memory_goal(
            input.run_id,
            &final_output_hash,
            Some(claim.claim_id.as_str()),
            index,
            &claim.text,
            claim.evidence_ids.clone(),
        ) else {
            continue;
        };
        let cost = canonical_item_cost(&goal)?;
        if projection_bytes.saturating_add(cost) <= MAX_COMPLETED_TURN_PROJECTION_BYTES {
            projection_bytes += cost;
            goals.entry(goal.memory_id.clone()).or_insert(goal);
        }
    }
    let tickers = input
        .tickers
        .iter()
        .filter(|ticker| is_canonical_ticker(ticker))
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .take(MAX_TICKERS)
        .collect::<Vec<_>>();
    let mut constraint_ids = BTreeSet::new();
    let constraints = input
        .constraints
        .iter()
        .filter(|constraint| {
            validate_constraint(constraint).is_ok()
                && constraint_ids.insert(constraint.constraint_id.clone())
        })
        .take(MAX_CONSTRAINTS)
        .cloned()
        .collect::<Vec<_>>();
    let mut supersession_ids = BTreeSet::new();
    let supersessions = input
        .supersessions
        .iter()
        .filter(|value| {
            bounded_id(&value.older_memory_id)
                && bounded_id(&value.newer_memory_id)
                && value.older_memory_id != value.newer_memory_id
                && supersession_ids.insert(value.older_memory_id.clone())
        })
        .take(MAX_CLAIMS_PER_DELTA)
        .cloned()
        .collect::<Vec<_>>();
    let mut resolution_ids = BTreeSet::new();
    let resolved_goals = input
        .resolved_goals
        .iter()
        .filter(|value| {
            bounded_id(&value.goal_memory_id)
                && bounded_id(&value.resolving_memory_id)
                && resolution_ids.insert(value.goal_memory_id.clone())
        })
        .take(MAX_GOALS_PER_DELTA)
        .cloned()
        .collect::<Vec<_>>();
    let delta = SessionMemoryDeltaV3 {
        schema_version: SESSION_MEMORY_SCHEMA_VERSION,
        authority: MemoryAuthority::ContextOnly,
        session_id_hash: ContentHash::sha256(input.session_id),
        parent_frontier_hash: input.parent_frontier_hash,
        revision: input.revision,
        source,
        tickers,
        constraints,
        claims,
        unresolved_goals: goals.into_values().collect(),
        supersessions,
        resolved_goals,
        recent_turn: RecentTurnV3 {
            source_run_id: input.run_id.to_owned(),
            user_content_hash: ContentHash::sha256(input.user_content),
            user_content,
            user_content_original_bytes,
            user_content_truncated,
            answer_content_hash: ContentHash::sha256(input.rendered_answer),
            answer_content,
            answer_content_original_bytes,
            answer_content_truncated,
        },
    };
    delta.validate()?;
    Ok(delta)
}

/// Store a direct Markdown response without manufacturing a claim graph from
/// prose. This keeps multi-turn context available while preventing later
/// turns from treating unparsed Markdown as independently verified evidence.
pub fn completed_markdown_turn_delta(
    input: CompletedMarkdownTurnInputV3<'_>,
) -> Result<SessionMemoryDeltaV3, MemoryError> {
    if !bounded_id(input.session_id) || !bounded_id(input.run_id) || input.revision == 0 {
        return Err(MemoryError::InvalidEnvelope);
    }
    let source = MemorySourceV3 {
        run_id: input.run_id.to_owned(),
        revision: input.revision,
        user_message_id: deterministic_message_id(input.run_id, "user"),
        assistant_message_id: deterministic_message_id(input.run_id, "assistant"),
        final_commit_intent_hash: input.final_commit_intent_hash,
        answer_bundle_hash: input.answer_bundle_hash,
        // `MemorySourceV3` predates direct Markdown and retains this field
        // name in its durable schema. Its value is nevertheless the exact
        // canonical final-output hash; no synthetic AnswerIR is created.
        final_output_hash: input.final_output_hash,
    };
    let (user_content, user_content_original_bytes, user_content_truncated) =
        utf8_excerpt(input.user_content, MAX_RECENT_TURN_EXCERPT_BYTES)?;
    let (answer_content, answer_content_original_bytes, answer_content_truncated) =
        utf8_excerpt(input.rendered_answer, MAX_RECENT_TURN_EXCERPT_BYTES)?;
    let tickers = input
        .tickers
        .iter()
        .filter(|ticker| is_canonical_ticker(ticker))
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .take(MAX_TICKERS)
        .collect::<Vec<_>>();
    let mut constraint_ids = BTreeSet::new();
    let constraints = input
        .constraints
        .iter()
        .filter(|constraint| {
            validate_constraint(constraint).is_ok()
                && constraint_ids.insert(constraint.constraint_id.clone())
        })
        .take(MAX_CONSTRAINTS)
        .cloned()
        .collect::<Vec<_>>();
    let mut supersession_ids = BTreeSet::new();
    let supersessions = input
        .supersessions
        .iter()
        .filter(|value| {
            bounded_id(&value.older_memory_id)
                && bounded_id(&value.newer_memory_id)
                && value.older_memory_id != value.newer_memory_id
                && supersession_ids.insert(value.older_memory_id.clone())
        })
        .take(MAX_CLAIMS_PER_DELTA)
        .cloned()
        .collect::<Vec<_>>();
    let mut resolution_ids = BTreeSet::new();
    let resolved_goals = input
        .resolved_goals
        .iter()
        .filter(|value| {
            bounded_id(&value.goal_memory_id)
                && bounded_id(&value.resolving_memory_id)
                && resolution_ids.insert(value.goal_memory_id.clone())
        })
        .take(MAX_GOALS_PER_DELTA)
        .cloned()
        .collect::<Vec<_>>();
    let delta = SessionMemoryDeltaV3 {
        schema_version: SESSION_MEMORY_SCHEMA_VERSION,
        authority: MemoryAuthority::ContextOnly,
        session_id_hash: ContentHash::sha256(input.session_id),
        parent_frontier_hash: input.parent_frontier_hash,
        revision: input.revision,
        source,
        tickers,
        constraints,
        claims: Vec::new(),
        unresolved_goals: Vec::new(),
        supersessions,
        resolved_goals,
        recent_turn: RecentTurnV3 {
            source_run_id: input.run_id.to_owned(),
            user_content_hash: ContentHash::sha256(input.user_content),
            user_content,
            user_content_original_bytes,
            user_content_truncated,
            answer_content_hash: ContentHash::sha256(input.rendered_answer),
            answer_content,
            answer_content_original_bytes,
            answer_content_truncated,
        },
    };
    delta.validate()?;
    Ok(delta)
}

fn memory_claim_id(
    run_id: &str,
    final_output_hash: &ContentHash,
    claim_id: &str,
) -> Result<String, MemoryError> {
    #[derive(Serialize)]
    struct Identity<'a> {
        kind: &'static str,
        run_id: &'a str,
        final_output_hash: &'a ContentHash,
        claim_id: &'a str,
    }
    Ok(ContentHash::sha256(serde_jcs::to_vec(&Identity {
        kind: "claim",
        run_id,
        final_output_hash,
        claim_id,
    })?)
    .to_string())
}

pub fn deterministic_message_id(run_id: &str, role: &str) -> String {
    ContentHash::sha256(format!("memory-message/v2\0{run_id}\0{role}")).to_string()
}

fn apply_supersession(
    claims: &mut BTreeMap<String, MemoryClaimV3>,
    active_claim_fingerprints: &mut BTreeMap<String, ContentHash>,
    supersession: &MemorySupersessionV3,
) -> Result<(), MemoryError> {
    if supersession.older_memory_id == supersession.newer_memory_id {
        return Err(MemoryError::SupersessionCycle);
    }
    let older_fingerprint = active_claim_fingerprints
        .get(&supersession.older_memory_id)
        .ok_or(MemoryError::UnknownMemory)?;
    let newer_fingerprint = active_claim_fingerprints
        .get(&supersession.newer_memory_id)
        .ok_or(MemoryError::UnknownMemory)?;
    if older_fingerprint != newer_fingerprint {
        return Err(MemoryError::InvalidSupersession);
    }
    active_claim_fingerprints.remove(&supersession.older_memory_id);
    if let Some(older) = claims.get_mut(&supersession.older_memory_id) {
        if older.superseded_by.is_some() {
            return Err(MemoryError::InvalidSupersession);
        }
        older.superseded_by = Some(supersession.newer_memory_id.clone());
    }
    Ok(())
}

fn claim_fingerprint(claim: &MemoryClaimV3) -> Result<ContentHash, MemoryError> {
    #[derive(Serialize)]
    struct Fingerprint<'a> {
        subject: &'a Option<String>,
        predicate: &'a Option<String>,
    }
    Ok(ContentHash::sha256(serde_jcs::to_vec(&Fingerprint {
        subject: &claim.claim.subject,
        predicate: &claim.claim.predicate,
    })?))
}

fn constraint_fingerprint(constraint: &UserConstraintV3) -> Result<ContentHash, MemoryError> {
    Ok(ContentHash::sha256(serde_jcs::to_vec(constraint)?))
}

fn canonical_item_cost(value: &impl Serialize) -> Result<usize, MemoryError> {
    Ok(serde_jcs::to_vec(value)?.len().saturating_add(64))
}

fn memory_goal(
    run_id: &str,
    final_output_hash: &ContentHash,
    source_claim_id: Option<&str>,
    ordinal: usize,
    text: &str,
    evidence_ids: Vec<String>,
) -> Result<MemoryGoalV3, MemoryError> {
    #[derive(Serialize)]
    struct Identity<'a> {
        kind: &'static str,
        run_id: &'a str,
        final_output_hash: &'a ContentHash,
        source_claim_id: Option<&'a str>,
        ordinal: usize,
        text_hash: ContentHash,
    }
    if !bounded_text(text, MAX_TEXT_BYTES) || evidence_ids.len() > MAX_EVIDENCE_REFS {
        return Err(MemoryError::Limit("goal"));
    }
    let memory_id = ContentHash::sha256(serde_jcs::to_vec(&Identity {
        kind: "goal",
        run_id,
        final_output_hash,
        source_claim_id,
        ordinal,
        text_hash: ContentHash::sha256(text),
    })?)
    .to_string();
    Ok(MemoryGoalV3 {
        memory_id,
        source_run_id: run_id.to_owned(),
        source_claim_id: source_claim_id.map(str::to_owned),
        text: text.to_owned(),
        evidence_ids,
        resolved_by: None,
    })
}

fn validate_source(source: &MemorySourceV3) -> Result<(), MemoryError> {
    if !bounded_id(&source.run_id)
        || !(1..=MAX_SESSION_MEMORY_REVISION).contains(&source.revision)
        || !bounded_id(&source.user_message_id)
        || !bounded_id(&source.assistant_message_id)
        || source.user_message_id != deterministic_message_id(&source.run_id, "user")
        || source.assistant_message_id != deterministic_message_id(&source.run_id, "assistant")
    {
        return Err(MemoryError::InvalidId("memory source"));
    }
    Ok(())
}

fn validate_constraint(constraint: &UserConstraintV3) -> Result<(), MemoryError> {
    if !bounded_id(&constraint.constraint_id)
        || !bounded_text(&constraint.text, MAX_TEXT_BYTES)
        || !bounded_id(&constraint.source_message_id)
        || ContentHash::sha256(&constraint.text) != constraint.source_message_hash
    {
        return Err(MemoryError::InvalidConstraint);
    }
    Ok(())
}

fn validate_memory_claim(claim: &MemoryClaimV3) -> Result<(), MemoryError> {
    if !bounded_id(&claim.memory_id)
        || !bounded_id(&claim.source_run_id)
        || !bounded_id(&claim.claim.claim_id)
        || !bounded_text(&claim.claim.text, MAX_TEXT_BYTES)
        || claim.claim.evidence_ids.len() > MAX_EVIDENCE_REFS
        || claim.claim.counter_evidence_ids.len() > MAX_EVIDENCE_REFS
        || claim.claim.calculation_ids.len() > MAX_EVIDENCE_REFS
        || claim.claim.goal_ids.len() > MAX_EVIDENCE_REFS
        || claim
            .superseded_by
            .as_deref()
            .is_some_and(|id| !bounded_id(id))
    {
        return Err(MemoryError::InvalidClaim);
    }
    ensure_canonical_size(claim, MAX_SESSION_MEMORY_VIEW_BYTES, "memory claim")
}

fn validate_goal(goal: &MemoryGoalV3) -> Result<(), MemoryError> {
    if !bounded_id(&goal.memory_id)
        || !bounded_id(&goal.source_run_id)
        || goal
            .source_claim_id
            .as_deref()
            .is_some_and(|id| !bounded_id(id))
        || !bounded_text(&goal.text, MAX_TEXT_BYTES)
        || goal.evidence_ids.len() > MAX_EVIDENCE_REFS
        || goal
            .resolved_by
            .as_deref()
            .is_some_and(|id| !bounded_id(id))
    {
        return Err(MemoryError::InvalidGoal);
    }
    Ok(())
}

fn validate_recent_turn(turn: &RecentTurnV3) -> Result<(), MemoryError> {
    let user_bytes = u64::try_from(turn.user_content.len()).map_err(|_| MemoryError::Overflow)?;
    let answer_bytes =
        u64::try_from(turn.answer_content.len()).map_err(|_| MemoryError::Overflow)?;
    if !bounded_id(&turn.source_run_id)
        || !bounded_text(&turn.user_content, MAX_TEXT_BYTES)
        || !bounded_text(&turn.answer_content, MAX_TEXT_BYTES)
        || turn.user_content_original_bytes < user_bytes
        || turn.answer_content_original_bytes < answer_bytes
        || turn.user_content_truncated != (turn.user_content_original_bytes > user_bytes)
        || turn.answer_content_truncated != (turn.answer_content_original_bytes > answer_bytes)
        || (!turn.user_content_truncated
            && ContentHash::sha256(&turn.user_content) != turn.user_content_hash)
        || (!turn.answer_content_truncated
            && ContentHash::sha256(&turn.answer_content) != turn.answer_content_hash)
    {
        return Err(MemoryError::InvalidRecentTurn);
    }
    Ok(())
}

fn utf8_excerpt(value: &str, max_bytes: usize) -> Result<(String, u64, bool), MemoryError> {
    let original_bytes = u64::try_from(value.len()).map_err(|_| MemoryError::Overflow)?;
    if value.trim().is_empty() {
        return Err(MemoryError::InvalidRecentTurn);
    }
    if value.len() <= max_bytes {
        return Ok((value.to_owned(), original_bytes, false));
    }
    let mut boundary = max_bytes.min(value.len());
    while boundary > 0 && !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    if boundary == 0 {
        return Err(MemoryError::InvalidRecentTurn);
    }
    Ok((value[..boundary].to_owned(), original_bytes, true))
}

fn validate_tickers(tickers: &[String]) -> Result<(), MemoryError> {
    if tickers.len() > MAX_TICKERS
        || !tickers.iter().all(|ticker| is_canonical_ticker(ticker))
        || tickers.iter().collect::<BTreeSet<_>>().len() != tickers.len()
    {
        return Err(MemoryError::InvalidTickerSet);
    }
    Ok(())
}

fn bounded_id(value: &str) -> bool {
    let bytes = value.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= MAX_SOURCE_ID_BYTES
        && !value.contains('\0')
        && bytes.iter().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b':' | b'/' | b'-')
        })
}

fn bounded_text(value: &str, max_bytes: usize) -> bool {
    !value.trim().is_empty() && value.len() <= max_bytes && !value.contains('\0')
}

fn ensure_canonical_size(
    value: &impl Serialize,
    max_bytes: usize,
    limit: &'static str,
) -> Result<(), MemoryError> {
    if serde_jcs::to_vec(value)?.len() > max_bytes {
        return Err(MemoryError::Limit(limit));
    }
    Ok(())
}

pub fn empty_frontier_hash() -> ContentHash {
    ContentHash::sha256("krw.session-memory/empty-v2")
}

pub fn next_frontier_hash(
    parent_hash: &ContentHash,
    revision: u64,
    delta_hash: &ContentHash,
) -> ContentHash {
    ContentHash::sha256(format!(
        "krw.session-memory/frontier-v2\0{}\0{}\0{}",
        parent_hash.as_str(),
        revision,
        delta_hash.as_str()
    ))
}

pub fn empty_source_lineage_hash() -> ContentHash {
    ContentHash::sha256("krw.session-memory/source-lineage-empty-v2")
}

pub fn next_source_lineage_hash(
    parent_hash: &ContentHash,
    revision: u64,
    source_run_hash: &ContentHash,
) -> ContentHash {
    ContentHash::sha256(format!(
        "krw.session-memory/source-lineage-v2\0{}\0{}\0{}",
        parent_hash.as_str(),
        revision,
        source_run_hash.as_str()
    ))
}

fn validate_recent_source_index(
    index: &BTreeMap<String, u64>,
    revision: u64,
) -> Result<(), MemoryError> {
    if !(1..=MAX_SESSION_MEMORY_REVISION).contains(&revision) {
        return Err(MemoryError::InvalidLineage);
    }
    let expected_len = usize::try_from(revision.min(MAX_RECENT_SOURCE_INDEX as u64))
        .map_err(|_| MemoryError::Overflow)?;
    if index.len() != expected_len
        || index
            .keys()
            .any(|hash| ContentHash::parse(hash.clone()).is_err())
    {
        return Err(MemoryError::InvalidLineage);
    }
    let first_revision = revision
        .checked_sub(expected_len as u64)
        .and_then(|value| value.checked_add(1))
        .ok_or(MemoryError::Overflow)?;
    let observed = index.values().copied().collect::<BTreeSet<_>>();
    let expected = (first_revision..=revision).collect::<BTreeSet<_>>();
    if observed != expected {
        return Err(MemoryError::InvalidLineage);
    }
    Ok(())
}

fn prune_recent_source_index(index: &mut BTreeMap<String, u64>) {
    while index.len() > MAX_RECENT_SOURCE_INDEX {
        let Some(oldest) = index
            .iter()
            .min_by_key(|(hash, revision)| (**revision, hash.as_str()))
            .map(|(hash, _)| hash.clone())
        else {
            break;
        };
        index.remove(&oldest);
    }
}

fn claim_search_text(claim: &MemoryClaimV3) -> String {
    let value = claim
        .claim
        .value
        .as_ref()
        .and_then(|value| serde_jcs::to_string(value).ok())
        .unwrap_or_default();
    format!(
        "{} {} {} {} {} {}",
        claim.claim.text,
        claim.claim.subject.as_deref().unwrap_or_default(),
        claim.claim.predicate.as_deref().unwrap_or_default(),
        value,
        claim.claim.period.as_deref().unwrap_or_default(),
        claim.claim.unit.as_deref().unwrap_or_default(),
    )
}

fn terms(text: &str) -> BTreeSet<String> {
    let lowercase = text.to_lowercase();
    let mut output = lowercase
        .split(|character: char| !character.is_alphanumeric())
        .filter(|term| !term.is_empty())
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    let compact = lowercase
        .chars()
        .filter(|character| character.is_alphanumeric())
        .collect::<Vec<_>>();
    for width in [2_usize, 3] {
        for window in compact.windows(width) {
            output.insert(window.iter().collect());
        }
    }
    output
}

fn score(query: &BTreeSet<String>, candidate: &BTreeSet<String>, recency: usize) -> usize {
    let lexical = query.intersection(candidate).count();
    lexical
        .saturating_mul(32)
        .saturating_add(recency.min(4_096))
        .saturating_add(1)
}

fn shrink_view_to_fit(view: &mut SessionMemoryViewV3, max_bytes: usize) -> Result<(), MemoryError> {
    loop {
        view.view_hash = empty_frontier_hash();
        if serde_jcs::to_vec(view)?.len() <= max_bytes {
            return Ok(());
        }
        if !view.recent_turns.is_empty() {
            view.recent_turns.pop();
        } else if !view.unresolved_goals.is_empty() {
            view.unresolved_goals.pop();
        } else if !view.claims.is_empty() {
            view.claims.pop();
        } else if !view.constraints.is_empty() {
            view.constraints.pop();
        } else {
            return Err(MemoryError::Limit("view bytes"));
        }
        let retained = view
            .claims
            .iter()
            .map(|claim| claim.source_run_id.as_str())
            .chain(
                view.unresolved_goals
                    .iter()
                    .map(|goal| goal.source_run_id.as_str()),
            )
            .chain(
                view.recent_turns
                    .iter()
                    .map(|turn| turn.source_run_id.as_str()),
            )
            .collect::<BTreeSet<_>>();
        view.sources
            .retain(|run_id, _| retained.contains(run_id.as_str()));
    }
}

fn scrub_optional(value: &mut Option<String>) {
    if let Some(value) = value {
        value.zeroize();
    }
}

fn scrub_json_option(value: &mut Option<Value>) {
    if let Some(value) = value {
        scrub_json(value);
    }
}

fn scrub_json(value: &mut Value) {
    match value {
        Value::String(text) => text.zeroize(),
        Value::Array(values) => values.iter_mut().for_each(scrub_json),
        Value::Object(values) => values.values_mut().for_each(scrub_json),
        _ => {}
    }
    *value = Value::Null;
}

#[derive(Debug, Error)]
pub enum MemoryError {
    #[error("session memory envelope is invalid")]
    InvalidEnvelope,
    #[error("session memory identifier is invalid: {0}")]
    InvalidId(&'static str),
    #[error("session memory ticker set is invalid")]
    InvalidTickerSet,
    #[error("session memory constraint is invalid")]
    InvalidConstraint,
    #[error("session memory claim is invalid")]
    InvalidClaim,
    #[error("session memory goal is invalid")]
    InvalidGoal,
    #[error("session recent turn is invalid")]
    InvalidRecentTurn,
    #[error("session memory lineage is invalid")]
    InvalidLineage,
    #[error("session memory frontier does not match")]
    FrontierMismatch,
    #[error("session memory view hash does not match")]
    ViewHashMismatch,
    #[error("session memory identifier is duplicated")]
    DuplicateId,
    #[error("session memory identifier conflicts with stored data")]
    ConflictingId,
    #[error("session memory record is unknown")]
    UnknownMemory,
    #[error("session memory supersession is invalid")]
    InvalidSupersession,
    #[error("session memory supersession forms a cycle")]
    SupersessionCycle,
    #[error("session memory counter overflow")]
    Overflow,
    #[error("session memory limit exceeded: {0}")]
    Limit(&'static str),
    #[error("session memory canonical serialization failed")]
    Canonical(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use krw_agent_evidence::{AnswerSection, ClaimStrength};

    use super::*;

    const SESSION_ID: &str = "session:tenant-a:conversation-1";
    const RUN_ID: &str = "run:worker-a:0002";

    fn hash(label: &str) -> ContentHash {
        ContentHash::sha256(label)
    }

    fn answer_ir() -> AnswerIr {
        AnswerIr {
            schema_version: 1,
            locale: "ko-KR".into(),
            sections: vec![AnswerSection {
                section_id: "summary".into(),
                heading: "요약".into(),
                intent: "answer".into(),
                claim_ids: vec!["revenue".into(), "cash-risk".into()],
                disclosed_uncertainty: Some("현금흐름 기간은 추가 확인이 필요합니다.".into()),
            }],
            claims: vec![
                Claim {
                    claim_id: "revenue".into(),
                    kind: ClaimKind::Number,
                    strength: ClaimStrength::Strong,
                    text: "2026년 매출은 감소하지 않고 11% 증가했습니다.".into(),
                    goal_ids: vec!["growth".into()],
                    evidence_ids: vec!["ev-revenue".into()],
                    counter_evidence_ids: Vec::new(),
                    calculation_ids: vec!["calc-growth".into()],
                    subject: Some("AAPL".into()),
                    predicate: Some("revenue_growth".into()),
                    value: Some(serde_json::json!(11)),
                    unit: Some("percent".into()),
                    period: Some("FY2026".into()),
                    comparison_basis: Some("FY2025".into()),
                },
                Claim {
                    claim_id: "cash-risk".into(),
                    kind: ClaimKind::Uncertainty,
                    strength: ClaimStrength::Qualified,
                    text: "최신 현금흐름 기간은 추가 확인이 필요합니다.".into(),
                    goal_ids: vec!["cash".into()],
                    evidence_ids: Vec::new(),
                    counter_evidence_ids: Vec::new(),
                    calculation_ids: Vec::new(),
                    subject: Some("AAPL".into()),
                    predicate: Some("cash_flow".into()),
                    value: None,
                    unit: None,
                    period: None,
                    comparison_basis: None,
                },
            ],
            calculations: Vec::new(),
            follow_up_questions: vec!["최신 현금흐름은 얼마인가?".into()],
        }
    }

    fn first_delta(catalog: &SessionMemoryCatalogV3) -> SessionMemoryDeltaV3 {
        completed_turn_delta(CompletedTurnInputV3 {
            session_id: SESSION_ID,
            parent_frontier_hash: catalog.frontier_hash.clone(),
            revision: 1,
            run_id: RUN_ID,
            final_commit_intent_hash: hash("final-intent"),
            answer_bundle_hash: hash("bundle"),
            user_content: "AAPL의 매출 성장과 현금흐름을 확인해줘.",
            rendered_answer: "매출은 11% 증가했고 현금흐름은 추가 확인이 필요합니다.",
            answer_ir: &answer_ir(),
            tickers: &["AAPL".into()],
            constraints: &[],
            supersessions: &[],
            resolved_goals: &[],
        })
        .unwrap()
    }

    #[test]
    fn completed_turn_preserves_number_period_and_negation_exactly() {
        let catalog = SessionMemoryCatalogV3::new(SESSION_ID).unwrap();
        let delta = first_delta(&catalog);
        let claim = &delta.claims[0].claim;
        assert_eq!(claim.value, Some(serde_json::json!(11)));
        assert_eq!(claim.period.as_deref(), Some("FY2026"));
        assert!(claim.text.contains("감소하지 않고"));
        assert_eq!(delta.source.final_output_hash, hash_answer(&answer_ir()));
        assert_ne!(delta.content_hash().unwrap(), empty_frontier_hash());
    }

    #[test]
    fn direct_markdown_turn_preserves_continuity_without_claim_authority() {
        let catalog = SessionMemoryCatalogV3::new(SESSION_ID).unwrap();
        let final_output_hash = ContentHash::sha256("## 결론\n\nAAPL 매출은 증가했습니다.");
        let delta = completed_markdown_turn_delta(CompletedMarkdownTurnInputV3 {
            session_id: SESSION_ID,
            parent_frontier_hash: catalog.frontier_hash.clone(),
            revision: 1,
            run_id: RUN_ID,
            final_commit_intent_hash: hash("markdown-final-intent"),
            answer_bundle_hash: hash("markdown-bundle"),
            final_output_hash: final_output_hash.clone(),
            user_content: "AAPL의 매출 추이를 알려줘.",
            rendered_answer: "## 결론\n\nAAPL 매출은 증가했습니다.",
            tickers: &["AAPL".into()],
            constraints: &[],
            supersessions: &[],
            resolved_goals: &[],
        })
        .unwrap();

        assert_eq!(delta.source.final_output_hash, final_output_hash);
        assert!(delta.claims.is_empty());
        assert!(delta.unresolved_goals.is_empty());
        assert_eq!(delta.recent_turn.source_run_id, RUN_ID);
        assert!(delta.recent_turn.answer_content.contains("AAPL"));
        delta.validate().unwrap();

        let mut catalog = catalog;
        catalog.apply_delta(&delta).unwrap();
        assert!(catalog.claims().is_empty());
        assert_eq!(catalog.recent_turns().len(), 1);
    }

    #[test]
    fn catalog_frontier_is_compare_and_swap_and_replay_safe() {
        let mut catalog = SessionMemoryCatalogV3::new(SESSION_ID).unwrap();
        let delta = first_delta(&catalog);
        let delta_hash = delta.content_hash().unwrap();
        let expected_frontier =
            next_frontier_hash(catalog.frontier_hash(), delta.revision, &delta_hash);
        catalog.apply_delta(&delta).unwrap();
        assert_eq!(catalog.revision(), 1);
        assert_eq!(catalog.frontier_hash(), &expected_frontier);
        assert_eq!(catalog.claims().len(), 2);
        assert_eq!(catalog.recent_turns().len(), 1);
        assert!(matches!(
            catalog.apply_delta(&delta),
            Err(MemoryError::FrontierMismatch)
        ));
        catalog.validate().unwrap();
    }

    #[test]
    fn frontier_v2_matches_cross_language_golden_vector() {
        let parent = ContentHash::parse(format!("sha256:{}", "1".repeat(64))).unwrap();
        let delta = ContentHash::parse(format!("sha256:{}", "2".repeat(64))).unwrap();
        assert_eq!(
            next_frontier_hash(&parent, 7, &delta).as_str(),
            "sha256:f2655508c5e2c43af0292f6e230fa57188befd420b2dc24c204d0b3a36744b42"
        );
    }

    #[test]
    fn source_lineage_v2_matches_cross_language_golden_vector() {
        let parent = ContentHash::parse(format!("sha256:{}", "1".repeat(64))).unwrap();
        let source = ContentHash::parse(format!("sha256:{}", "2".repeat(64))).unwrap();
        assert_eq!(
            empty_source_lineage_hash().as_str(),
            "sha256:e0d62521a0b7c1b8d1f3ba5e090c6a7bd60261c57adae0f7ba496d4b06216d39"
        );
        assert_eq!(
            next_source_lineage_hash(&parent, 7, &source).as_str(),
            "sha256:f38c86974bb7db4cb455248dfd43fe4b9011f63ba5f87b50523dbd3fc3823307"
        );
    }

    #[test]
    fn oversized_completed_turn_is_bounded_without_blocking_revision() {
        let catalog = SessionMemoryCatalogV3::new(SESSION_ID).unwrap();
        let mut large_answer = answer_ir();
        let oversized_claim = Claim {
            claim_id: "oversized".into(),
            kind: ClaimKind::Fact,
            strength: ClaimStrength::Qualified,
            text: "매우 긴 주장".repeat(40_000),
            goal_ids: Vec::new(),
            evidence_ids: Vec::new(),
            counter_evidence_ids: Vec::new(),
            calculation_ids: Vec::new(),
            subject: Some("AAPL".into()),
            predicate: Some("oversized_projection".into()),
            value: None,
            unit: None,
            period: None,
            comparison_basis: None,
        };
        large_answer
            .claims
            .extend(std::iter::repeat_n(oversized_claim, 12));
        let user = "질문".repeat(100_000);
        let rendered = "답변".repeat(100_000);
        let full_final_output_hash = ContentHash::sha256(serde_jcs::to_vec(&large_answer).unwrap());
        let delta = completed_turn_delta(CompletedTurnInputV3 {
            session_id: SESSION_ID,
            parent_frontier_hash: catalog.frontier_hash.clone(),
            revision: 1,
            run_id: "run:oversized:1",
            final_commit_intent_hash: hash("oversized-intent"),
            answer_bundle_hash: hash("oversized-bundle"),
            user_content: &user,
            rendered_answer: &rendered,
            answer_ir: &large_answer,
            tickers: &["AAPL".into()],
            constraints: &[],
            supersessions: &[],
            resolved_goals: &[],
        })
        .unwrap();

        assert_eq!(delta.revision, 1);
        assert_eq!(delta.source.final_output_hash, full_final_output_hash);
        assert_eq!(
            delta.recent_turn.user_content_hash,
            ContentHash::sha256(&user)
        );
        assert_eq!(
            delta.recent_turn.answer_content_hash,
            ContentHash::sha256(&rendered)
        );
        assert!(delta.recent_turn.user_content_truncated);
        assert!(delta.recent_turn.answer_content_truncated);
        assert!(delta.recent_turn.user_content.len() <= MAX_RECENT_TURN_EXCERPT_BYTES);
        assert!(delta.recent_turn.answer_content.len() <= MAX_RECENT_TURN_EXCERPT_BYTES);
        assert_eq!(
            delta.claims.len(),
            2,
            "oversized claims are omitted, not fatal"
        );
        delta.validate().unwrap();
    }

    fn reanchored_snapshot(revision: u64) -> SessionMemorySnapshotV3 {
        let mut catalog = SessionMemoryCatalogV3::new(SESSION_ID).unwrap();
        catalog.apply_delta(&first_delta(&catalog)).unwrap();
        let mut snapshot = catalog.snapshot().unwrap();
        snapshot.revision = revision;
        snapshot.frontier_hash = hash(&format!("frontier-anchor-{revision}"));
        snapshot.source_lineage_hash = hash(&format!("source-lineage-anchor-{revision}"));
        let count = revision.min(MAX_RECENT_SOURCE_INDEX as u64);
        snapshot.recent_source_revision_index = ((revision - count + 1)..=revision)
            .map(|source_revision| {
                (
                    hash(&format!("historical-source-{source_revision}")).to_string(),
                    source_revision,
                )
            })
            .collect();
        snapshot.validate().unwrap();
        snapshot
    }

    fn apply_one_after_snapshot(snapshot: &SessionMemorySnapshotV3, run_id: &str) {
        let mut catalog = SessionMemoryCatalogV3::from_snapshot(SESSION_ID, snapshot).unwrap();
        let revision = snapshot.revision.checked_add(1).unwrap();
        let delta = completed_turn_delta(CompletedTurnInputV3 {
            session_id: SESSION_ID,
            parent_frontier_hash: snapshot.frontier_hash.clone(),
            revision,
            run_id,
            final_commit_intent_hash: hash("high-intent"),
            answer_bundle_hash: hash("high-bundle"),
            user_content: "고수명 세션 질문",
            rendered_answer: "고수명 세션 답변",
            answer_ir: &answer_ir(),
            tickers: &["AAPL".into()],
            constraints: &[],
            supersessions: &[],
            resolved_goals: &[],
        })
        .unwrap();
        let expected_source_lineage = next_source_lineage_hash(
            &snapshot.source_lineage_hash,
            revision,
            &ContentHash::sha256(run_id),
        );
        catalog.apply_delta(&delta).unwrap();
        assert_eq!(catalog.revision(), revision);
        assert_eq!(catalog.source_lineage_hash(), &expected_source_lineage);
        let next_snapshot = catalog.snapshot().unwrap();
        assert_eq!(next_snapshot.revision, revision);
        assert_eq!(
            next_snapshot.recent_source_revision_index.len(),
            MAX_RECENT_SOURCE_INDEX
        );
        assert_eq!(
            next_snapshot
                .recent_source_revision_index
                .get(ContentHash::sha256(run_id).as_str()),
            Some(&revision)
        );
        next_snapshot.validate().unwrap();
    }

    #[test]
    fn revision_above_4096_uses_bounded_snapshot_lineage() {
        let snapshot = reanchored_snapshot(4_096);
        assert!(snapshot.canonical_bytes().unwrap().len() < MAX_SESSION_MEMORY_SNAPSHOT_BYTES);
        apply_one_after_snapshot(&snapshot, "run:source:4097");
    }

    #[test]
    fn revision_near_postgres_bigint_max_advances_without_history_replay() {
        let snapshot = reanchored_snapshot(MAX_SESSION_MEMORY_REVISION - 1);
        apply_one_after_snapshot(&snapshot, "run:source:bigint-max");
    }

    #[test]
    fn snapshot_tamper_is_rejected_and_restore_preserves_anchor() {
        let snapshot = reanchored_snapshot(8_192);
        let restored = SessionMemoryCatalogV3::from_snapshot(SESSION_ID, &snapshot).unwrap();
        assert_eq!(restored.revision(), snapshot.revision);
        assert_eq!(restored.frontier_hash(), &snapshot.frontier_hash);
        assert_eq!(
            restored.source_lineage_hash(),
            &snapshot.source_lineage_hash
        );

        let mut tampered = snapshot.clone();
        let first = tampered
            .recent_source_revision_index
            .values_mut()
            .next()
            .unwrap();
        *first = 1;
        assert!(tampered.validate().is_err());
    }

    #[test]
    fn question_conditioned_view_is_bounded_and_context_only() {
        let mut catalog = SessionMemoryCatalogV3::new(SESSION_ID).unwrap();
        catalog.apply_delta(&first_delta(&catalog)).unwrap();
        let view = catalog
            .select_view("현금흐름의 최신 기간을 다시 확인해줘", 32 * 1024)
            .unwrap();
        assert_eq!(view.authority, MemoryAuthority::ContextOnly);
        assert!(
            view.claims
                .iter()
                .any(|claim| claim.claim.claim_id == "cash-risk")
        );
        assert!(view.canonical_context().unwrap().len() <= 32 * 1024);
        assert_eq!(view.source_revision, 1);
        assert_eq!(view.view_hash, view.compute_view_hash().unwrap());
    }

    #[test]
    fn view_tamper_and_cross_session_delta_fail_closed() {
        let mut catalog = SessionMemoryCatalogV3::new(SESSION_ID).unwrap();
        let delta = first_delta(&catalog);
        catalog.apply_delta(&delta).unwrap();
        let mut view = catalog.select_view("매출", 32 * 1024).unwrap();
        view.recent_turns[0].answer_content.push_str(" 변조");
        assert!(view.validate(32 * 1024).is_err());

        let mut other = SessionMemoryCatalogV3::new("session:tenant-b:conversation-9").unwrap();
        assert!(matches!(
            other.apply_delta(&delta),
            Err(MemoryError::FrontierMismatch)
        ));
    }

    #[test]
    fn supersession_preserves_old_claim_and_rejects_different_metric() {
        let mut catalog = SessionMemoryCatalogV3::new(SESSION_ID).unwrap();
        catalog.apply_delta(&first_delta(&catalog)).unwrap();
        let older = catalog
            .claims
            .values()
            .find(|claim| claim.claim.claim_id == "revenue")
            .unwrap()
            .memory_id
            .clone();
        let different = catalog
            .claims
            .values()
            .find(|claim| claim.claim.claim_id == "cash-risk")
            .unwrap()
            .memory_id
            .clone();
        assert!(matches!(
            apply_supersession(
                &mut catalog.claims,
                &mut catalog.active_claim_fingerprints,
                &MemorySupersessionV3 {
                    older_memory_id: older.clone(),
                    newer_memory_id: different,
                },
            ),
            Err(MemoryError::InvalidSupersession)
        ));
        assert!(catalog.claims.contains_key(&older));
    }

    #[test]
    fn debug_output_never_contains_user_or_answer_text() {
        let catalog = SessionMemoryCatalogV3::new(SESSION_ID).unwrap();
        let delta = first_delta(&catalog);
        let debug = format!("{delta:?}");
        assert!(!debug.contains("AAPL의 매출"));
        assert!(!debug.contains("11% 증가"));
    }

    fn hash_answer(answer: &AnswerIr) -> ContentHash {
        ContentHash::sha256(serde_jcs::to_vec(answer).unwrap())
    }
}
