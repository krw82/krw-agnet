//! Supplemental company-research ladder: the filing-event and news fallback
//! reads behind event-shaped premises.
//!
//! The physical contracts are the same `krw-ontology-front` surfaces the
//! krw-feed deployment serves, so exchange validation and the observed-id
//! authorization state are shared with [`crate::front_mapping`]. What differs
//! is the evidence projection: results map through the ontology adapter's
//! supplemental mappers so the analyst's ledger receives vendor-neutral
//! records instead of the feed-deployment projection.

use krw_agent_contracts::{
    KRW_FEED_CONTEXT_INPUT_V2, KRW_FEED_CONTEXT_V2, KRW_FEED_LIST_ITEMS_INPUT_V1,
    KRW_FEED_LIST_ITEMS_RESULT_V1, KRW_FILING_BRIEF_INPUT_V1, KRW_FILING_BRIEF_RESULT_V1,
    KRW_FILING_SEARCH_INPUT_V1, KRW_FILING_SEARCH_RESULT_V1, KRW_WEB_NEWS_SEARCH_INPUT_V1,
    KRW_WEB_NEWS_SEARCH_RESULT_V1, NORMALIZED_CAPABILITY_RESULT_V1, validate_front_exchange,
};
use krw_agent_evidence::{Answerability, EvidenceRecord};
use krw_agent_execution_contracts::DependencyFailure;
use krw_ontology_adapter::{MappingContext, map_feed_issue_context, map_filing_event_brief, map_filing_event_search, map_web_news};
use serde_json::{Map, Value, json};

use crate::front_mapping::FrontRunState;
use crate::reject;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LadderMapping {
    FilingEventSearch,
    FilingEventBrief,
    FeedIssueList,
    FeedIssueContext,
    WebNewsSearch,
}

impl LadderMapping {
    pub(crate) const fn input_contract(self) -> &'static str {
        match self {
            Self::FilingEventSearch => KRW_FILING_SEARCH_INPUT_V1,
            Self::FilingEventBrief => KRW_FILING_BRIEF_INPUT_V1,
            Self::FeedIssueList => KRW_FEED_LIST_ITEMS_INPUT_V1,
            Self::FeedIssueContext => KRW_FEED_CONTEXT_INPUT_V2,
            Self::WebNewsSearch => KRW_WEB_NEWS_SEARCH_INPUT_V1,
        }
    }

    pub(crate) const fn output_contract(self) -> &'static str {
        match self {
            Self::FilingEventSearch => KRW_FILING_SEARCH_RESULT_V1,
            Self::FilingEventBrief => KRW_FILING_BRIEF_RESULT_V1,
            Self::FeedIssueList => KRW_FEED_LIST_ITEMS_RESULT_V1,
            Self::FeedIssueContext => KRW_FEED_CONTEXT_V2,
            Self::WebNewsSearch => KRW_WEB_NEWS_SEARCH_RESULT_V1,
        }
    }

    pub(crate) const fn output_contracts(self) -> &'static [&'static str] {
        match self {
            Self::FilingEventSearch => &[
                KRW_FILING_SEARCH_RESULT_V1,
                NORMALIZED_CAPABILITY_RESULT_V1,
            ],
            Self::FilingEventBrief => &[
                KRW_FILING_BRIEF_RESULT_V1,
                NORMALIZED_CAPABILITY_RESULT_V1,
            ],
            Self::FeedIssueList => &[
                KRW_FEED_LIST_ITEMS_RESULT_V1,
                NORMALIZED_CAPABILITY_RESULT_V1,
            ],
            Self::FeedIssueContext => &[KRW_FEED_CONTEXT_V2, NORMALIZED_CAPABILITY_RESULT_V1],
            Self::WebNewsSearch => &[
                KRW_WEB_NEWS_SEARCH_RESULT_V1,
                NORMALIZED_CAPABILITY_RESULT_V1,
            ],
        }
    }

    /// True when dispatch requires a prior observed identifier.
    pub(crate) const fn requires_observed_ids(self) -> bool {
        matches!(self, Self::FilingEventBrief | Self::FeedIssueContext)
    }

    /// True when the physical pairing is a `krw-ontology-front` exchange.
    /// The local web-news lookup owns its own typed result contract instead.
    pub(crate) const fn uses_front_exchange(self) -> bool {
        !matches!(self, Self::WebNewsSearch)
    }
}

/// Fail-closed pre-dispatch authorization for follow-up reads: every
/// requested identifier must have been observed by a prior committed
/// producer result of this run.
pub(crate) fn authorize_ladder(
    mapping: LadderMapping,
    front_state: &FrontRunState,
    arguments: &Value,
) -> Result<(), DependencyFailure> {
    match mapping {
        LadderMapping::FilingEventBrief => {
            let event_id = arguments
                .get("filing_event_id")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    reject(
                        "filing_brief_input_identity",
                        "filing brief input did not retain its filing event id",
                    )
                })?;
            front_state.authorize_observed_filing_event(event_id).map(|_| ())
        }
        LadderMapping::FeedIssueContext => {
            let issue_ids = arguments
                .get("issue_ids")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    reject(
                        "feed_context_input_identity",
                        "feed context input did not retain its issue ids",
                    )
                })?;
            if issue_ids.is_empty() {
                return Err(reject(
                    "feed_context_input_identity",
                    "feed context input retained no issue ids",
                ));
            }
            for issue_id in issue_ids {
                let issue_id = issue_id.as_str().ok_or_else(|| {
                    reject(
                        "feed_context_input_identity",
                        "feed context issue id must be a string",
                    )
                })?;
                front_state
                    .authorize_observed_feed_issue(issue_id)
                    .map(|_| ())?;
            }
            Ok(())
        }
        LadderMapping::FilingEventSearch
        | LadderMapping::FeedIssueList
        | LadderMapping::WebNewsSearch => Ok(()),
    }
}

/// Validate the exchange pairing and fold a committed producer result into
/// the observed-id authorization state.
pub(crate) fn apply_ladder_committed(
    mapping: LadderMapping,
    front_state: &mut FrontRunState,
    arguments: &Value,
    payload: &Value,
) -> Result<(), DependencyFailure> {
    if mapping.uses_front_exchange() {
        validate_ladder_exchange(mapping, arguments, payload)?;
    }
    match mapping {
        LadderMapping::FilingEventSearch => {
            front_state.observe_ladder_filing_search(arguments, payload)
        }
        LadderMapping::FeedIssueList => front_state.observe_ladder_feed_list(arguments, payload),
        LadderMapping::FilingEventBrief
        | LadderMapping::FeedIssueContext
        | LadderMapping::WebNewsSearch => Ok(()),
    }
}

pub(crate) fn validate_ladder_exchange(
    mapping: LadderMapping,
    arguments: &Value,
    payload: &Value,
) -> Result<(), DependencyFailure> {
    validate_front_exchange(
        mapping.input_contract(),
        arguments,
        mapping.output_contract(),
        payload,
    )
    .map_err(|error| reject("ladder_exchange_invalid", format!("{error:?}")))
}

/// Project one validated ladder result into supplemental evidence records.
/// The raw physical payloads stay the provider-visible content; only the
/// bounded, vendor-neutral projection enters the ledger.
pub(crate) fn map_ladder_evidence(
    mapping: LadderMapping,
    front_state: Option<&FrontRunState>,
    arguments: &Value,
    payload: &Value,
    context: &MappingContext,
) -> Result<(Vec<EvidenceRecord>, Option<Answerability>), DependencyFailure> {
    match mapping {
        LadderMapping::FilingEventSearch => {
            let ticker = required_ticker(arguments)?;
            // The physical result is a bare canonical array; the adapter's
            // mapper consumes the items envelope.
            let wrapped = json!({"items": payload});
            let records = map_filing_event_search(&wrapped, ticker, context)
                .map_err(|error| reject("filing_event_search_mapping", format!("{error:?}")))?;
            let answerability = if records.is_empty() {
                Answerability::QualifiedOnly
            } else {
                Answerability::StrongAllowed
            };
            Ok((records, Some(answerability)))
        }
        LadderMapping::FilingEventBrief => {
            let front_state = front_state.ok_or_else(|| {
                reject(
                    "front_state_unavailable",
                    "front validation state was not provided",
                )
            })?;
            let event_id = arguments
                .get("filing_event_id")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    reject(
                        "filing_brief_input_identity",
                        "filing brief input did not retain its filing event id",
                    )
                })?;
            // Scope inheritance: the trusted ticker is the one whose
            // scope-validated search observed this event.
            let ticker = front_state.authorize_observed_filing_event(event_id)?;
            let flattened = flatten_filing_brief(payload)?;
            let record = map_filing_event_brief(&flattened, ticker, context)
                .map_err(|error| reject("filing_event_brief_mapping", format!("{error:?}")))?;
            Ok((vec![record], Some(Answerability::StrongAllowed)))
        }
        LadderMapping::FeedIssueList => {
            let requested = requested_feed_tickers(arguments);
            let items = required_items(payload)?;
            let mut records = Vec::new();
            for item in items.iter().take(MAX_LADDER_ITEM_RECORDS) {
                let Some(object) = item.as_object() else {
                    continue;
                };
                // Same binding rule the observation state applies: a matching
                // entity ticker wins, a unique requested ticker binds the
                // rest, and an unresolvable issue contributes no evidence.
                let Some(ticker) = ladder_issue_ticker(object, &requested) else {
                    continue;
                };
                if let Ok(record) = map_feed_issue_context(item, ticker, context) {
                    records.push(record);
                }
            }
            Ok((records, Some(Answerability::QualifiedOnly)))
        }
        LadderMapping::FeedIssueContext => {
            let front_state = front_state.ok_or_else(|| {
                reject(
                    "front_state_unavailable",
                    "front validation state was not provided",
                )
            })?;
            let items = required_items(payload)?;
            let mut records = Vec::new();
            for item in items.iter().take(MAX_LADDER_ITEM_RECORDS) {
                let Some(issue_id) = item.get("id").and_then(Value::as_str) else {
                    continue;
                };
                let Ok(ticker) = front_state.authorize_observed_feed_issue(issue_id) else {
                    continue;
                };
                if let Ok(record) = map_feed_issue_context(item, ticker, context) {
                    records.push(record);
                }
            }
            Ok((records, Some(Answerability::QualifiedOnly)))
        }
        LadderMapping::WebNewsSearch => {
            let ticker = required_ticker(arguments)?;
            let records = map_web_news(payload, ticker, context)
                .map_err(|error| reject("web_news_mapping", format!("{error:?}")))?;
            Ok((records, Some(Answerability::QualifiedOnly)))
        }
    }
}

const MAX_LADDER_ITEM_RECORDS: usize = 64;

fn required_ticker(arguments: &Value) -> Result<&str, DependencyFailure> {
    arguments
        .get("ticker")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            reject(
                "ladder_input_identity",
                "ladder input did not retain a ticker",
            )
        })
}

fn requested_feed_tickers(arguments: &Value) -> Vec<&str> {
    arguments
        .get("tickers")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice)
        .iter()
        .filter_map(Value::as_str)
        .collect()
}

/// Resolve the trusted ticker one feed issue is bound to: a matching entity
/// ticker wins, a unique requested ticker binds everything else. Shared with
/// the observation state so a list's evidence binding and its recorded
/// authorization can never drift apart.
pub(crate) fn ladder_issue_ticker<'a>(
    object: &Map<String, Value>,
    requested: &[&'a str],
) -> Option<&'a str> {
    if let Some(entities) = object.get("entities").and_then(Value::as_array) {
        for entity in entities {
            let Some(ticker) = entity.get("ticker").and_then(Value::as_str) else {
                continue;
            };
            if let Some(trusted) = requested.iter().find(|candidate| *candidate == &ticker) {
                return Some(trusted);
            }
        }
    }
    if requested.len() == 1 {
        return Some(requested[0]);
    }
    None
}

fn required_items(payload: &Value) -> Result<&[Value], DependencyFailure> {
    payload
        .get("items")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or_else(|| reject("ladder_output_shape", "canonical items array is missing"))
}

/// Flatten one `krw-filing-brief-result/v1` into the single object the
/// adapter's brief mapper consumes. The physical result nests the filing
/// metadata beside the brief; the mapper requires identity fields (form
/// type, filing date, event id, SEC items) at the top level alongside the
/// brief's headline and sentences, so the two objects merge with the brief
/// winning key conflicts. The first event tag rides along as the citation
/// event tag.
fn flatten_filing_brief(payload: &Value) -> Result<Value, DependencyFailure> {
    let filing = payload
        .get("filing")
        .and_then(Value::as_object)
        .ok_or_else(|| reject("filing_brief_shape", "brief result lacks its filing metadata"))?;
    let mut flattened: Map<String, Value> = filing.clone();
    if let Some(tags) = filing.get("event_tags").and_then(Value::as_array)
        && let Some(first) = tags.first()
    {
        flattened
            .entry("event_tag".to_owned())
            .or_insert_with(|| first.clone());
    }
    if let Some(brief) = payload.get("brief").and_then(Value::as_object) {
        for (key, value) in brief {
            flattened.insert(key.clone(), value.clone());
        }
    }
    Ok(Value::Object(flattened))
}
