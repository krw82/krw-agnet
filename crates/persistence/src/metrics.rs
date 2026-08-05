//! Lightweight Prometheus metrics owned by the daemon's run supervisor.
//!
//! These globals are intentionally minimal — they give a `/metrics` scrape
//! endpoint enough signal to observe active runs, run outcomes, and capability
//! dispatch. The registry and collectors are lazily initialized once via
//! [`OnceLock`] and shared between the supervisor (which records them) and the
//! daemon binary (which serves them).

use std::sync::OnceLock;

use prometheus::{HistogramVec, IntCounterVec, IntGauge, Registry};

/// Terminal outcome label used for the run counter and duration histogram.
pub const OUTCOME_FINAL: &str = "final";
/// Terminal cancel label.
pub const OUTCOME_CANCELLED: &str = "cancelled";
/// Terminal failure label.
pub const OUTCOME_FAILED: &str = "failed";

static REGISTRY: OnceLock<Registry> = OnceLock::new();
static RUNS_TOTAL: OnceLock<IntCounterVec> = OnceLock::new();
static RUN_DURATION: OnceLock<HistogramVec> = OnceLock::new();
static ACTIVE_RUNS: OnceLock<IntGauge> = OnceLock::new();
static CAPABILITY_CALLS_TOTAL: OnceLock<IntCounterVec> = OnceLock::new();
static CAPABILITY_DURATION: OnceLock<HistogramVec> = OnceLock::new();
static PROVIDER_TURN_DURATION: OnceLock<HistogramVec> = OnceLock::new();

/// Lazily initialize and return the shared Prometheus [`Registry`].
///
/// Safe to call from any thread; the registry is created on first use and
/// reused thereafter. The daemon binary gathers metrics from this registry to
/// serve its `/metrics` endpoint.
pub fn registry() -> &'static Registry {
    REGISTRY.get_or_init(|| {
        let registry = Registry::new();

        let runs = IntCounterVec::new(
            prometheus::Opts::new("krw_runs_total", "Total claimed runs by terminal outcome"),
            &["outcome"],
        )
        .expect("krw_runs_total collector is unique");

        let duration = HistogramVec::new(
            prometheus::HistogramOpts::new(
                "krw_run_duration_seconds",
                "Wall-clock duration of a claimed run by outcome",
            )
            .buckets(vec![30.0, 60.0, 90.0, 120.0, 180.0, 240.0, 300.0]),
            &["outcome"],
        )
        .expect("krw_run_duration_seconds collector is unique");

        let active = IntGauge::new(
            "krw_active_runs",
            "Number of claimed runs currently being driven by this daemon",
        )
        .expect("krw_active_runs collector is unique");

        let cap_calls = IntCounterVec::new(
            prometheus::Opts::new(
                "krw_capability_calls_total",
                "Capability invocations dispatched by the run engine",
            ),
            &["capability_id", "outcome"],
        )
        .expect("krw_capability_calls_total collector is unique");

        // Finer buckets than `krw_run_duration_seconds` because per-call
        // latency lives in the tens-of-ms to single-digit-seconds range.
        let cap_duration = HistogramVec::new(
            prometheus::HistogramOpts::new(
                "krw_capability_duration_seconds",
                "Wall-clock duration of a single capability dispatch by outcome",
            )
            .buckets(vec![0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0]),
            &["capability_id", "outcome"],
        )
        .expect("krw_capability_duration_seconds collector is unique");

        let provider_turn_duration = HistogramVec::new(
            prometheus::HistogramOpts::new(
                "krw_provider_turn_duration_seconds",
                "Wall-clock duration of a single provider turn (Provider::complete)",
            )
            .buckets(vec![0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0]),
            &[],
        )
        .expect("krw_provider_turn_duration_seconds collector is unique");

        registry
            .register(Box::new(runs.clone()))
            .expect("krw_runs_total registers");
        registry
            .register(Box::new(duration.clone()))
            .expect("krw_run_duration_seconds registers");
        registry
            .register(Box::new(active.clone()))
            .expect("krw_active_runs registers");
        registry
            .register(Box::new(cap_calls.clone()))
            .expect("krw_capability_calls_total registers");
        registry
            .register(Box::new(cap_duration.clone()))
            .expect("krw_capability_duration_seconds registers");
        registry
            .register(Box::new(provider_turn_duration.clone()))
            .expect("krw_provider_turn_duration_seconds registers");

        let _ = RUNS_TOTAL.set(runs);
        let _ = RUN_DURATION.set(duration);
        let _ = ACTIVE_RUNS.set(active);
        let _ = CAPABILITY_CALLS_TOTAL.set(cap_calls);
        let _ = CAPABILITY_DURATION.set(cap_duration);
        let _ = PROVIDER_TURN_DURATION.set(provider_turn_duration);

        registry
    })
}

fn runs_total() -> &'static IntCounterVec {
    RUNS_TOTAL.get().expect("registry initializes RUNS_TOTAL")
}

fn run_duration() -> &'static HistogramVec {
    RUN_DURATION
        .get()
        .expect("registry initializes RUN_DURATION")
}

fn active_runs() -> &'static IntGauge {
    ACTIVE_RUNS.get().expect("registry initializes ACTIVE_RUNS")
}

/// Record that a claimed run has begun executing. Increments the active gauge.
pub fn record_run_started() {
    // Ensure the registry is initialized before any recording so the gauge
    // exists even if /metrics has not yet been scraped.
    let _ = registry();
    active_runs().inc();
}

/// Record a terminal run outcome and decrement the active gauge.
///
/// `elapsed` is the wall-clock duration the run spent executing under this
/// daemon. `outcome` should be one of [`OUTCOME_FINAL`], [`OUTCOME_CANCELLED`],
/// or [`OUTCOME_FAILED`].
pub fn record_run_outcome(outcome: &str, elapsed: std::time::Duration) {
    let _ = registry();
    active_runs().dec();
    runs_total().with_label_values(&[outcome]).inc();
    run_duration()
        .with_label_values(&[outcome])
        .observe(elapsed.as_secs_f64());
}

/// Record a capability dispatch outcome.
///
/// `capability_id` is the image-scoped capability identifier and `outcome` is
/// `"success"` or `"error"`.
pub fn record_capability_call(capability_id: &str, outcome: &str) {
    let _ = registry();
    CAPABILITY_CALLS_TOTAL
        .get()
        .expect("registry initializes CAPABILITY_CALLS_TOTAL")
        .with_label_values(&[capability_id, outcome])
        .inc();
}

/// Record the wall-clock duration of a single capability dispatch.
///
/// `ms` is the elapsed time in milliseconds spent inside the capability
/// invocation (including any MCP round trip). It is converted to seconds
/// (as a float) for the Prometheus histogram observation. Observe at the same
/// call site as [`record_capability_call`].
pub fn record_capability_duration_seconds(capability_id: &str, outcome: &str, ms: u64) {
    let _ = registry();
    CAPABILITY_DURATION
        .get()
        .expect("registry initializes CAPABILITY_DURATION")
        .with_label_values(&[capability_id, outcome])
        .observe(milliseconds_to_seconds(ms));
}

/// Record the wall-clock duration of a single provider turn (the
/// `Provider::complete` future). `ms` is the elapsed time in milliseconds.
pub fn record_provider_turn_duration_seconds(ms: u64) {
    let _ = registry();
    PROVIDER_TURN_DURATION
        .get()
        .expect("registry initializes PROVIDER_TURN_DURATION")
        .with_label_values(&[])
        .observe(milliseconds_to_seconds(ms));
}

/// Convert a millisecond `u64` measurement into seconds for histogram
/// observation. Sub-millisecond precision is irrelevant for the bucket layout
/// in use here, so the clippy `cast_precision_loss` lint is intentionally
/// allowed: a wall-clock measurement of, say, 2^53 ms (~285k years) is not a
/// meaningful runtime input.
#[allow(clippy::cast_precision_loss)]
fn milliseconds_to_seconds(ms: u64) -> f64 {
    ms as f64 / 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use prometheus::TextEncoder;

    #[test]
    fn registry_is_idempotent_and_records_lifecycle() {
        let first = registry();
        let second = registry();
        assert!(std::ptr::eq(first, second));

        record_run_started();
        record_run_started();
        record_run_outcome(OUTCOME_FINAL, std::time::Duration::from_secs(45));
        record_run_outcome(OUTCOME_FAILED, std::time::Duration::from_secs(10));
        record_capability_call("ontology.query", "success");
        record_capability_call("ontology.query", "error");
        record_capability_duration_seconds("ontology.query", "success", 250);
        record_provider_turn_duration_seconds(1_200);

        let metric_families = first.gather();
        let mut buffer = String::new();
        let encoder = TextEncoder::new();
        encoder.encode_utf8(&metric_families, &mut buffer).unwrap();
        assert!(buffer.contains("krw_active_runs"));
        assert!(buffer.contains("krw_runs_total"));
        assert!(buffer.contains("krw_run_duration_seconds"));
        assert!(buffer.contains("krw_capability_calls_total"));
        assert!(buffer.contains("krw_capability_duration_seconds"));
        assert!(buffer.contains("krw_provider_turn_duration_seconds"));
        assert!(buffer.contains("ontology.query"));
    }
}
