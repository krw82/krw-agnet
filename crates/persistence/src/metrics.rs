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
/// Targeted-query attribution label: the model copied the advertised
/// candidate topic exactly.
pub const ATTRIBUTION_VERBATIM: &str = "verbatim";
/// Targeted-query attribution label: the model's topic was a token-subset
/// rewrite of exactly one advertised candidate.
pub const ATTRIBUTION_CANONICALIZED: &str = "canonicalized";
/// Targeted-query attribution label: candidates were advertised for the
/// queried ticker's missing clauses but the call matched none of them.
pub const ATTRIBUTION_UNMATCHED: &str = "unmatched";

static REGISTRY: OnceLock<Registry> = OnceLock::new();
static RUNS_TOTAL: OnceLock<IntCounterVec> = OnceLock::new();
static RUN_DURATION: OnceLock<HistogramVec> = OnceLock::new();
static ACTIVE_RUNS: OnceLock<IntGauge> = OnceLock::new();
static CAPABILITY_CALLS_TOTAL: OnceLock<IntCounterVec> = OnceLock::new();
static CAPABILITY_DURATION: OnceLock<HistogramVec> = OnceLock::new();
static PROVIDER_TURN_DURATION: OnceLock<HistogramVec> = OnceLock::new();
static TARGETED_QUERY_ATTRIBUTION_TOTAL: OnceLock<IntCounterVec> = OnceLock::new();

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

        // Copy-verbatim measurement signal for targeted queries: how often a
        // model-authored `ontology.query` is an exact copy of the advertised
        // candidate, a paraphrase the kernel canonicalized back, or a call
        // that matched no advertised candidate. The only label is the closed
        // outcome string, so cardinality is fixed at three.
        let targeted_query_attribution = IntCounterVec::new(
            prometheus::Opts::new(
                "krw_targeted_query_attribution_total",
                "Model-authored targeted queries attributed to advertised exact candidates",
            ),
            &["outcome"],
        )
        .expect("krw_targeted_query_attribution_total collector is unique");

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
        registry
            .register(Box::new(targeted_query_attribution.clone()))
            .expect("krw_targeted_query_attribution_total registers");

        let _ = RUNS_TOTAL.set(runs);
        let _ = RUN_DURATION.set(duration);
        let _ = ACTIVE_RUNS.set(active);
        let _ = CAPABILITY_CALLS_TOTAL.set(cap_calls);
        let _ = CAPABILITY_DURATION.set(cap_duration);
        let _ = PROVIDER_TURN_DURATION.set(provider_turn_duration);
        let _ = TARGETED_QUERY_ATTRIBUTION_TOTAL.set(targeted_query_attribution);

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

/// Record one attribution outcome for a model-authored targeted query.
///
/// `outcome` must be one of the closed label set [`ATTRIBUTION_VERBATIM`],
/// [`ATTRIBUTION_CANONICALIZED`], or [`ATTRIBUTION_UNMATCHED`]. The only
/// caller (run-engine targeted-query canonicalization) derives the value
/// from a closed enum, so the exposed label cardinality is fixed at three
/// regardless of tickers, clauses, or topics.
pub fn record_targeted_query_attribution(outcome: &str) {
    let _ = registry();
    TARGETED_QUERY_ATTRIBUTION_TOTAL
        .get()
        .expect("registry initializes TARGETED_QUERY_ATTRIBUTION_TOTAL")
        .with_label_values(&[outcome])
        .inc();
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
    fn targeted_query_attribution_counter_records_the_closed_outcome_set() {
        // Counters are process-global and cumulative, so the exactness
        // assertion is a before/after delta. This is the only test in the
        // crate that touches the attribution counter, keeping the delta
        // deterministic under the default parallel test runner.
        let _ = registry();
        let counter = TARGETED_QUERY_ATTRIBUTION_TOTAL
            .get()
            .expect("registry initializes TARGETED_QUERY_ATTRIBUTION_TOTAL");
        let before = [
            counter
                .with_label_values(&[ATTRIBUTION_VERBATIM])
                .get(),
            counter
                .with_label_values(&[ATTRIBUTION_CANONICALIZED])
                .get(),
            counter
                .with_label_values(&[ATTRIBUTION_UNMATCHED])
                .get(),
        ];

        record_targeted_query_attribution(ATTRIBUTION_VERBATIM);
        record_targeted_query_attribution(ATTRIBUTION_CANONICALIZED);
        record_targeted_query_attribution(ATTRIBUTION_UNMATCHED);

        assert_eq!(
            counter
                .with_label_values(&[ATTRIBUTION_VERBATIM])
                .get(),
            before[0] + 1,
            "one verbatim attribution increments exactly once"
        );
        assert_eq!(
            counter
                .with_label_values(&[ATTRIBUTION_CANONICALIZED])
                .get(),
            before[1] + 1,
            "one canonicalized attribution increments exactly once"
        );
        assert_eq!(
            counter
                .with_label_values(&[ATTRIBUTION_UNMATCHED])
                .get(),
            before[2] + 1,
            "one unmatched attribution increments exactly once"
        );

        // The exposed label space is exactly the three closed outcome
        // strings: no capability ids, tickers, or topics ride along.
        let family = registry()
            .gather()
            .into_iter()
            .find(|family| family.get_name() == "krw_targeted_query_attribution_total")
            .expect("attribution counter family is registered");
        let mut outcomes = family
            .get_metric()
            .iter()
            .map(|metric| {
                assert_eq!(metric.get_label().len(), 1, "outcome is the only label");
                metric
                    .get_label()
                    .iter()
                    .map(|pair| pair.get_value().to_owned())
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .collect::<Vec<_>>();
        outcomes.sort_unstable();
        assert_eq!(outcomes, vec!["canonicalized", "unmatched", "verbatim"]);
    }

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
