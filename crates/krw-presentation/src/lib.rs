//! Deterministic visualization compiler for the KRW presentation channel.
//!
//! The ontology capability delivers a verified `PresentationSeriesPackV2`
//! (chart-series sidecar) through the private MCP `_meta` channel. This crate
//! turns that pack into `KrwVisualizationArtifact` JSON consumed verbatim by
//! the frontend renderer (`krw-visualization` format, schema version 4).
//!
//! Chart-worthiness is decided here from the secured data alone — never from
//! question keywords and never by asking the model. Every derived number
//! (period-over-period growth) is computed in Rust and anchored to the
//! evidence object ids that produced it. A pack that cannot support a chart
//! simply yields no artifacts; the text answer is unaffected.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

/// Private vendor channel on the tools/call envelope that carries the pack.
pub const PRESENTATION_META_KEY: &str = "com.krwontology/presentationSeries";
/// Frontend artifact format identifier (`KRW_VISUALIZATION_ARTIFACT_FORMAT`).
pub const ARTIFACT_FORMAT: &str = "krw-visualization";
/// Frontend artifact schema version (`KRW_VISUALIZATION_SCHEMA_VERSION`).
pub const ARTIFACT_SCHEMA_VERSION: u32 = 4;

const MAX_ARTIFACTS_PER_PACK: usize = 3;
const MIN_TREND_POINTS: usize = 3;
const MIN_COMPARISON_SERIES: usize = 2;

/// Compile every artifact the pack can deterministically support.
///
/// Returns `Err` only when the pack is structurally unusable (the capture
/// boundary already bounds its size); an insufficient pack is `Ok(vec![])`.
pub fn compile(pack: &Value) -> Result<Vec<Value>, CompileError> {
    if pack.get("schema_version").and_then(Value::as_u64) != Some(2) {
        return Err(CompileError::InvalidPack);
    }
    if pack
        .get("chart_clauses")
        .and_then(Value::as_array)
        .map_or(true, Vec::is_empty)
    {
        return Ok(Vec::new());
    }
    let clauses = parse_chart_clauses(pack)?;
    if clauses.is_empty() {
        return Ok(Vec::new());
    }
    let series = parse_series(pack)?;
    if series.is_empty() {
        return Ok(Vec::new());
    }
    let mut groups: BTreeMap<GroupKey, Vec<PackSeries>> = BTreeMap::new();
    for item in series {
        let raw_metric = item
            .canonical_metric
            .clone()
            .or_else(|| item.metric_name.clone())
            .unwrap_or_default();
        let metric = chart_metric_key(&raw_metric).unwrap_or(raw_metric);
        for clause in clauses.iter().filter(|clause| {
            clause.metrics.contains(&metric)
                && item
                    .ticker
                    .as_ref()
                    .is_some_and(|ticker| clause.tickers.contains(ticker))
                && series_matches_clause(&item, clause)
        }) {
            let key = GroupKey {
                clause_id: clause.clause_id.clone(),
                metric: if metric.is_empty() {
                    item.series_key.clone()
                } else {
                    metric.clone()
                },
                unit: item.unit.clone().unwrap_or_default(),
                basis: item.basis.clone().unwrap_or_default(),
                currency: item.currency.clone().unwrap_or_default(),
                duration: item.duration.clone().unwrap_or_default(),
                period_type: item.period_type.clone().unwrap_or_default(),
                period_basis: item.period_basis.clone().unwrap_or_default(),
                period_axis: item.period_axis.clone(),
                chart_kind: clause.chart_kind.clone(),
            };
            groups.entry(key).or_default().push(item.clone());
        }
    }
    let mut artifacts = Vec::new();
    for (key, mut group) in groups {
        group.sort_by(|a, b| {
            a.scope_key
                .cmp(&b.scope_key)
                .then_with(|| a.ticker.cmp(&b.ticker))
        });
        if let Some(artifact) = compile_group(&key, &group) {
            artifacts.push(artifact);
            if artifacts.len() == MAX_ARTIFACTS_PER_PACK {
                break;
            }
        }
    }
    Ok(artifacts)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompileError {
    /// The pack is not a presentation-series pack this compiler understands.
    InvalidPack,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct GroupKey {
    clause_id: String,
    metric: String,
    unit: String,
    basis: String,
    currency: String,
    duration: String,
    period_type: String,
    period_basis: String,
    period_axis: String,
    chart_kind: String,
}

struct ChartClause {
    clause_id: String,
    chart_kind: String,
    metrics: BTreeSet<String>,
    tickers: BTreeSet<String>,
    metric_scope: String,
    metric_dimensions: BTreeSet<String>,
}

fn parse_chart_clauses(pack: &Value) -> Result<Vec<ChartClause>, CompileError> {
    let Some(clauses) = pack.get("chart_clauses").and_then(Value::as_array) else {
        return Err(CompileError::InvalidPack);
    };
    let mut parsed = Vec::new();
    for item in clauses {
        let Some(object) = item.as_object() else {
            continue;
        };
        let Some(clause_id) = text(object, "clause_id") else {
            continue;
        };
        let metrics = object
            .get("metrics")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter_map(chart_metric_key)
            .collect::<BTreeSet<_>>();
        if metrics.is_empty() {
            continue;
        }
        let tickers = object
            .get("tickers")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(|ticker| ticker.trim().to_ascii_uppercase())
            .filter(|ticker| !ticker.is_empty())
            .collect::<BTreeSet<_>>();
        if tickers.is_empty() {
            continue;
        }
        let metric_dimensions = object
            .get("metric_dimensions")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|dimension| !dimension.is_empty())
            .map(str::to_owned)
            .collect::<BTreeSet<_>>();
        let metric_scope = text(object, "metric_scope").unwrap_or_else(|| "company_total".into());
        let window = text(object, "calculation_window");
        let has_window = window
            .as_deref()
            .is_some_and(|window| matches!(window, "period_over_period" | "year_over_year"));
        let dimensioned = metric_scope == "dimensioned" || metric_scope == "any";
        let chart_kind = if has_window {
            "trend"
        } else if dimensioned && !metric_dimensions.is_empty() && tickers.len() == 1 {
            "composition"
        } else if tickers.len() >= MIN_COMPARISON_SERIES {
            "comparison"
        } else {
            continue;
        };
        parsed.push(ChartClause {
            clause_id,
            chart_kind: chart_kind.into(),
            metrics,
            tickers,
            metric_scope,
            metric_dimensions,
        });
    }
    parsed.sort_by(|left, right| {
        left.chart_kind
            .cmp(&right.chart_kind)
            .then_with(|| left.clause_id.cmp(&right.clause_id))
    });
    Ok(parsed)
}

fn chart_metric_allowed(metric: &str) -> bool {
    matches!(
        metric,
        "adjusted_free_cash_flow"
            | "capital_expenditures"
            | "capex"
            | "cash_and_cash_equivalents"
            | "cash_and_equivalents"
            | "eps"
            | "free_cash_flow"
            | "gross_margin"
            | "ma_cash_outflow"
            | "ma_related_costs"
            | "net_income"
            | "net_sales"
            | "operating_cash_flow"
            | "operating_income"
            | "operating_margin"
            | "research_and_development"
            | "selling_general_and_admin"
            | "revenue"
            | "share_repurchase"
            | "stock_based_compensation"
            | "total_assets"
            | "total_debt"
            | "total_liabilities"
    )
}

fn chart_metric_key(metric: &str) -> Option<String> {
    let raw = metric.trim().to_ascii_lowercase();
    let normalized = match raw.as_str() {
        "capex" => "capital_expenditures".to_owned(),
        "cash_and_equivalents" => "cash_and_cash_equivalents".to_owned(),
        _ => raw,
    };
    chart_metric_allowed(&normalized).then_some(normalized)
}

#[derive(Debug, Clone)]
struct PackSeries {
    series_key: String,
    label: String,
    ticker: Option<String>,
    metric_name: Option<String>,
    canonical_metric: Option<String>,
    unit: Option<String>,
    currency: Option<String>,
    basis: Option<String>,
    duration: Option<String>,
    period_type: Option<String>,
    period_basis: Option<String>,
    period_axis: String,
    scope_kind: String,
    scope_key: String,
    scope_label: Option<String>,
    scope_dimension: Option<String>,
    composition_eligible: bool,
    points: Vec<PackPoint>,
}

#[derive(Debug, Clone)]
struct PackPoint {
    period: String,
    period_axis: String,
    period_basis: Option<String>,
    period_sort_key: i64,
    fiscal_year: Option<i64>,
    fiscal_quarter: Option<i64>,
    period_start: Option<String>,
    period_end: Option<String>,
    value: f64,
    formatted_value: Option<String>,
    currency: Option<String>,
    object_id: String,
}

fn parse_series(pack: &Value) -> Result<Vec<PackSeries>, CompileError> {
    let Some(series) = pack.get("series").and_then(Value::as_array) else {
        return Err(CompileError::InvalidPack);
    };
    let mut parsed = Vec::new();
    for item in series {
        let Some(object) = item.as_object() else {
            continue;
        };
        let Some(series_key) = text(object, "series_key") else {
            continue;
        };
        let label = text(object, "label").unwrap_or_else(|| series_key.clone());
        let scope = object.get("scope").and_then(Value::as_object);
        let scope_kind = scope
            .and_then(|scope| text(scope, "kind"))
            .unwrap_or_else(|| "unspecified".into());
        let scope_key = scope
            .and_then(|scope| text(scope, "key"))
            .unwrap_or_else(|| series_key.clone());
        let scope_label = scope.and_then(|scope| text(scope, "label"));
        let scope_dimension = scope.and_then(|scope| text(scope, "dimension"));
        let composition_eligible = scope
            .and_then(|scope| scope.get("composition_eligible"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let mut points = Vec::new();
        for point in object
            .get("points")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(point_object) = point.as_object() else {
                continue;
            };
            let Some(period) = text(point_object, "period") else {
                continue;
            };
            let Some(object_id) = text(point_object, "object_id") else {
                continue;
            };
            let Some(value) = finite_number(point_object.get("value")) else {
                continue;
            };
            let Some((period_axis, period_year, period_quarter)) = parse_period_components(&period)
            else {
                // A point without a canonical FY/CY axis cannot be safely
                // ordered or compared. Keep the text answer intact, but do
                // not invent an axis position for a chart.
                continue;
            };
            let explicit_year = point_object.get("fiscal_year").and_then(Value::as_i64);
            let explicit_quarter = point_object.get("fiscal_quarter").and_then(Value::as_i64);
            if explicit_year.is_some_and(|year| year != period_year)
                || explicit_quarter.is_some_and(|quarter| quarter != period_quarter)
            {
                // The textual period is the canonical axis. Do not let
                // contradictory metadata move an observation to another FY/CY.
                continue;
            }
            let fiscal_quarter = (period_quarter > 0).then_some(period_quarter);
            let canonical_period_sort_key = period_year * 10 + period_quarter;
            if let Some(explicit_period_sort_key) = point_object
                .get("period_sort_key")
                .and_then(Value::as_i64)
                .filter(|value| *value > 0)
                && explicit_period_sort_key != canonical_period_sort_key
            {
                // The wire cannot override the period semantics with a
                // conflicting axis number.
                continue;
            }
            let period_sort_key = canonical_period_sort_key;
            points.push(PackPoint {
                period,
                period_axis,
                period_basis: text(point_object, "period_basis"),
                period_sort_key,
                fiscal_year: Some(period_year),
                fiscal_quarter,
                period_start: text(point_object, "period_start"),
                period_end: text(point_object, "period_end"),
                value,
                formatted_value: text(point_object, "formatted_value"),
                currency: text(point_object, "currency").or_else(|| text(object, "currency")),
                object_id,
            });
        }
        points.sort_by(|a, b| {
            a.period_sort_key
                .cmp(&b.period_sort_key)
                .then_with(|| a.period.cmp(&b.period))
        });
        let mut deduped = Vec::with_capacity(points.len());
        for point in points {
            if let Some(previous) = deduped
                .iter()
                .find(|item: &&PackPoint| item.period == point.period)
            {
                if previous.object_id != point.object_id || previous.value != point.value {
                    // Conflicting observations for one axis are unsafe to
                    // visualize. The text answer can still use the source.
                    deduped.clear();
                    break;
                }
                continue;
            }
            deduped.push(point);
        }
        points = deduped;
        if points.is_empty() {
            continue;
        }
        let series_period_axis = points.first().map(|point| point.period_axis.clone());
        if points
            .iter()
            .any(|point| Some(point.period_axis.clone()) != series_period_axis)
        {
            continue;
        }
        let series_period_basis = points.first().and_then(|point| point.period_basis.clone());
        if points
            .iter()
            .any(|point| point.period_basis != series_period_basis)
        {
            // FY and CY observations are not interchangeable axes. Keep the
            // text answer intact but refuse to chart a mixed series.
            continue;
        }
        let series_currency = text(object, "currency")
            .or_else(|| points.iter().find_map(|point| point.currency.clone()));
        if points.iter().any(|point| {
            point.currency.is_some()
                && series_currency.is_some()
                && point.currency != series_currency
        }) {
            continue;
        }
        parsed.push(PackSeries {
            series_key,
            label,
            ticker: text(object, "ticker").map(|ticker| ticker.to_ascii_uppercase()),
            metric_name: text(object, "metric_name"),
            canonical_metric: text(object, "canonical_metric"),
            unit: text(object, "unit"),
            currency: series_currency,
            basis: text(object, "basis"),
            duration: text(object, "duration"),
            period_type: text(object, "period_type"),
            period_basis: series_period_basis,
            period_axis: series_period_axis.unwrap_or_else(|| "".into()),
            scope_kind,
            scope_key,
            scope_label,
            scope_dimension,
            composition_eligible,
            points,
        });
    }
    Ok(parsed)
}

fn text(object: &Map<String, Value>, key: &str) -> Option<String> {
    object
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn finite_number(value: Option<&Value>) -> Option<f64> {
    let number = value?.as_f64()?;
    if number.is_finite() {
        Some(number)
    } else {
        None
    }
}

fn parse_period_components(period: &str) -> Option<(String, i64, i64)> {
    let upper = period.trim().to_ascii_uppercase();
    let (axis, digits) = if let Some(digits) = upper.strip_prefix("FY") {
        ("FY", digits)
    } else if let Some(digits) = upper.strip_prefix("CY") {
        ("CY", digits)
    } else {
        return None;
    };
    let (year, quarter) = if let Some((year, quarter)) = digits.split_once('Q') {
        (year.parse::<i64>().ok()?, quarter.parse::<i64>().ok()?)
    } else {
        (digits.parse::<i64>().ok()?, 0)
    };
    if !(1900..=2200).contains(&year) || !(0..=4).contains(&quarter) {
        return None;
    }
    Some((axis.to_owned(), year, quarter))
}

fn parse_period_sort_key(period: &str) -> Option<i64> {
    let (_, year, quarter) = parse_period_components(period)?;
    Some(year * 10 + quarter)
}

fn is_breakdown(series: &PackSeries) -> bool {
    !matches!(
        series.scope_kind.as_str(),
        "company_total" | "total" | "" | "unspecified"
    )
}

fn common_periods(group: &[PackSeries]) -> Vec<String> {
    let mut periods: Option<Vec<String>> = None;
    for series in group {
        let mut own: Vec<String> = series.points.iter().map(|p| p.period.clone()).collect();
        own.sort_by_key(|period| parse_period_sort_key(period).unwrap_or_default());
        own.dedup();
        periods = Some(match periods {
            None => own,
            Some(mut shared) => {
                shared.retain(|period| own.contains(period));
                shared
            }
        });
    }
    periods.unwrap_or_default()
}

fn latest_common_period(group: &[PackSeries]) -> Option<String> {
    common_periods(group)
        .into_iter()
        .max_by_key(|period| parse_period_sort_key(period).unwrap_or_default())
}

fn series_matches_clause(series: &PackSeries, clause: &ChartClause) -> bool {
    let is_breakdown = is_breakdown(series);
    if clause.chart_kind == "comparison"
        && is_breakdown
        && clause.metric_scope != "company_total"
        && clause.metric_scope != "total"
    {
        // A multi-ticker comparison of arbitrary segments would silently
        // compare unlike products/regions. Composition is the only chart
        // kind that is allowed to consume dimensioned rows.
        return false;
    }
    match clause.metric_scope.as_str() {
        "company_total" | "total" => !is_breakdown,
        "dimensioned" => {
            (!is_breakdown && clause.chart_kind == "composition")
                || (is_breakdown
                    && !clause.metric_dimensions.is_empty()
                    && series
                        .scope_dimension
                        .as_ref()
                        .is_some_and(|dimension| clause.metric_dimensions.contains(dimension)))
        }
        // `any` is intentionally conservative when no dimension is named:
        // mixing company totals and arbitrary segments is worse than omitting
        // a chart. A named dimension is still safe to select.
        "any" => {
            if clause.metric_dimensions.is_empty() {
                !is_breakdown
            } else {
                (!is_breakdown && clause.chart_kind == "composition")
                    || (is_breakdown
                        && series
                            .scope_dimension
                            .as_ref()
                            .is_some_and(|dimension| clause.metric_dimensions.contains(dimension)))
            }
        }
        _ => false,
    }
}

/// One group's artifact: a primary view plus an optional deterministic growth
/// view. `None` means the group cannot support a chart at all.
fn compile_group(key: &GroupKey, group: &[PackSeries]) -> Option<Value> {
    let breakdown_series = group
        .iter()
        .filter(|series| is_breakdown(series))
        .collect::<Vec<_>>();
    let total_series = group.iter().find(|series| !is_breakdown(series));
    let distinct_scopes = breakdown_series
        .iter()
        .map(|series| series.scope_key.as_str())
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    let unit = (!key.unit.is_empty()).then(|| key.unit.clone());

    let (chart_type, title, dataset, relationship, scope_mode) = match key.chart_kind.as_str() {
        "composition" => {
            let dimensions = breakdown_series
                .iter()
                .map(|series| series.scope_dimension.as_deref())
                .collect::<BTreeSet<_>>();
            if total_series.is_none()
                || breakdown_series.len() < MIN_COMPARISON_SERIES
                || distinct_scopes < MIN_COMPARISON_SERIES
                || !breakdown_series
                    .iter()
                    .all(|series| series.composition_eligible)
                || dimensions.len() != 1
                || dimensions.contains(&None)
            {
                return None;
            }
            let period = latest_common_period(group)?;
            let snapshot: Vec<&PackSeries> = breakdown_series
                .iter()
                .filter(|series| series.points.iter().any(|p| p.period == period))
                .copied()
                .collect();
            let values = snapshot
                .iter()
                .filter_map(|series| series.points.iter().find(|p| p.period == period))
                .map(|point| point.value)
                .collect::<Vec<_>>();
            if values.iter().any(|value| *value < 0.0) {
                // Composition with negatives is not a share chart; report no
                // chart rather than a misleading one.
                return None;
            }
            let total = total_series
                .and_then(|series| series.points.iter().find(|p| p.period == period))
                .map(|point| point.value)?;
            if total <= 0.0 {
                // A zero/negative denominator cannot produce a meaningful
                // share chart, even when all parts happen to sum to zero.
                return None;
            }
            let sum = values.iter().sum::<f64>();
            let tolerance = (sum - total).abs() / total.abs().max(1.0);
            if tolerance > 0.01 {
                // A dimensioned view is only a composition when its parts
                // reconcile to the company total for the same period.
                return None;
            }
            (
                "donut",
                format!("{period} {} 구성", metric_label(key, group)),
                composition_dataset(&snapshot, &period),
                "composition_snapshot",
                "breakdown",
            )
        }
        "trend" => {
            if !group
                .iter()
                .any(|series| series.points.len() >= MIN_TREND_POINTS)
            {
                return None;
            }
            (
                "line",
                format!("{} 추이", metric_label(key, group)),
                json!({"kind": "series"}),
                "trend",
                "total",
            )
        }
        "comparison" => {
            if group.len() < MIN_COMPARISON_SERIES {
                return None;
            }
            let period = latest_common_period(group)?;
            (
                "bar",
                format!("{} 비교 ({period})", metric_label(key, group)),
                json!({"kind": "series"}),
                "comparison",
                "total",
            )
        }
        _ => return None,
    };

    let mut views = vec![view_json(
        "view-1",
        chart_type,
        &title,
        unit.as_deref(),
        group,
        &dataset,
    )];
    let mut transform_hints: Vec<&str> = Vec::new();
    let mut has_derived = false;
    if let Some(growth) = growth_view(key, group) {
        transform_hints.push(growth.transform);
        has_derived = true;
        views.push(growth.view);
    }

    let mut evidence_refs: Vec<String> = Vec::new();
    for view in &views {
        collect_evidence_refs(view, &mut evidence_refs);
    }
    let series_count: usize = views
        .iter()
        .filter_map(|view| view.get("series")?.as_array().map(Vec::len))
        .sum();
    let point_count: usize = views
        .iter()
        .filter_map(|view| {
            view.get("series")?.as_array().map(|series| {
                series
                    .iter()
                    .filter_map(|item| item.get("points")?.as_array().map(Vec::len))
                    .sum::<usize>()
            })
        })
        .sum();
    let mut source_kinds = vec!["ontology"];
    if has_derived {
        source_kinds.push("derived");
    }
    let fingerprint = semantic_fingerprint(&views, chart_type);
    let fingerprint_id = fingerprint.strip_prefix("sha256:").unwrap_or(&fingerprint);
    let artifact_ref = format!("viz_{}", &fingerprint_id[..16.min(fingerprint_id.len())]);
    Some(json!({
        "artifact_format": ARTIFACT_FORMAT,
        "schema_version": ARTIFACT_SCHEMA_VERSION,
        "artifact_ref": artifact_ref,
        "semantic_fingerprint": fingerprint,
        "intent": {
            "goal": title,
            "measures": [key.metric],
            "relationship": relationship,
            "scope_mode": scope_mode,
            "transform_hints": transform_hints,
        },
        "title": title,
        "views": views,
        "provenance": {
            "source_tool_call_ids": [],
            "evidence_refs": evidence_refs,
            "source_kinds": source_kinds,
        },
        "quality": {
            "series_count": series_count,
            "point_count": point_count,
            "warnings": [],
        },
    }))
}

fn metric_label(key: &GroupKey, group: &[PackSeries]) -> String {
    group
        .first()
        .map(|series| series.label.clone())
        .filter(|label| !label.is_empty())
        .unwrap_or_else(|| key.metric.clone())
}

fn view_json(
    view_id: &str,
    chart_type: &str,
    title: &str,
    unit: Option<&str>,
    group: &[PackSeries],
    dataset: &Value,
) -> Value {
    let series = group.iter().map(series_json).collect::<Vec<_>>();
    json!({
        "view_id": view_id,
        "chart_type": chart_type,
        "title": title,
        "unit": unit,
        "series": series,
        "dataset": dataset,
    })
}

fn series_json(series: &PackSeries) -> Value {
    let points = series
        .points
        .iter()
        .map(|point| {
            let mut object = json!({
                "period": point.period,
                "period_basis": point.period_basis,
                "period_axis": point.period_axis,
                "period_sort_key": point.period_sort_key,
                "fiscal_year": point.fiscal_year,
                "fiscal_quarter": point.fiscal_quarter,
                "period_start": point.period_start,
                "period_end": point.period_end,
                "value": number(point.value),
                "evidence_ref": point.object_id,
            });
            if let Some(formatted) = &point.formatted_value {
                object["formatted_value"] = json!(formatted);
            }
            object
        })
        .collect::<Vec<_>>();
    let mut object = json!({
        "series_key": series.series_key,
        "label": series.label,
        "source_label": series.label,
        "scope": {
            "kind": series.scope_kind,
            "key": series.scope_key,
            "label": series.scope_label,
            "dimension": series.scope_dimension,
            "composition_eligible": series.composition_eligible,
        },
        "points": points,
    });
    let map = object.as_object_mut().expect("series object");
    for (key, value) in [
        ("ticker", &series.ticker),
        ("metric_name", &series.metric_name),
        ("canonical_metric", &series.canonical_metric),
        ("unit", &series.unit),
        ("currency", &series.currency),
        ("period_type", &series.period_type),
        ("duration", &series.duration),
    ] {
        if let Some(value) = value {
            map.insert(key.into(), json!(value));
        }
    }
    object
}

fn composition_dataset(snapshot: &[&PackSeries], period: &str) -> Value {
    let mut slices = Vec::new();
    for series in snapshot {
        let Some(point) = series.points.iter().find(|p| p.period == period) else {
            continue;
        };
        let mut slice = json!({
            "id": series.scope_key,
            "label": series.scope_label.clone().unwrap_or_else(|| series.label.clone()),
            "value": number(point.value),
            "evidence_refs": [point.object_id],
        });
        if let Some(formatted) = &point.formatted_value {
            slice["formatted_value"] = json!(formatted);
        }
        slices.push(slice);
    }
    json!({
        "kind": "composition",
        "period": period,
        "slices": slices,
    })
}

struct GrowthView {
    view: Value,
    transform: &'static str,
}

/// Deterministic period-over-period growth view. The transform name follows
/// the source period type (`yoy_percent` for annual, `qoq_percent` otherwise).
fn growth_view(key: &GroupKey, group: &[PackSeries]) -> Option<GrowthView> {
    let transform = if group
        .iter()
        .any(|series| series.period_type.as_deref() == Some("annual"))
    {
        "yoy_percent"
    } else {
        "qoq_percent"
    };
    let mut derived_series = Vec::new();
    for series in group {
        if series.points.len() < 2 {
            continue;
        }
        let mut points = Vec::new();
        for window in series.points.windows(2) {
            let (previous, current) = (&window[0], &window[1]);
            if !periods_are_compatible(previous, current, transform) {
                continue;
            }
            if previous.value == 0.0 {
                continue;
            }
            points.push(json!({
                "period": current.period,
                "value": number((current.value / previous.value - 1.0) * 100.0),
                "evidence_ref": current.object_id,
                "derived_from_evidence_refs": [previous.object_id, current.object_id],
            }));
        }
        if points.is_empty() {
            continue;
        }
        derived_series.push(json!({
            "series_key": series.series_key,
            "label": format!("{} (증가율)", series.label),
            "source_label": series.label,
            "scope": {
                "kind": series.scope_kind,
                "key": series.scope_key,
                "label": series.scope_label,
            },
            "unit": "%",
            "points": points,
        }));
    }
    let chartable = derived_series
        .iter()
        .any(|series| series["points"].as_array().is_some_and(|p| p.len() >= 2));
    if !chartable {
        return None;
    }
    let ticker_suffix = group
        .first()
        .and_then(|series| series.ticker.clone())
        .map(|ticker| format!(" — {ticker}"))
        .unwrap_or_default();
    Some(GrowthView {
        view: json!({
            "view_id": "view-2",
            "chart_type": "line",
            "title": format!("{} 증가율{ticker_suffix}", metric_label(key, group)),
            "unit": "%",
            "series": derived_series,
            "dataset": {"kind": "series"},
        }),
        transform,
    })
}

fn periods_are_compatible(previous: &PackPoint, current: &PackPoint, transform: &str) -> bool {
    if previous.period_axis != current.period_axis || previous.period_basis != current.period_basis
    {
        return false;
    }
    let previous_parts = parse_period_components(&previous.period);
    let current_parts = parse_period_components(&current.period);
    if transform == "yoy_percent" {
        return match (previous_parts, current_parts) {
            (
                Some((_, previous_year, previous_quarter)),
                Some((_, current_year, current_quarter)),
            ) => current_year - previous_year == 1 && previous_quarter == current_quarter,
            _ => false,
        };
    }
    match (previous_parts, current_parts) {
        (Some((_, previous_year, previous_quarter)), Some((_, current_year, current_quarter)))
            if previous_quarter > 0 && current_quarter > 0 =>
        {
            current_year * 4 + current_quarter - (previous_year * 4 + previous_quarter) == 1
        }
        _ => false,
    }
}

fn number(value: f64) -> Value {
    serde_json::Number::from_f64(value).map_or(Value::Null, Value::Number)
}

fn collect_evidence_refs(view: &Value, refs: &mut Vec<String>) {
    let Some(series) = view.get("series").and_then(Value::as_array) else {
        return;
    };
    for item in series {
        let Some(points) = item.get("points").and_then(Value::as_array) else {
            continue;
        };
        for point in points {
            if let Some(reference) = point.get("evidence_ref").and_then(Value::as_str)
                && !refs.iter().any(|existing| existing == reference)
            {
                refs.push(reference.to_owned());
            }
            if let Some(derived) = point
                .get("derived_from_evidence_refs")
                .and_then(Value::as_array)
            {
                for reference in derived.iter().filter_map(Value::as_str) {
                    if !reference.is_empty() && !refs.iter().any(|existing| existing == reference) {
                        refs.push(reference.to_owned());
                    }
                }
            }
        }
    }
}

/// Stable selected-data identity. The fingerprint is invariant to title
/// wording, but changes whenever any rendered number, period, chart type, or
/// evidence anchor in any view changes.
fn semantic_fingerprint(views: &[Value], chart_type: &str) -> String {
    #[derive(Serialize)]
    struct Identity<'a> {
        format: &'a str,
        chart_type: &'a str,
        series: &'a [Value],
    }
    let identity_series = views
        .iter()
        .filter_map(|view| view.get("series").and_then(Value::as_array))
        .flatten()
        .cloned()
        .collect::<Vec<_>>();
    let identity = Identity {
        format: "krw-presentation/identity/v1",
        chart_type,
        series: &identity_series,
    };
    let canonical =
        serde_jcs::to_vec(&identity).expect("identity canonicalization is total for JSON");
    let digest = Sha256::digest(&canonical);
    format!("sha256:{}", hex(&digest))
}

fn hex(bytes: &[u8]) -> String {
    const TABLE: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(TABLE[usize::from(byte >> 4)] as char);
        output.push(TABLE[usize::from(byte & 0x0f)] as char);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn point(period: &str, value: f64, id: &str) -> Value {
        json!({"period": period, "value": value, "object_id": id})
    }

    fn total_series(ticker: &str, points: Vec<Value>) -> Value {
        json!({
            "series_key": format!("{ticker}:revenue"),
            "label": "Revenue",
            "ticker": ticker,
            "canonical_metric": "revenue",
            "metric_name": "Revenue",
            "unit": "USD_millions",
            "basis": "consolidated",
            "duration": "fy",
            "period_type": "annual",
            "currency": "USD",
            "scope": {"kind": "company_total", "key": ticker, "label": ticker,
                      "composition_eligible": false},
            "points": points,
        })
    }

    fn breakdown_series(scope_key: &str, label: &str, points: Vec<Value>) -> Value {
        json!({
            "series_key": format!("AAPL:revenue:product:{scope_key}"),
            "label": label,
            "ticker": "AAPL",
            "canonical_metric": "revenue",
            "metric_name": "Revenue",
            "unit": "USD_millions",
            "basis": "consolidated",
            "duration": "fy",
            "period_type": "annual",
            "currency": "USD",
            "scope": {"kind": "product", "key": scope_key, "label": label,
                      "dimension": "product", "composition_eligible": true},
            "points": points,
        })
    }

    fn pack(series: Vec<Value>) -> Value {
        json!({
            "schema_version": 2,
            "mode": "chart_series_sidecar",
            "chart_clauses": [{
                "clause_id": "fixture_chart",
                "required": true,
                "metrics": ["revenue"],
                "tickers": ["AAPL", "MSFT"],
                "metric_scope": "company_total",
                "metric_dimensions": [],
                "calculation_window": "year_over_year"
            }],
            "series": series
        })
    }

    #[test]
    fn trend_pack_compiles_a_line_artifact_with_stable_identity() {
        let pack = pack(vec![total_series(
            "AAPL",
            vec![
                point("FY2023", 383.3, "obj-1"),
                point("FY2024", 391.0, "obj-2"),
                point("FY2025", 416.2, "obj-3"),
            ],
        )]);
        let artifacts = compile(&pack).expect("trend pack compiles");
        assert_eq!(artifacts.len(), 1);
        let artifact = &artifacts[0];
        assert_eq!(artifact["artifact_format"], ARTIFACT_FORMAT);
        assert_eq!(artifact["schema_version"], 4);
        assert!(
            artifact["artifact_ref"]
                .as_str()
                .is_some_and(|value| value.starts_with("viz_"))
        );
        assert_eq!(artifact["views"][0]["chart_type"], "line");
        assert_eq!(artifact["views"][0]["dataset"]["kind"], "series");
        assert_eq!(
            artifact["views"][0]["series"][0]["points"]
                .as_array()
                .map(Vec::len),
            Some(3)
        );
        assert_eq!(
            artifact["views"][0]["series"][0]["points"][0]["evidence_ref"],
            "obj-1"
        );
        // Chart-critical metadata rides along for the renderer.
        assert_eq!(artifact["views"][0]["series"][0]["unit"], "USD_millions");
        assert_eq!(artifact["views"][0]["series"][0]["period_type"], "annual");
        assert_eq!(
            artifact["provenance"]["evidence_refs"],
            json!(["obj-1", "obj-2", "obj-3"])
        );
        assert_eq!(
            artifact["provenance"]["source_kinds"],
            json!(["ontology", "derived"])
        );

        // Deterministic: same input, same fingerprint.
        let again = compile(&pack).expect("recompile");
        assert_eq!(
            again[0]["semantic_fingerprint"],
            artifact["semantic_fingerprint"]
        );

        // The annual series also earns a deterministic YoY view.
        assert_eq!(artifact["views"].as_array().map(Vec::len), Some(2));
        assert_eq!(artifact["views"][1]["chart_type"], "line");
        assert_eq!(
            artifact["views"][1]["series"][0]["points"]
                .as_array()
                .map(Vec::len),
            Some(2)
        );
        let growth = &artifact["views"][1]["series"][0]["points"][0];
        let expected = (391.0 / 383.3 - 1.0) * 100.0;
        assert_eq!(growth["value"].as_f64().unwrap(), expected);
        assert_eq!(
            growth["derived_from_evidence_refs"],
            json!(["obj-1", "obj-2"])
        );
        assert_eq!(
            artifact["intent"]["transform_hints"],
            json!(["yoy_percent"])
        );
    }

    #[test]
    fn two_tickers_become_one_multi_series_trend_view() {
        let pack = pack(vec![
            total_series(
                "AAPL",
                vec![
                    point("FY2023", 383.3, "a-1"),
                    point("FY2024", 391.0, "a-2"),
                    point("FY2025", 416.2, "a-3"),
                ],
            ),
            total_series(
                "MSFT",
                vec![
                    point("FY2023", 211.9, "m-1"),
                    point("FY2024", 245.1, "m-2"),
                    point("FY2025", 281.7, "m-3"),
                ],
            ),
        ]);
        let artifacts = compile(&pack).expect("multi-ticker trend");
        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0]["views"][0]["chart_type"], "line");
        assert_eq!(
            artifacts[0]["views"][0]["series"].as_array().map(Vec::len),
            Some(2)
        );
    }

    #[test]
    fn breakdown_scopes_compile_a_donut_composition_snapshot() {
        let mut pack = pack(vec![
            total_series("AAPL", vec![point("FY2025", 335.8, "total-1")]),
            breakdown_series("iphone", "iPhone", vec![point("FY2025", 209.6, "p-1")]),
            breakdown_series("services", "Services", vec![point("FY2025", 96.2, "p-2")]),
            breakdown_series("mac", "Mac", vec![point("FY2025", 30.0, "p-3")]),
        ]);
        pack["chart_clauses"][0]["tickers"] = json!(["AAPL"]);
        pack["chart_clauses"][0]["metric_scope"] = json!("dimensioned");
        pack["chart_clauses"][0]["metric_dimensions"] = json!(["product"]);
        pack["chart_clauses"][0]["calculation_window"] = Value::Null;
        let artifacts = compile(&pack).expect("composition compiles");
        assert_eq!(artifacts.len(), 1);
        let artifact = &artifacts[0];
        assert_eq!(artifact["views"][0]["chart_type"], "donut");
        let dataset = &artifact["views"][0]["dataset"];
        assert_eq!(dataset["kind"], "composition");
        assert_eq!(dataset["period"], "FY2025");
        assert_eq!(dataset["slices"].as_array().map(Vec::len), Some(3));
        assert_eq!(dataset["slices"][0]["evidence_refs"], json!(["p-1"]));
        assert_eq!(artifact["intent"]["relationship"], "composition_snapshot");
        // A single period per slice supports no growth view.
        assert_eq!(artifact["views"].as_array().map(Vec::len), Some(1));
    }

    #[test]
    fn two_tickers_with_one_period_become_a_bar_comparison() {
        let mut pack = pack(vec![
            total_series("AAPL", vec![point("FY2025", 416.2, "a-1")]),
            total_series("MSFT", vec![point("FY2025", 281.7, "m-1")]),
        ]);
        pack["chart_clauses"][0]["calculation_window"] = Value::Null;
        let artifacts = compile(&pack).expect("comparison compiles");
        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0]["views"][0]["chart_type"], "bar");
        assert!(
            artifacts[0]["views"][0]["title"]
                .as_str()
                .is_some_and(|title| title.contains("FY2025"))
        );
    }

    #[test]
    fn mixed_basis_groups_stay_separate_artifacts() {
        let mut as_reported = total_series(
            "AAPL",
            vec![
                point("FY2023", 100.0, "r-1"),
                point("FY2024", 110.0, "r-2"),
                point("FY2025", 120.0, "r-3"),
            ],
        );
        as_reported["basis"] = json!("as_reported");
        let pack = pack(vec![
            as_reported,
            total_series(
                "AAPL",
                vec![
                    point("FY2023", 90.0, "c-1"),
                    point("FY2024", 115.0, "c-2"),
                    point("FY2025", 130.0, "c-3"),
                ],
            ),
        ]);
        let artifacts = compile(&pack).expect("mixed basis compiles separately");
        assert_eq!(artifacts.len(), 2, "each basis is its own comparable group");
        assert_ne!(
            artifacts[0]["semantic_fingerprint"],
            artifacts[1]["semantic_fingerprint"]
        );
    }

    #[test]
    fn each_clause_gets_its_own_chart_kind() {
        let mut pack = pack(vec![
            total_series(
                "AAPL",
                vec![
                    point("FY2023", 383.3, "a-1"),
                    point("FY2024", 391.0, "a-2"),
                    point("FY2025", 416.2, "a-3"),
                ],
            ),
            total_series(
                "MSFT",
                vec![
                    point("FY2023", 211.9, "m-1"),
                    point("FY2024", 245.1, "m-2"),
                    point("FY2025", 281.7, "m-3"),
                ],
            ),
        ]);
        pack["chart_clauses"] = json!([
            {
                "clause_id": "trend_clause",
                "metrics": ["revenue"],
                "tickers": ["AAPL", "MSFT"],
                "metric_scope": "company_total",
                "calculation_window": "year_over_year"
            },
            {
                "clause_id": "comparison_clause",
                "metrics": ["revenue"],
                "tickers": ["AAPL", "MSFT"],
                "metric_scope": "company_total",
                "calculation_window": null
            }
        ]);
        let artifacts = compile(&pack).expect("both clauses compile");
        assert_eq!(artifacts.len(), 2);
        let kinds = artifacts
            .iter()
            .map(|artifact| artifact["views"][0]["chart_type"].as_str().unwrap())
            .collect::<BTreeSet<_>>();
        assert_eq!(kinds, BTreeSet::from(["bar", "line"]));
    }

    #[test]
    fn fy_and_cy_axes_are_not_combined_when_metadata_is_missing() {
        let pack = pack(vec![total_series(
            "AAPL",
            vec![
                point("FY2023", 383.3, "a-1"),
                point("CY2024", 391.0, "a-2"),
                point("FY2025", 416.2, "a-3"),
            ],
        )]);
        assert!(compile(&pack).expect("mixed axes are fail-open").is_empty());
    }

    #[test]
    fn quarterly_cross_year_growth_uses_qoq_ordinal() {
        let mut series = total_series(
            "AAPL",
            vec![
                point("FY2024Q3", 90.0, "q-3"),
                point("FY2024Q4", 100.0, "q-4"),
                point("FY2025Q1", 110.0, "q-5"),
            ],
        );
        series["period_type"] = json!("quarterly");
        let mut pack = pack(vec![series]);
        pack["chart_clauses"][0]["calculation_window"] = json!("period_over_period");
        let artifacts = compile(&pack).expect("quarterly trend");
        assert_eq!(artifacts.len(), 1);
        assert_eq!(
            artifacts[0]["intent"]["transform_hints"],
            json!(["qoq_percent"])
        );
        assert_eq!(artifacts[0]["views"].as_array().map(Vec::len), Some(2));
        assert_eq!(
            artifacts[0]["views"][1]["series"][0]["points"]
                .as_array()
                .map(Vec::len),
            Some(2)
        );
    }

    #[test]
    fn insufficient_or_negative_data_omits_charts_without_error() {
        // One series, one point: no trend, no comparison.
        let single = pack(vec![total_series(
            "AAPL",
            vec![point("FY2025", 416.2, "a-1")],
        )]);
        assert!(compile(&single).expect("single point").is_empty());

        // Two points: below the three-period trend floor.
        let two = pack(vec![total_series(
            "AAPL",
            vec![point("FY2024", 391.0, "a-1"), point("FY2025", 416.2, "a-2")],
        )]);
        assert!(compile(&two).expect("two points").is_empty());

        // Composition with a negative slice is not a share chart.
        let negative = pack(vec![
            total_series("AAPL", vec![point("FY2025", 293.8, "total-1")]),
            breakdown_series("iphone", "iPhone", vec![point("FY2025", 209.6, "p-1")]),
            breakdown_series("other", "Other", vec![point("FY2025", -12.0, "p-2")]),
            breakdown_series("services", "Services", vec![point("FY2025", 96.2, "p-3")]),
        ]);
        assert!(compile(&negative).expect("negative composition").is_empty());

        // Non-finite and non-numeric points are dropped, not rendered. Three
        // valid points survive the two invalid ones and still chart.
        let mut invalid = total_series(
            "AAPL",
            vec![
                point("FY2022", 365.8, "a-0"),
                point("FY2023", 383.3, "a-1"),
                json!({"period": "FY2024", "value": "n/a", "object_id": "a-2"}),
                point("FY2025", 416.2, "a-3"),
            ],
        );
        invalid["points"].as_array_mut().unwrap().push(json!({
            "period": "FY2026", "value": Value::Bool(false), "object_id": "a-4"
        }));
        let pack = pack(vec![invalid]);
        let artifacts = compile(&pack).expect("invalid points are skipped");
        assert_eq!(artifacts.len(), 1);
        assert_eq!(
            artifacts[0]["views"][0]["series"][0]["points"]
                .as_array()
                .map(Vec::len),
            Some(3)
        );
    }

    #[test]
    fn structurally_invalid_pack_fails_instead_of_rendering() {
        assert_eq!(
            compile(&json!({"schema_version": 1})).unwrap_err(),
            CompileError::InvalidPack
        );
    }
}
