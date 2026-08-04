//! Closed, deny-monotone policy composition for the KRW agent kernel.
//!
//! This is deliberately not a hook runtime. It cannot execute code, perform
//! I/O, grant permissions, raise evidence strength, or increase a budget. A
//! compiled agent program can only make an already-authorized execution more
//! restrictive and the resulting canonical receipt is safe to persist.

use std::collections::{BTreeMap, BTreeSet};

use krw_agent_protocol::{BudgetLimits, ContentHash};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const POLICY_RECEIPT_SCHEMA_VERSION: u16 = 1;

const MAX_DECISIONS: usize = 64;
const MAX_CAPABILITIES: usize = 64;
const MAX_CONTEXT_REFS: usize = 256;
const MAX_CLAIMS: usize = 256;
const MAX_ISSUES: usize = 64;
const MAX_OBSERVERS: usize = 64;
const MAX_ID_BYTES: usize = 128;

/// Closed lifecycle points at which a compiled policy program may run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyPhase {
    Admission,
    PlanValidation,
    BeforeAction,
    AfterAction,
    BeforeCompose,
    BeforeFinalCommit,
}

/// Authority order is recorded for audit. It never changes the core rule that
/// one denial at any layer is final.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyAuthority {
    KernelInvariant,
    Deployment,
    AgentImage,
    Role,
    PrincipalScope,
    Capability,
    RunScope,
    Resource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifierTier {
    None,
    Structural,
    Evidence,
    HighRiskSemantic,
}

/// Higher variants are weaker. Policies may only move a claim toward a larger
/// value, never in the opposite direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimStrength {
    Strong,
    Qualified,
    Unverified,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PolicyEffect {
    Deny {
        reason_code: String,
    },
    RestrictCapabilities {
        retained: BTreeSet<String>,
    },
    RestrictContextRefs {
        retained: BTreeSet<String>,
    },
    TightenBudget {
        limits: BudgetLimits,
    },
    RequireVerifier {
        tier: VerifierTier,
    },
    DowngradeClaims {
        claims: BTreeMap<String, ClaimStrength>,
    },
    RequestRepair {
        issue_codes: BTreeSet<String>,
    },
    EmitObserver {
        event_kind: String,
        payload_ref: ContentHash,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyDecision {
    pub policy_id: String,
    pub phase: PolicyPhase,
    pub authority: PolicyAuthority,
    pub effect: PolicyEffect,
}

/// The maximum authority available before restrictive policy composition.
/// Construction must happen only after the kernel's positive authorization
/// checks have independently succeeded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyCeiling {
    pub capabilities: BTreeSet<String>,
    pub context_refs: BTreeSet<String>,
    pub budget: BudgetLimits,
    pub verifier_tier: VerifierTier,
    pub claim_strengths: BTreeMap<String, ClaimStrength>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObserverReceipt {
    pub event_kind: String,
    pub payload_ref: ContentHash,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppliedPolicyDecision {
    pub policy_id: String,
    pub phase: PolicyPhase,
    pub authority: PolicyAuthority,
    pub effect_hash: ContentHash,
}

/// Canonical, content-addressed audit output. It contains no prompt, tool
/// result, principal identifier, or private provider reasoning.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyReceipt {
    pub schema_version: u16,
    pub denied: bool,
    pub deny_reason_codes: BTreeSet<String>,
    pub retained_capabilities: BTreeSet<String>,
    pub retained_context_refs: BTreeSet<String>,
    pub effective_budget: BudgetLimits,
    pub required_verifier: VerifierTier,
    pub claim_strengths: BTreeMap<String, ClaimStrength>,
    pub repair_issue_codes: BTreeSet<String>,
    pub observers: Vec<ObserverReceipt>,
    pub applied: Vec<AppliedPolicyDecision>,
}

impl PolicyReceipt {
    pub fn content_hash(&self) -> Result<ContentHash, PolicyError> {
        Ok(ContentHash::sha256(serde_jcs::to_vec(self)?))
    }
}

#[derive(Debug, Clone)]
pub struct PolicyAccumulator {
    receipt: PolicyReceipt,
    seen_policy_ids: BTreeSet<String>,
}

impl PolicyAccumulator {
    pub fn new(ceiling: PolicyCeiling) -> Result<Self, PolicyError> {
        validate_ceiling(&ceiling)?;
        Ok(Self {
            receipt: PolicyReceipt {
                schema_version: POLICY_RECEIPT_SCHEMA_VERSION,
                denied: false,
                deny_reason_codes: BTreeSet::new(),
                retained_capabilities: ceiling.capabilities,
                retained_context_refs: ceiling.context_refs,
                effective_budget: ceiling.budget,
                required_verifier: ceiling.verifier_tier,
                claim_strengths: ceiling.claim_strengths,
                repair_issue_codes: BTreeSet::new(),
                observers: Vec::new(),
                applied: Vec::new(),
            },
            seen_policy_ids: BTreeSet::new(),
        })
    }

    pub fn apply(&mut self, decision: PolicyDecision) -> Result<(), PolicyError> {
        if self.receipt.applied.len() >= MAX_DECISIONS {
            return Err(PolicyError::Limit("decisions"));
        }
        validate_id(&decision.policy_id, "policy_id")?;
        if !self.seen_policy_ids.insert(decision.policy_id.clone()) {
            return Err(PolicyError::DuplicatePolicy(decision.policy_id));
        }
        validate_effect(&decision.effect)?;
        let effect_hash = ContentHash::sha256(serde_jcs::to_vec(&decision.effect)?);

        match &decision.effect {
            PolicyEffect::Deny { reason_code } => {
                self.receipt.denied = true;
                self.receipt.deny_reason_codes.insert(reason_code.clone());
            }
            PolicyEffect::RestrictCapabilities { retained } => {
                require_subset(
                    retained,
                    &self.receipt.retained_capabilities,
                    "capabilities",
                )?;
                self.receipt.retained_capabilities.clone_from(retained);
            }
            PolicyEffect::RestrictContextRefs { retained } => {
                require_subset(
                    retained,
                    &self.receipt.retained_context_refs,
                    "context refs",
                )?;
                self.receipt.retained_context_refs.clone_from(retained);
            }
            PolicyEffect::TightenBudget { limits } => {
                require_budget_not_larger(limits, &self.receipt.effective_budget)?;
                self.receipt.effective_budget = limits.clone();
            }
            PolicyEffect::RequireVerifier { tier } => {
                if *tier < self.receipt.required_verifier {
                    return Err(PolicyError::AuthorityExpansion("verifier tier"));
                }
                self.receipt.required_verifier = *tier;
            }
            PolicyEffect::DowngradeClaims { claims } => {
                for (claim_id, requested) in claims {
                    let current = self
                        .receipt
                        .claim_strengths
                        .get_mut(claim_id)
                        .ok_or_else(|| PolicyError::UnknownClaim(claim_id.clone()))?;
                    if *requested < *current {
                        return Err(PolicyError::AuthorityExpansion("claim strength"));
                    }
                    *current = *requested;
                }
            }
            PolicyEffect::RequestRepair { issue_codes } => {
                self.receipt
                    .repair_issue_codes
                    .extend(issue_codes.iter().cloned());
                if self.receipt.repair_issue_codes.len() > MAX_ISSUES {
                    return Err(PolicyError::Limit("repair issues"));
                }
            }
            PolicyEffect::EmitObserver {
                event_kind,
                payload_ref,
            } => {
                if self.receipt.observers.len() >= MAX_OBSERVERS {
                    return Err(PolicyError::Limit("observers"));
                }
                self.receipt.observers.push(ObserverReceipt {
                    event_kind: event_kind.clone(),
                    payload_ref: payload_ref.clone(),
                });
            }
        }

        self.receipt.applied.push(AppliedPolicyDecision {
            policy_id: decision.policy_id,
            phase: decision.phase,
            authority: decision.authority,
            effect_hash,
        });
        Ok(())
    }

    pub fn finish(self) -> PolicyReceipt {
        self.receipt
    }
}

fn validate_ceiling(ceiling: &PolicyCeiling) -> Result<(), PolicyError> {
    validate_set(&ceiling.capabilities, MAX_CAPABILITIES, "capabilities")?;
    validate_set(&ceiling.context_refs, MAX_CONTEXT_REFS, "context refs")?;
    validate_budget(&ceiling.budget)?;
    if ceiling.claim_strengths.len() > MAX_CLAIMS {
        return Err(PolicyError::Limit("claims"));
    }
    for claim_id in ceiling.claim_strengths.keys() {
        validate_id(claim_id, "claim_id")?;
    }
    Ok(())
}

fn validate_effect(effect: &PolicyEffect) -> Result<(), PolicyError> {
    match effect {
        PolicyEffect::Deny { reason_code } => validate_id(reason_code, "reason_code"),
        PolicyEffect::RestrictCapabilities { retained } => {
            validate_set(retained, MAX_CAPABILITIES, "capabilities")
        }
        PolicyEffect::RestrictContextRefs { retained } => {
            validate_set(retained, MAX_CONTEXT_REFS, "context refs")
        }
        PolicyEffect::TightenBudget { limits } => validate_budget(limits),
        PolicyEffect::RequireVerifier { .. } => Ok(()),
        PolicyEffect::DowngradeClaims { claims } => {
            if claims.len() > MAX_CLAIMS {
                return Err(PolicyError::Limit("claims"));
            }
            for claim_id in claims.keys() {
                validate_id(claim_id, "claim_id")?;
            }
            Ok(())
        }
        PolicyEffect::RequestRepair { issue_codes } => {
            validate_set(issue_codes, MAX_ISSUES, "repair issues")
        }
        PolicyEffect::EmitObserver { event_kind, .. } => validate_id(event_kind, "observer event"),
    }
}

fn validate_set(
    values: &BTreeSet<String>,
    limit: usize,
    field: &'static str,
) -> Result<(), PolicyError> {
    if values.len() > limit {
        return Err(PolicyError::Limit(field));
    }
    for value in values {
        validate_id(value, field)?;
    }
    Ok(())
}

fn validate_id(value: &str, field: &'static str) -> Result<(), PolicyError> {
    if value.is_empty()
        || value.len() > MAX_ID_BYTES
        || value.contains('\0')
        || value.chars().any(char::is_control)
    {
        return Err(PolicyError::InvalidIdentifier(field));
    }
    Ok(())
}

fn require_subset(
    candidate: &BTreeSet<String>,
    current: &BTreeSet<String>,
    field: &'static str,
) -> Result<(), PolicyError> {
    if candidate.is_subset(current) {
        Ok(())
    } else {
        Err(PolicyError::AuthorityExpansion(field))
    }
}

fn validate_budget(limits: &BudgetLimits) -> Result<(), PolicyError> {
    if limits.max_provider_turns == 0
        || limits.max_output_tokens == 0
        || limits.deadline_ms == 0
        || limits
            .capability_call_limits
            .iter()
            .any(|(id, limit)| validate_id(id, "capability limit").is_err() || *limit == 0)
    {
        return Err(PolicyError::InvalidBudget);
    }
    Ok(())
}

fn require_budget_not_larger(
    candidate: &BudgetLimits,
    current: &BudgetLimits,
) -> Result<(), PolicyError> {
    validate_budget(candidate)?;
    let scalar_not_larger = candidate.max_provider_turns <= current.max_provider_turns
        && candidate.max_capability_calls <= current.max_capability_calls
        && candidate.max_replans <= current.max_replans
        && candidate.max_repairs <= current.max_repairs
        && candidate.max_input_tokens <= current.max_input_tokens
        && candidate.max_output_tokens <= current.max_output_tokens
        && candidate.max_evidence_bytes <= current.max_evidence_bytes
        && candidate.deadline_ms <= current.deadline_ms;
    // Missing means "no per-capability ceiling" in `BudgetLimits`, so a
    // policy must preserve and tighten every existing key. It may add a new
    // key because that constrains a capability that was previously governed
    // only by the global call budget.
    let per_capability_not_larger =
        current
            .capability_call_limits
            .iter()
            .all(|(id, current_limit)| {
                candidate
                    .capability_call_limits
                    .get(id)
                    .is_some_and(|limit| limit <= current_limit)
            });
    if scalar_not_larger && per_capability_not_larger {
        Ok(())
    } else {
        Err(PolicyError::AuthorityExpansion("budget"))
    }
}

#[derive(Debug, Error)]
pub enum PolicyError {
    #[error("policy field exceeds the closed bound: {0}")]
    Limit(&'static str),
    #[error("invalid bounded identifier in {0}")]
    InvalidIdentifier(&'static str),
    #[error("duplicate policy id: {0}")]
    DuplicatePolicy(String),
    #[error("policy attempted to expand authority through {0}")]
    AuthorityExpansion(&'static str),
    #[error("policy referenced an unknown claim: {0}")]
    UnknownClaim(String),
    #[error("policy budget is invalid")]
    InvalidBudget,
    #[error("policy receipt serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(values: &[&str]) -> BTreeSet<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    fn budget() -> BudgetLimits {
        BudgetLimits {
            max_provider_turns: 8,
            max_capability_calls: 6,
            max_replans: 3,
            max_repairs: 1,
            max_input_tokens: 50_000,
            max_output_tokens: 8_000,
            max_evidence_bytes: 4 * 1024 * 1024,
            deadline_ms: 60_000,
            capability_call_limits: BTreeMap::from([
                ("ontology.query_context".into(), 2),
                ("ontology.query".into(), 2),
                ("ontology.trace".into(), 1),
            ]),
        }
    }

    fn ceiling() -> PolicyCeiling {
        PolicyCeiling {
            capabilities: set(&["ontology.query_context", "ontology.query", "ontology.trace"]),
            context_refs: set(&["scope", "evidence", "memory"]),
            budget: budget(),
            verifier_tier: VerifierTier::Structural,
            claim_strengths: BTreeMap::from([
                ("claim-a".into(), ClaimStrength::Strong),
                ("claim-b".into(), ClaimStrength::Qualified),
            ]),
        }
    }

    fn decision(id: &str, effect: PolicyEffect) -> PolicyDecision {
        PolicyDecision {
            policy_id: id.into(),
            phase: PolicyPhase::BeforeAction,
            authority: PolicyAuthority::AgentImage,
            effect,
        }
    }

    #[test]
    fn composition_can_only_reduce_authority() {
        let mut accumulator = PolicyAccumulator::new(ceiling()).unwrap();
        accumulator
            .apply(decision(
                "capability-frontier",
                PolicyEffect::RestrictCapabilities {
                    retained: set(&["ontology.query_context", "ontology.query"]),
                },
            ))
            .unwrap();
        accumulator
            .apply(decision(
                "context-minimization",
                PolicyEffect::RestrictContextRefs {
                    retained: set(&["scope", "evidence"]),
                },
            ))
            .unwrap();
        let mut tightened = budget();
        tightened.max_provider_turns = 6;
        tightened
            .capability_call_limits
            .insert("ontology.query".into(), 1);
        accumulator
            .apply(decision(
                "resource-pressure",
                PolicyEffect::TightenBudget { limits: tightened },
            ))
            .unwrap();
        accumulator
            .apply(decision(
                "risk-verifier",
                PolicyEffect::RequireVerifier {
                    tier: VerifierTier::HighRiskSemantic,
                },
            ))
            .unwrap();
        accumulator
            .apply(decision(
                "claim-ceiling",
                PolicyEffect::DowngradeClaims {
                    claims: BTreeMap::from([("claim-a".into(), ClaimStrength::Unverified)]),
                },
            ))
            .unwrap();

        let receipt = accumulator.finish();
        assert_eq!(receipt.retained_capabilities.len(), 2);
        assert_eq!(receipt.retained_context_refs, set(&["scope", "evidence"]));
        assert_eq!(receipt.effective_budget.max_provider_turns, 6);
        assert_eq!(receipt.required_verifier, VerifierTier::HighRiskSemantic);
        assert_eq!(
            receipt.claim_strengths["claim-a"],
            ClaimStrength::Unverified
        );
        assert!(!receipt.denied);
    }

    #[test]
    fn every_authority_expansion_is_rejected() {
        let attempts = [
            PolicyEffect::RestrictCapabilities {
                retained: set(&["ontology.query_context", "write.anything"]),
            },
            PolicyEffect::RestrictContextRefs {
                retained: set(&["scope", "cross-principal"]),
            },
            PolicyEffect::RequireVerifier {
                tier: VerifierTier::None,
            },
            PolicyEffect::DowngradeClaims {
                claims: BTreeMap::from([("claim-b".into(), ClaimStrength::Strong)]),
            },
        ];
        for (index, effect) in attempts.into_iter().enumerate() {
            let mut accumulator = PolicyAccumulator::new(ceiling()).unwrap();
            assert!(matches!(
                accumulator.apply(decision(&format!("attempt-{index}"), effect)),
                Err(PolicyError::AuthorityExpansion(_))
            ));
        }

        let mut expanded = budget();
        expanded.deadline_ms += 1;
        let mut accumulator = PolicyAccumulator::new(ceiling()).unwrap();
        assert!(matches!(
            accumulator.apply(decision(
                "budget-expansion",
                PolicyEffect::TightenBudget { limits: expanded }
            )),
            Err(PolicyError::AuthorityExpansion("budget"))
        ));

        let mut removed_limit = budget();
        removed_limit
            .capability_call_limits
            .remove("ontology.trace");
        let mut accumulator = PolicyAccumulator::new(ceiling()).unwrap();
        assert!(matches!(
            accumulator.apply(decision(
                "removed-limit",
                PolicyEffect::TightenBudget {
                    limits: removed_limit
                }
            )),
            Err(PolicyError::AuthorityExpansion("budget"))
        ));
    }

    #[test]
    fn denial_is_irreversible_and_all_reasons_are_retained() {
        let mut accumulator = PolicyAccumulator::new(ceiling()).unwrap();
        for (id, reason) in [("scope", "scope_denied"), ("budget", "budget_denied")] {
            accumulator
                .apply(decision(
                    id,
                    PolicyEffect::Deny {
                        reason_code: reason.into(),
                    },
                ))
                .unwrap();
        }
        accumulator
            .apply(decision(
                "more-restrictive",
                PolicyEffect::RestrictCapabilities {
                    retained: BTreeSet::new(),
                },
            ))
            .unwrap();
        let receipt = accumulator.finish();
        assert!(receipt.denied);
        assert_eq!(
            receipt.deny_reason_codes,
            set(&["scope_denied", "budget_denied"])
        );
        assert!(receipt.retained_capabilities.is_empty());
    }

    #[test]
    fn adding_a_new_per_capability_ceiling_is_restrictive() {
        let mut accumulator = PolicyAccumulator::new(ceiling()).unwrap();
        let mut tightened = budget();
        tightened
            .capability_call_limits
            .insert("new.read_capability".into(), 1);
        accumulator
            .apply(decision(
                "new-limit",
                PolicyEffect::TightenBudget { limits: tightened },
            ))
            .unwrap();
        assert_eq!(
            accumulator.finish().effective_budget.capability_call_limits["new.read_capability"],
            1
        );
    }

    #[test]
    fn canonical_receipt_hash_is_deterministic() {
        fn produce() -> PolicyReceipt {
            let mut accumulator = PolicyAccumulator::new(ceiling()).unwrap();
            accumulator
                .apply(decision(
                    "observe",
                    PolicyEffect::EmitObserver {
                        event_kind: "policy_audit".into(),
                        payload_ref: ContentHash::sha256("payload"),
                    },
                ))
                .unwrap();
            accumulator.finish()
        }
        assert_eq!(
            produce().content_hash().unwrap(),
            produce().content_hash().unwrap()
        );
    }

    #[test]
    fn duplicate_policy_and_unknown_claim_fail_closed() {
        let mut accumulator = PolicyAccumulator::new(ceiling()).unwrap();
        accumulator
            .apply(decision(
                "same",
                PolicyEffect::RequireVerifier {
                    tier: VerifierTier::Evidence,
                },
            ))
            .unwrap();
        assert!(matches!(
            accumulator.apply(decision(
                "same",
                PolicyEffect::RequireVerifier {
                    tier: VerifierTier::HighRiskSemantic,
                }
            )),
            Err(PolicyError::DuplicatePolicy(_))
        ));

        let mut accumulator = PolicyAccumulator::new(ceiling()).unwrap();
        assert!(matches!(
            accumulator.apply(decision(
                "unknown-claim",
                PolicyEffect::DowngradeClaims {
                    claims: BTreeMap::from([("not-authorized".into(), ClaimStrength::Unverified)])
                }
            )),
            Err(PolicyError::UnknownClaim(_))
        ));
    }
}
