use std::collections::BTreeSet;

use super::RecentTurnV3;
use super::terms;

pub(crate) const TARGET_CONTINUITY_TURNS: usize = 4;
pub(crate) const MIN_CONTINUITY_TURNS: usize = 2;

/// Trusted selectors are supplied by the fenced run request. They are only
/// used to break ties among records that already belong to this session; they
/// never widen the session-memory search scope.
#[derive(Debug, Clone, Copy)]
pub struct SessionViewQuery<'a> {
    pub question: &'a str,
    pub trusted_tickers: &'a [String],
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct RankedCandidate {
    pub index: usize,
    pub lexical_overlap: usize,
    pub ticker_hits: usize,
    pub score: u32,
}

pub(crate) fn normalized_recency(current_revision: u64, source_revision: u64) -> u32 {
    let age = current_revision.saturating_sub(source_revision).min(31);
    31_u32.saturating_sub(age as u32)
}

pub(crate) fn ticker_hits(trusted_tickers: &[String], text: &str) -> usize {
    if trusted_tickers.is_empty() {
        return 0;
    }
    let candidate_terms = terms(text);
    trusted_tickers
        .iter()
        .filter(|ticker| {
            let normalized = ticker.to_ascii_lowercase();
            candidate_terms.contains(&normalized)
        })
        .count()
}

/// An explicit prior-turn scope that does not intersect the current fenced
/// scope is not usable room continuity.  Empty scope is legacy/unknown data:
/// it may still surface through a relevant lexical match, but is never pinned
/// merely for recency.
pub(crate) fn ticker_scope_conflicts(
    candidate_tickers: &[String],
    trusted_tickers: &[String],
) -> bool {
    !trusted_tickers.is_empty()
        && !candidate_tickers.is_empty()
        && !candidate_tickers
            .iter()
            .any(|candidate| trusted_tickers.iter().any(|trusted| trusted == candidate))
}

pub(crate) fn ticker_scope_matches_for_continuity(
    candidate_tickers: &[String],
    trusted_tickers: &[String],
) -> bool {
    trusted_tickers.is_empty()
        || (!candidate_tickers.is_empty()
            && candidate_tickers
                .iter()
                .any(|candidate| trusted_tickers.iter().any(|trusted| trusted == candidate)))
}

pub(crate) fn rank_text(
    question_terms: &BTreeSet<String>,
    candidate_text: &str,
    trusted_tickers: &[String],
    current_revision: u64,
    source_revision: u64,
    index: usize,
) -> RankedCandidate {
    let candidate_terms = terms(candidate_text);
    let lexical_overlap = question_terms.intersection(&candidate_terms).count();
    let ticker_hits = ticker_hits(trusted_tickers, candidate_text);
    let recency = normalized_recency(current_revision, source_revision);
    let score = u32::try_from(lexical_overlap)
        .unwrap_or(u32::MAX)
        .saturating_mul(64)
        .saturating_add(
            u32::try_from(ticker_hits)
                .unwrap_or(u32::MAX)
                .saturating_mul(256),
        )
        .saturating_add(recency);
    RankedCandidate {
        index,
        lexical_overlap,
        ticker_hits,
        score,
    }
}

pub(crate) fn is_relevant(candidate: RankedCandidate) -> bool {
    candidate.lexical_overlap > 0 || candidate.ticker_hits > 0
}

pub(crate) fn turn_text(turn: &RecentTurnV3) -> String {
    format!("{} {}", turn.user_content, turn.answer_content)
}
