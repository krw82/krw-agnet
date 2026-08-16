//! `krw-agent-deploy` — deployment controller (doc/refetoring/05).
//!
//! This crate implements the deployment controller: production config
//! parsing, the ordered read-only preflight check registry, immutable
//! receipts (preflight, per-stage, terminal), and the forward-only stage
//! machine for all twelve 05-doc stages. Mutating stages (build, seal,
//! frontend image prepare, migrations, admission transitions, activation,
//! deep readiness) execute strictly through the [`pipeline::StageExecutor`]
//! trait so every world command is a structured, assertable record. Any
//! stage failure writes a terminal failure receipt with admission recorded
//! closed (from `admission_close` onward) and exits non-zero; the
//! controller never rolls back binaries or DB schema — recovery is always
//! fix-forward through a fresh run.

pub mod checks;
pub mod config;
pub mod contract;
pub mod executor;
pub mod hashing;
pub mod pipeline;
pub mod receipts;
pub mod stages;
pub mod target;
pub mod timeutil;
