//! `krw-agent-deploy` — deployment controller (doc/refetoring/05).
//!
//! This crate implements the honest fail-closed core of the deployment
//! controller: production config parsing, the ordered read-only preflight
//! check registry, immutable receipts, and the forward-only stage machine.
//! Stages that would mutate production (build, seal, migrations, admission
//! transitions, activation) are declared but intentionally not implemented in
//! this revision: reaching them in a live `deploy` writes a terminal failure
//! receipt with admission recorded as closed and exits non-zero. The
//! controller never half-activates and never rolls back.

pub mod checks;
pub mod config;
pub mod contract;
pub mod executor;
pub mod hashing;
pub mod receipts;
pub mod stages;
pub mod target;
pub mod timeutil;
