//! Engine-side presentation-pack synthesis for openbb observation series.
//!
//! The deterministic chart pipeline (`retain_presentation_pack` →
//! `krw_presentation::compile` → the committed answer's visualizations) was
//! designed around servers attaching packs through the private MCP `_meta`
//! channel. The openbb transport never will, so its statement/estimate reads
//! would render no chart even though the run already holds the data and the
//! grounding evidence. This module closes that gap inside the kernel: after
//! an openbb series result is ingested, the same projected records become a
//! best-effort pack whose every point references the ingested evidence
//! record — the grounding filter and every existing bound apply unchanged,
//! and a synthesis defect can only omit a chart, never touch the text
//! answer. Advisory-only doctrine is intact: the pack inherits the
//! observation's evidence id, so the chart can never outrank the run's
//! citation set.

use serde_json::{json, Value};

/// fmp record field → canonical chart metric, in series-priority order. Only
/// field names observed live on the Starter transport (2026-09-07 probes of
/// `equity_fundamental_income` / `equity_fundamental_metrics`) and only
/// metrics the presentation vocabulary allows; anything else is skipped
/// honestly instead of aliased loosely.
const FIELD_METRICS: [(&str, &str); 9] = [
    ("revenue", "revenue"),
    ("net_income_from_continuing_operations", "net_income"),
    ("bottom_line_net_income", "net_income"),
    ("diluted_earnings_per_share", "eps"),
    ("basic_earnings_per_share", "eps"),
    ("operating_income", "operating_income"),
    ("gross_margin", "gross_margin"),
    ("operating_margin", "operating_margin"),
    ("research_and_development_expense", "research_and_development"),
];

const METRIC_LABELS: [(&str, &str); 7] = [
    ("revenue", "Revenue"),
    ("net_income", "Net income"),
    ("eps", "EPS"),
    ("operating_income", "Operating income"),
    ("gross_margin", "Gross margin"),
    ("operating_margin", "Operating margin"),
    ("research_and_development", "R&D"),
];

const MAX_SYNTH_SERIES: usize = 4;
const MAX_SYNTH_POINTS: usize = 12;
const MIN_SYNTH_POINTS: usize = 3;
const MAX_SYNTH_VALUE_ABS: f64 = 1.0e13;

struct SynthPoint {
    period: String,
    period_basis: &'static str,
    sort_key: u64,
    fiscal_year: i64,
    value: f64,
}

/// Build a chart-series-sidecar pack from one ingested openbb result, or
/// `None` when the records do not hold at least one chartable metric with
/// enough period points. `capability_id` gates the estimate-lane alias (the
/// forward-EPS rows carry a bare `mean` field that must never be read as a
/// generic metric on any other lane). `symbol` is the call's physical
/// ticker: the adapter attributes provider content to a trusted ticker only
/// on the price lane, so the other lanes' content carries `ticker: null`
/// even though the read was ticker-scoped.
pub(crate) fn synthesize(
    provider_content: &Value,
    capability_id: &str,
    symbol: Option<&str>,
    evidence_id: &str,
) -> Option<Value> {
    if provider_content.get("format").and_then(Value::as_str) != Some("openbb-series-context/v1")
        || provider_content.get("status").and_then(Value::as_str) != Some("available")
    {
        return None;
    }
    let ticker = symbol
        .or_else(|| provider_content.get("ticker").and_then(Value::as_str))
        .map(str::trim)
        .filter(|ticker| !ticker.is_empty() && ticker.len() <= 32)?
        .to_ascii_uppercase();
    let records = provider_content
        .get("records")
        .and_then(Value::as_array)
        .filter(|records| !records.is_empty())?;

    let lane_fields: Vec<(&str, &str)> = if capability_id == "openbb.forward_eps" {
        vec![("mean", "eps")]
    } else {
        FIELD_METRICS.iter().copied().collect()
    };
    let mut clauses = Vec::new();
    let mut series = Vec::new();
    let mut built_metrics = std::collections::BTreeSet::new();
    for (field, metric) in lane_fields {
        if series.len() >= MAX_SYNTH_SERIES {
            break;
        }
        // Two fmp fields can alias one metric (basic/diluted EPS,
        // continuing/bottom-line net income): keep the first (priority
        // order above) so one pack carries ONE series and ONE clause per
        // metric — duplicates would render as repeated lines.
        if !built_metrics.insert(metric) {
            continue;
        }
        let points = collect_points(records, field, capability_id)?;
        let points = bound_points(points);
        if points.len() < MIN_SYNTH_POINTS {
            continue;
        }
        let period_type = if points.first().is_some_and(|point| point.period_basis == "FY") {
            "annual"
        } else {
            "quarter"
        };
        let currency = records
            .iter()
            .find_map(|record| {
                record
                    .get("reported_currency")
                    .or_else(|| record.get("currency"))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .filter(|currency| currency.len() <= 8);
        let label = METRIC_LABELS
            .iter()
            .find(|(candidate, _)| *candidate == metric)
            .map(|(_, label)| *label)
            .unwrap_or(metric);
        let series_key = format!("{ticker}:{metric}");
        clauses.push(json!({
            "clause_id": format!("openbb_{ticker}_{metric}"),
            "metrics": [metric],
            "tickers": [ticker],
            "metric_scope": "company_total",
            "metric_dimensions": [],
            "calculation_window": "period_over_period",
        }));
        series.push(json!({
            "series_key": series_key,
            "label": label,
            "ticker": ticker,
            "canonical_metric": metric,
            "metric_name": label,
            "basis": "consolidated",
            "duration": if period_type == "annual" { "fy" } else { "quarter" },
            "period_type": period_type,
            "scope": {
                "kind": "company_total",
                "key": ticker,
                "label": ticker,
                "composition_eligible": false,
            },
            "points": points
                .iter()
                .map(|point| {
                    let mut object = json!({
                        "period": point.period,
                        "period_basis": point.period_basis,
                        "period_sort_key": point.sort_key,
                        "fiscal_year": point.fiscal_year,
                        "value": point.value,
                        "object_id": evidence_id,
                    });
                    if let Some(currency) = currency.as_ref() {
                        object["currency"] = json!(currency);
                    }
                    object
                })
                .collect::<Vec<_>>(),
        }));
    }
    if series.is_empty() {
        return None;
    }
    Some(json!({
        "schema_version": 2,
        "mode": "chart_series_sidecar",
        "chart_clauses": clauses,
        "series": series,
    }))
}

/// Extract one metric's points from the projected records. Statement/metrics
/// rows identify their period through `fiscal_year` + `fiscal_period`
/// (FY/Q1..Q4); TTM and undated rows are skipped. The forward-EPS lane is
/// capability-gated: its rows carry a bare `mean` with no fiscal fields, so
/// the calendar year of `date` is the period, and `mean` is never read as a
/// metric on any other lane.
fn collect_points(records: &[Value], field: &str, capability_id: &str) -> Option<Vec<SynthPoint>> {
    let forward_lane = capability_id == "openbb.forward_eps";
    let mut points = Vec::new();
    for record in records {
        let Some(object) = record.as_object() else {
            continue;
        };
        let value = match object.get(field) {
            Some(Value::Number(number)) => number
                .as_f64()
                .filter(|value| value.is_finite() && value.abs() <= MAX_SYNTH_VALUE_ABS)?,
            _ => continue,
        };
        let point = if forward_lane {
            let year = object
                .get("date")
                .and_then(Value::as_str)
                .and_then(|date| date.get(0..4))
                .and_then(|year| year.parse::<i64>().ok())?;
            SynthPoint {
                period: format!("FY{year}"),
                period_basis: "FY",
                sort_key: year.unsigned_abs() * 10,
                fiscal_year: year,
                value,
            }
        } else {
            let year = object.get("fiscal_year").and_then(Value::as_i64)?;
            let fiscal_period = object
                .get("fiscal_period")
                .and_then(Value::as_str)
                .unwrap_or("FY");
            let quarter = match fiscal_period {
                "FY" | "Annual" => 0_u64,
                quarter if quarter.len() == 2
                    && quarter.starts_with('Q')
                    && quarter[1..].parse::<u64>().is_ok_and(|q| (1..=4).contains(&q)) =>
                {
                    quarter[1..].parse::<u64>().expect("validated digit")
                }
                // TTM and exotic period labels are not a chartable axis.
                _ => continue,
            };
            SynthPoint {
                period: if quarter == 0 {
                    format!("FY{year}")
                } else {
                    format!("FY{year}Q{quarter}")
                },
                period_basis: if quarter == 0 { "FY" } else { "Q" },
                sort_key: year.unsigned_abs() * 10 + quarter,
                fiscal_year: year,
                value,
            }
        };
        points.push(point);
    }
    Some(points)
}

/// Deterministic tail selection: one chart axis per series (annual rows win
/// when both annual and quarterly rows exist — the compiler rejects a mixed
/// axis), one point per period, ascending order, at most `MAX_SYNTH_POINTS`
/// points.
fn bound_points(mut points: Vec<SynthPoint>) -> Vec<SynthPoint> {
    if points.iter().any(|point| point.period_basis == "FY") {
        points.retain(|point| point.period_basis == "FY");
    }
    points.sort_by(|a, b| {
        a.sort_key
            .cmp(&b.sort_key)
            .then_with(|| a.period.cmp(&b.period))
    });
    points.dedup_by(|a, b| a.period == b.period);
    if points.len() > MAX_SYNTH_POINTS {
        points.split_off(points.len() - MAX_SYNTH_POINTS);
    }
    points
}

#[cfg(test)]
mod tests {
    use super::*;

    fn income_content() -> Value {
        json!({
            "format": "openbb-series-context/v1",
            "ticker": "SMCI",
            "status": "available",
            "record_count": 4,
            "records": [
                {"fiscal_year": 2023, "fiscal_period": "FY", "revenue": 7123.0,
                 "net_income_from_continuing_operations": 740.0,
                 "diluted_earnings_per_share": 12.55, "reported_currency": "USD"},
                {"fiscal_year": 2024, "fiscal_period": "FY", "revenue": 9884.0,
                 "net_income_from_continuing_operations": 1215.0,
                 "diluted_earnings_per_share": 19.02, "reported_currency": "USD"},
                {"fiscal_year": 2025, "fiscal_period": "FY", "revenue": 14989.0,
                 "net_income_from_continuing_operations": 1334.0,
                 "diluted_earnings_per_share": 21.0, "reported_currency": "USD"},
                {"fiscal_year": 2026, "fiscal_period": "TTM", "revenue": 21000.0,
                 "net_income_from_continuing_operations": 1800.0,
                 "diluted_earnings_per_share": 27.0, "reported_currency": "USD"}
            ]
        })
    }

    #[test]
    fn synthesizes_trend_pack_with_grounded_points() {
        let pack = synthesize(
            &income_content(),
            "openbb.income_statement",
            None,
            "openbb-advisory:x",
        )
        .expect("pack");
        assert_eq!(pack["schema_version"], 2);
        let clauses = pack["chart_clauses"].as_array().expect("clauses");
        let series = pack["series"].as_array().expect("series");
        assert!(!clauses.is_empty() && clauses.len() == series.len());
        let revenue = series
            .iter()
            .find(|item| item["canonical_metric"] == "revenue")
            .expect("revenue series");
        // TTM row must be excluded: three FY points only.
        assert_eq!(revenue["points"].as_array().expect("points").len(), 3);
        assert_eq!(revenue["period_type"], "annual");
        for point in revenue["points"].as_array().expect("points") {
            assert_eq!(point["object_id"], "openbb-advisory:x");
        }
        // The pack compiles into artifacts through the real compiler.
        assert!(
            !krw_presentation::compile(&pack)
                .expect("compiles")
                .is_empty()
        );
    }

    #[test]
    fn ticker_comes_from_the_call_symbol_when_content_ticker_is_unattributed() {
        // Only the price lane gets a trusted ticker in provider content; the
        // statement lanes carry ticker:null and the call's physical symbol is
        // the authoritative scope (live 2026-09-07: a VRT income read
        // produced no pack because the content ticker was null).
        let mut content = income_content();
        content["ticker"] = serde_json::Value::Null;
        let pack =
            synthesize(&content, "openbb.income_statement", Some("vrt"), "e").expect("pack");
        assert_eq!(pack["series"][0]["ticker"], "VRT");
        assert_eq!(pack["chart_clauses"][0]["tickers"][0], "VRT");
        assert!(
            synthesize(&content, "openbb.income_statement", None, "e").is_none(),
            "without either ticker source there is no chartable scope"
        );
    }

    #[test]
    fn skips_when_no_chartable_metric_or_shape() {
        let mut content = income_content();
        content["status"] = json!("no_data");
        assert!(synthesize(&content, "openbb.income_statement", None, "e").is_none());
        let two_rows = json!({
            "format": "openbb-series-context/v1",
            "ticker": "SMCI",
            "status": "available",
            "records": [
                {"fiscal_year": 2025, "fiscal_period": "FY", "revenue": 1.0},
                {"fiscal_year": 2024, "fiscal_period": "FY", "revenue": 2.0}
            ]
        });
        assert!(synthesize(&two_rows, "openbb.income_statement", None, "e").is_none());
    }

    #[test]
    fn forward_eps_lane_reads_mean_by_date_year_only_on_that_lane() {
        let content = json!({
            "format": "openbb-series-context/v1",
            "ticker": "MU",
            "status": "available",
            "records": [
                {"date": "2026-12-31", "mean": 6.72, "symbol": "MU"},
                {"date": "2027-12-31", "mean": 9.14, "symbol": "MU"},
                {"date": "2028-12-31", "mean": 11.56, "symbol": "MU"}
            ]
        });
        // The `mean` field is not in the statement alias table: it is read
        // only when the capability is the forward-EPS lane.
        assert!(
            synthesize(&content, "openbb.income_statement", None, "e").is_none(),
            "mean must not be aliased on non-forward lanes"
        );
        let pack = synthesize(&content, "openbb.forward_eps", None, "e").expect("gated pack");
        let eps = &pack["series"][0];
        assert_eq!(eps["canonical_metric"], "eps");
        assert_eq!(eps["points"].as_array().expect("points").len(), 3);
        assert_eq!(eps["points"][0]["period"], "FY2026");
        assert!(
            !krw_presentation::compile(&pack)
                .expect("compiles")
                .is_empty()
        );
    }

    #[test]
    fn dedupes_duplicate_periods_keeping_single_axis() {
        let content = json!({
            "format": "openbb-series-context/v1",
            "ticker": "X",
            "status": "available",
            "records": [
                {"fiscal_year": 2024, "fiscal_period": "FY", "revenue": 1.0},
                {"fiscal_year": 2024, "fiscal_period": "Q4", "revenue": 4.0},
                {"fiscal_year": 2025, "fiscal_period": "FY", "revenue": 2.0},
                {"fiscal_year": 2026, "fiscal_period": "FY", "revenue": 3.0}
            ]
        });
        let pack = synthesize(&content, "openbb.income_statement", None, "e").expect("pack");
        let revenue = &pack["series"][0];
        // Annual rows win over the stray quarterly row: one axis per series.
        let periods: Vec<&str> = revenue["points"]
            .as_array()
            .expect("points")
            .iter()
            .map(|point| point["period"].as_str().expect("period"))
            .collect();
        assert_eq!(periods, vec!["FY2024", "FY2025", "FY2026"]);
    }
}
