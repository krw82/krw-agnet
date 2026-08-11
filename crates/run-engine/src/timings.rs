//! Bounded, non-authoritative runtime timing counters.
//!
//! These counters intentionally live outside the durable execution state while
//! a run is active.  They are diagnostics only: workflow transitions, leases,
//! and budget admission never depend on them.  Keeping them as atomics lets a
//! provider wrapper and the engine record the same run's local stages without
//! introducing a lock or an unbounded metrics label set.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

#[derive(Debug, Default)]
pub struct RuntimeStageTimings {
    provider_queue_wait_ms: AtomicU64,
    session_memory_total_ms: AtomicU64,
    market_preflight_ms: AtomicU64,
    prompt_build_total_ms: AtomicU64,
    checkpoint_total_ms: AtomicU64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RuntimeStageTimingSnapshot {
    pub provider_queue_wait_ms: u64,
    pub session_memory_total_ms: u64,
    pub market_preflight_ms: u64,
    pub prompt_build_total_ms: u64,
    pub checkpoint_total_ms: u64,
}

fn millis(duration: Duration) -> u64 {
    duration.as_millis().try_into().unwrap_or(u64::MAX)
}

fn add(counter: &AtomicU64, duration: Duration) {
    let amount = millis(duration);
    let _ = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
        Some(current.saturating_add(amount))
    });
}

impl RuntimeStageTimings {
    pub fn add_provider_queue_wait(&self, duration: Duration) {
        add(&self.provider_queue_wait_ms, duration);
    }

    pub fn add_session_memory(&self, duration: Duration) {
        add(&self.session_memory_total_ms, duration);
    }

    pub fn add_market_preflight(&self, duration: Duration) {
        add(&self.market_preflight_ms, duration);
    }

    pub fn add_prompt_build(&self, duration: Duration) {
        add(&self.prompt_build_total_ms, duration);
    }

    pub fn add_checkpoint(&self, duration: Duration) {
        add(&self.checkpoint_total_ms, duration);
    }

    pub fn snapshot(&self) -> RuntimeStageTimingSnapshot {
        RuntimeStageTimingSnapshot {
            provider_queue_wait_ms: self.provider_queue_wait_ms.load(Ordering::Relaxed),
            session_memory_total_ms: self.session_memory_total_ms.load(Ordering::Relaxed),
            market_preflight_ms: self.market_preflight_ms.load(Ordering::Relaxed),
            prompt_build_total_ms: self.prompt_build_total_ms.load(Ordering::Relaxed),
            checkpoint_total_ms: self.checkpoint_total_ms.load(Ordering::Relaxed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_saturate_and_snapshot_without_a_lock() {
        let timings = RuntimeStageTimings::default();
        timings.add_provider_queue_wait(Duration::from_millis(7));
        timings.add_session_memory(Duration::from_millis(11));
        timings.add_market_preflight(Duration::from_millis(13));
        timings.add_prompt_build(Duration::from_millis(17));
        timings.add_checkpoint(Duration::from_millis(19));
        assert_eq!(
            timings.snapshot(),
            RuntimeStageTimingSnapshot {
                provider_queue_wait_ms: 7,
                session_memory_total_ms: 11,
                market_preflight_ms: 13,
                prompt_build_total_ms: 17,
                checkpoint_total_ms: 19,
            }
        );
    }
}
