//! Bounded deficit-round-robin admission for a small resident claim window.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmissionItem {
    pub run_id: String,
    pub session_id: String,
    pub principal_id: String,
    pub estimated_cost: u32,
}

#[derive(Debug)]
pub struct FairScheduler {
    max_resident: usize,
    quantum: u32,
    max_item_cost: u32,
    resident: usize,
    queues: BTreeMap<String, VecDeque<AdmissionItem>>,
    order: VecDeque<String>,
    deficit: BTreeMap<String, u64>,
    active_sessions: BTreeSet<String>,
    resident_runs: BTreeSet<String>,
}

impl FairScheduler {
    pub fn new(
        max_resident: usize,
        quantum: u32,
        max_item_cost: u32,
    ) -> Result<Self, SchedulerError> {
        if max_resident == 0 {
            return Err(SchedulerError::InvalidResidentWindow);
        }
        if quantum == 0 || max_item_cost == 0 || quantum > max_item_cost {
            return Err(SchedulerError::InvalidCostConfiguration);
        }
        Ok(Self {
            max_resident,
            quantum,
            max_item_cost,
            resident: 0,
            queues: BTreeMap::new(),
            order: VecDeque::new(),
            deficit: BTreeMap::new(),
            active_sessions: BTreeSet::new(),
            resident_runs: BTreeSet::new(),
        })
    }

    pub fn enqueue(&mut self, item: AdmissionItem) -> Result<(), SchedulerError> {
        if self.resident >= self.max_resident {
            return Err(SchedulerError::ResidentWindowFull(self.max_resident));
        }
        if item.estimated_cost == 0 || item.estimated_cost > self.max_item_cost {
            return Err(SchedulerError::InvalidItemCost {
                observed: item.estimated_cost,
                max: self.max_item_cost,
            });
        }
        if !self.resident_runs.insert(item.run_id.clone()) {
            return Err(SchedulerError::DuplicateRun(item.run_id));
        }
        let principal = item.principal_id.clone();
        let queue = self.queues.entry(principal.clone()).or_default();
        if queue.is_empty() && !self.order.contains(&principal) {
            self.order.push_back(principal.clone());
        }
        queue.push_back(item);
        self.deficit.entry(principal).or_default();
        self.resident += 1;
        Ok(())
    }

    pub fn pop_next(&mut self) -> Option<AdmissionItem> {
        let rounds = self.order.len();
        for _ in 0..rounds {
            let principal = self.order.pop_front()?;
            let queue = self.queues.get_mut(&principal)?;
            let deficit = self.deficit.entry(principal.clone()).or_default();
            *deficit = deficit
                .saturating_add(u64::from(self.quantum))
                .min(u64::from(self.max_item_cost));
            let runnable_index = queue.iter().position(|item| {
                !self.active_sessions.contains(&item.session_id)
                    && u64::from(item.estimated_cost) <= *deficit
            });
            if let Some(index) = runnable_index {
                let item = queue.remove(index).expect("index came from this queue");
                *deficit -= u64::from(item.estimated_cost);
                self.resident -= 1;
                self.resident_runs.remove(&item.run_id);
                self.active_sessions.insert(item.session_id.clone());
                if queue.is_empty() {
                    self.queues.remove(&principal);
                    self.deficit.remove(&principal);
                } else {
                    self.order.push_back(principal);
                }
                return Some(item);
            }
            self.order.push_back(principal);
        }
        None
    }

    pub fn complete_session(&mut self, session_id: &str) {
        self.active_sessions.remove(session_id);
    }

    pub fn cancel_resident(&mut self, run_id: &str) -> bool {
        let mut removed = false;
        let principals = self.queues.keys().cloned().collect::<Vec<_>>();
        for principal in principals {
            let mut empty = false;
            if let Some(queue) = self.queues.get_mut(&principal)
                && let Some(index) = queue.iter().position(|item| item.run_id == run_id)
            {
                queue.remove(index);
                empty = queue.is_empty();
                removed = true;
            }
            if empty {
                self.queues.remove(&principal);
                self.deficit.remove(&principal);
                self.order.retain(|candidate| candidate != &principal);
            }
            if removed {
                self.resident = self.resident.saturating_sub(1);
                self.resident_runs.remove(run_id);
                break;
            }
        }
        removed
    }

    pub fn resident_len(&self) -> usize {
        self.resident
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum SchedulerError {
    #[error("resident window must be greater than zero")]
    InvalidResidentWindow,
    #[error("quantum and maximum item cost must be positive, with quantum no greater than max")]
    InvalidCostConfiguration,
    #[error("resident claim window is full at {0} items")]
    ResidentWindowFull(usize),
    #[error("estimated cost {observed} must be in 1..={max}")]
    InvalidItemCost { observed: u32, max: u32 },
    #[error("run already exists in the resident window: {0}")]
    DuplicateRun(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAX_FAIR_TAG: u64 = i64::MAX as u64;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum OracleState {
        Queued,
        Deferred,
        Active,
        Complete,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct OracleRun {
        run_id: String,
        tenant_id: String,
        principal_id: String,
        session_id: String,
        state: OracleState,
        queue_start_tag: u64,
        queue_finish_tag: u64,
        available_at: u64,
        created_at: u64,
        lease_deadline: Option<u64>,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum OracleError {
        IdentityConflict,
        TagOverflow,
        InvalidTransition,
    }

    /// Deterministic unit-cost oracle for the `PostgreSQL` start-time fair queue.
    /// This is test-only: queued runs remain database rows in production.
    #[derive(Debug, Default)]
    struct DatabaseFairQueueOracle {
        virtual_start_tag: u64,
        principal_tail: BTreeMap<(String, String), u64>,
        runs: BTreeMap<String, OracleRun>,
        active_sessions: BTreeSet<(String, String)>,
        next_created_at: u64,
    }

    impl DatabaseFairQueueOracle {
        fn enqueue(
            &mut self,
            run_id: &str,
            tenant_id: &str,
            principal_id: &str,
            session_id: &str,
            available_at: u64,
        ) -> Result<bool, OracleError> {
            if let Some(existing) = self.runs.get(run_id) {
                if existing.tenant_id == tenant_id
                    && existing.principal_id == principal_id
                    && existing.session_id == session_id
                    && existing.available_at == available_at
                {
                    return Ok(false);
                }
                return Err(OracleError::IdentityConflict);
            }
            let (queue_start_tag, queue_finish_tag) = self.allocate_tag(tenant_id, principal_id)?;
            let created_at = self.next_created_at;
            self.next_created_at = self
                .next_created_at
                .checked_add(1)
                .ok_or(OracleError::TagOverflow)?;
            self.runs.insert(
                run_id.into(),
                OracleRun {
                    run_id: run_id.into(),
                    tenant_id: tenant_id.into(),
                    principal_id: principal_id.into(),
                    session_id: session_id.into(),
                    state: OracleState::Queued,
                    queue_start_tag,
                    queue_finish_tag,
                    available_at,
                    created_at,
                    lease_deadline: None,
                },
            );
            Ok(true)
        }

        fn claim(&mut self, now: u64) -> Option<(String, bool)> {
            let recovery = self
                .runs
                .values()
                .filter(|run| {
                    run.state == OracleState::Active
                        && run.lease_deadline.is_some_and(|deadline| deadline <= now)
                })
                .min_by_key(|run| (run.lease_deadline, run.created_at, run.run_id.as_str()))
                .map(|run| run.run_id.clone());
            if let Some(run_id) = recovery {
                self.runs
                    .get_mut(&run_id)
                    .expect("recovery candidate exists")
                    .lease_deadline = Some(u64::MAX);
                return Some((run_id, true));
            }

            let selected = self
                .runs
                .values()
                .filter(|run| {
                    matches!(run.state, OracleState::Queued | OracleState::Deferred)
                        && run.available_at <= now
                        && !self
                            .active_sessions
                            .contains(&(run.tenant_id.clone(), run.session_id.clone()))
                })
                .min_by_key(|run| {
                    (
                        run.queue_start_tag,
                        run.queue_finish_tag,
                        run.available_at,
                        run.created_at,
                        run.run_id.as_str(),
                    )
                })
                .map(|run| run.run_id.clone())?;
            let run = self
                .runs
                .get_mut(&selected)
                .expect("claim candidate exists");
            self.virtual_start_tag = self.virtual_start_tag.max(run.queue_start_tag);
            self.active_sessions
                .insert((run.tenant_id.clone(), run.session_id.clone()));
            run.state = OracleState::Active;
            run.lease_deadline = Some(u64::MAX);
            Some((selected, false))
        }

        fn defer(&mut self, run_id: &str, available_at: u64) -> Result<(), OracleError> {
            let (tenant_id, principal_id, session_id) = {
                let run = self
                    .runs
                    .get(run_id)
                    .ok_or(OracleError::InvalidTransition)?;
                if run.state != OracleState::Active {
                    return Err(OracleError::InvalidTransition);
                }
                (
                    run.tenant_id.clone(),
                    run.principal_id.clone(),
                    run.session_id.clone(),
                )
            };
            let (queue_start_tag, queue_finish_tag) =
                self.allocate_tag(&tenant_id, &principal_id)?;
            self.active_sessions.remove(&(tenant_id, session_id));
            let run = self
                .runs
                .get_mut(run_id)
                .expect("validated deferred run exists");
            run.state = OracleState::Deferred;
            run.queue_start_tag = queue_start_tag;
            run.queue_finish_tag = queue_finish_tag;
            run.available_at = available_at;
            run.lease_deadline = None;
            Ok(())
        }

        fn complete(&mut self, run_id: &str) -> Result<(), OracleError> {
            let run = self
                .runs
                .get_mut(run_id)
                .ok_or(OracleError::InvalidTransition)?;
            if run.state != OracleState::Active {
                return Err(OracleError::InvalidTransition);
            }
            self.active_sessions
                .remove(&(run.tenant_id.clone(), run.session_id.clone()));
            run.state = OracleState::Complete;
            run.lease_deadline = None;
            Ok(())
        }

        fn expire(&mut self, run_id: &str, lease_deadline: u64) -> Result<(), OracleError> {
            let run = self
                .runs
                .get_mut(run_id)
                .ok_or(OracleError::InvalidTransition)?;
            if run.state != OracleState::Active {
                return Err(OracleError::InvalidTransition);
            }
            run.lease_deadline = Some(lease_deadline);
            Ok(())
        }

        fn allocate_tag(
            &mut self,
            tenant_id: &str,
            principal_id: &str,
        ) -> Result<(u64, u64), OracleError> {
            let key = (tenant_id.to_owned(), principal_id.to_owned());
            let tail = self.principal_tail.get(&key).copied().unwrap_or(0);
            let start = self.virtual_start_tag.max(tail);
            if start >= MAX_FAIR_TAG {
                return Err(OracleError::TagOverflow);
            }
            let finish = start + 1;
            self.principal_tail.insert(key, finish);
            Ok((start, finish))
        }
    }

    fn item(run: &str, session: &str, principal: &str, cost: u32) -> AdmissionItem {
        AdmissionItem {
            run_id: run.into(),
            session_id: session.into(),
            principal_id: principal.into(),
            estimated_cost: cost,
        }
    }

    #[test]
    fn bounded_window_and_session_fifo_are_enforced() {
        let mut scheduler = FairScheduler::new(3, 2, 8).unwrap();
        scheduler.enqueue(item("a1", "s1", "a", 1)).unwrap();
        scheduler.enqueue(item("a2", "s1", "a", 1)).unwrap();
        scheduler.enqueue(item("b1", "s2", "b", 1)).unwrap();
        assert_eq!(
            scheduler.enqueue(item("c1", "s3", "c", 1)),
            Err(SchedulerError::ResidentWindowFull(3))
        );
        let first = scheduler.pop_next().unwrap();
        assert_eq!(first.run_id, "a1");
        let second = scheduler.pop_next().unwrap();
        assert_eq!(second.run_id, "b1");
        assert!(scheduler.pop_next().is_none());
        scheduler.complete_session("s1");
        assert_eq!(scheduler.pop_next().unwrap().run_id, "a2");
    }

    #[test]
    fn blocked_session_does_not_block_another_session_for_the_same_principal() {
        let mut scheduler = FairScheduler::new(4, 2, 8).unwrap();
        scheduler.enqueue(item("a1", "s1", "a", 1)).unwrap();
        scheduler.enqueue(item("a2", "s1", "a", 1)).unwrap();
        scheduler.enqueue(item("a3", "s2", "a", 1)).unwrap();
        assert_eq!(scheduler.pop_next().unwrap().run_id, "a1");
        assert_eq!(scheduler.pop_next().unwrap().run_id, "a3");
        assert!(scheduler.pop_next().is_none());
    }

    #[test]
    fn duplicate_and_cancelled_resident_runs_do_not_leak_capacity() {
        let mut scheduler = FairScheduler::new(1, 1, 4).unwrap();
        scheduler.enqueue(item("a1", "s1", "a", 1)).unwrap();
        assert_eq!(
            scheduler.enqueue(item("a1", "s2", "a", 1)),
            Err(SchedulerError::ResidentWindowFull(1))
        );
        assert!(scheduler.cancel_resident("a1"));
        assert_eq!(scheduler.resident_len(), 0);
        scheduler.enqueue(item("a2", "s2", "a", 1)).unwrap();
    }

    #[test]
    fn database_oracle_interleaves_100_continuously_eligible_principals() {
        let mut oracle = DatabaseFairQueueOracle::default();
        for principal in 0..100 {
            for round in 0..4 {
                oracle
                    .enqueue(
                        &format!("p{principal:03}-r{round}"),
                        "tenant",
                        &format!("p{principal:03}"),
                        &format!("s{principal:03}-{round}"),
                        0,
                    )
                    .unwrap();
            }
        }
        assert_eq!(oracle.principal_tail.len(), 100);
        assert_eq!(oracle.runs.len(), 400);

        let claimed = (0..400)
            .map(|_| oracle.claim(0).expect("all runs are eligible").0)
            .collect::<Vec<_>>();
        for (round, claims) in claimed.chunks_exact(100).enumerate() {
            let principals = claims
                .iter()
                .map(|run_id| &run_id[..4])
                .collect::<BTreeSet<_>>();
            assert_eq!(principals.len(), 100);
            assert!(
                claims
                    .iter()
                    .all(|run_id| run_id.ends_with(&format!("r{round}")))
            );
        }
    }

    #[test]
    fn one_noisy_principal_cannot_take_a_second_turn_before_quiet_principals() {
        let mut oracle = DatabaseFairQueueOracle::default();
        for index in 0..100 {
            oracle
                .enqueue(
                    &format!("noisy-{index:03}"),
                    "tenant",
                    "noisy",
                    &format!("noisy-session-{index:03}"),
                    0,
                )
                .unwrap();
        }
        for index in 0..99 {
            oracle
                .enqueue(
                    &format!("quiet-{index:03}"),
                    "tenant",
                    &format!("quiet-{index:03}"),
                    &format!("quiet-session-{index:03}"),
                    0,
                )
                .unwrap();
        }

        let first_round = (0..100)
            .map(|_| oracle.claim(0).unwrap().0)
            .collect::<Vec<_>>();
        assert_eq!(
            first_round
                .iter()
                .filter(|run_id| run_id.starts_with("noisy-"))
                .count(),
            1
        );
        assert_eq!(
            first_round
                .iter()
                .filter(|run_id| run_id.starts_with("quiet-"))
                .count(),
            99
        );
    }

    #[test]
    fn late_arrival_joins_at_virtual_time_instead_of_the_noisy_tail() {
        let mut oracle = DatabaseFairQueueOracle::default();
        oracle.enqueue("noisy-0", "t", "noisy", "s0", 0).unwrap();
        oracle.enqueue("noisy-1", "t", "noisy", "s1", 0).unwrap();
        assert_eq!(oracle.claim(0).unwrap().0, "noisy-0");
        oracle.enqueue("late-0", "t", "late", "late-s", 0).unwrap();
        assert_eq!(oracle.runs["late-0"].queue_start_tag, 0);
        assert_eq!(oracle.claim(0).unwrap().0, "late-0");
        assert_eq!(oracle.claim(0).unwrap().0, "noisy-1");
    }

    #[test]
    fn blocked_session_is_skipped_without_blocking_its_principal() {
        let mut oracle = DatabaseFairQueueOracle::default();
        oracle.enqueue("a0", "t", "a", "blocked", 0).unwrap();
        oracle.enqueue("a1", "t", "a", "free", 0).unwrap();
        oracle
            .active_sessions
            .insert(("t".into(), "blocked".into()));
        assert_eq!(oracle.claim(0).unwrap().0, "a1");
        assert_eq!(oracle.runs["a0"].state, OracleState::Queued);
    }

    #[test]
    fn each_deferral_receives_a_fresh_principal_tail_tag() {
        let mut oracle = DatabaseFairQueueOracle::default();
        oracle.enqueue("a0", "t", "a", "s0", 0).unwrap();
        oracle.enqueue("a1", "t", "a", "s1", 0).unwrap();
        oracle.enqueue("a2", "t", "a", "s2", 0).unwrap();
        assert_eq!(oracle.claim(0).unwrap().0, "a0");
        oracle.defer("a0", 0).unwrap();
        assert_eq!(oracle.runs["a0"].queue_start_tag, 3);
        assert_eq!(oracle.claim(0).unwrap().0, "a1");
        oracle.defer("a1", 0).unwrap();
        assert_eq!(oracle.runs["a1"].queue_start_tag, 4);
        assert_eq!(oracle.claim(0).unwrap().0, "a2");
    }

    #[test]
    fn expired_recovery_precedes_fresh_fair_queue_work() {
        let mut oracle = DatabaseFairQueueOracle::default();
        oracle
            .enqueue("active", "t", "old", "old-session", 0)
            .unwrap();
        assert_eq!(oracle.claim(0).unwrap(), ("active".into(), false));
        oracle.expire("active", 5).unwrap();
        oracle
            .enqueue("fresh", "t", "fresh", "fresh-session", 0)
            .unwrap();
        assert_eq!(oracle.claim(10).unwrap(), ("active".into(), true));
        oracle.complete("active").unwrap();
        assert_eq!(oracle.claim(10).unwrap(), ("fresh".into(), false));
    }

    #[test]
    fn replay_preserves_identity_and_tag_overflow_fails_closed() {
        let mut oracle = DatabaseFairQueueOracle::default();
        assert_eq!(oracle.enqueue("run", "t", "p", "s", 0), Ok(true));
        let tail = oracle.principal_tail.clone();
        assert_eq!(oracle.enqueue("run", "t", "p", "s", 0), Ok(false));
        assert_eq!(oracle.principal_tail, tail);
        assert_eq!(
            oracle.enqueue("run", "t", "different", "s", 0),
            Err(OracleError::IdentityConflict)
        );
        oracle.virtual_start_tag = MAX_FAIR_TAG;
        assert_eq!(
            oracle.enqueue("overflow", "t", "new", "new-s", 0),
            Err(OracleError::TagOverflow)
        );
        assert!(!oracle.runs.contains_key("overflow"));
    }
}
