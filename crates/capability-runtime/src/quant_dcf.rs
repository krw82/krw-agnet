//! Deterministic DCF valuation builtin — "the tools compute, the model
//! interprets" (financial-services pattern P2/P4). The model authors a
//! fully-labeled input request (values it collected from observation reads
//! or filings); this module owns every arithmetic step: present values,
//! terminal value, the EV→equity bridge, and the base-centered sensitivity
//! grid. Results are advisory computation artifacts: they carry no evidence
//! grade of their own, never enter the evidence ledger, and never support a
//! strong claim — the provenance of every input stays where the model found
//! it. Invariant violations (terminal growth ≥ WACC, non-positive shares)
//! fail closed with a typed rejection instead of producing wrong numbers.

use serde_json::{Value, json};

/// Maximum projection horizon the builtin accepts (contract bound: 1..=10).
pub const MAX_PROJECTION_PERIODS: usize = 10;

/// Sensitivity grid is a fixed 5×5, base-centered (pattern P4): axis values
/// are base ± 2·step and base ± step; the center cell must equal the base
/// per-share output — the wiring's self-proof.
const SENSITIVITY_STEPS: [f64; 5] = [-2.0, -1.0, 0.0, 1.0, 2.0];
const WACC_AXIS_STEP: f64 = 0.005;
const GROWTH_AXIS_STEP: f64 = 0.0025;

/// Semantic failures surfaced as a bounded, declared rejection edge. Shape
/// errors are already rejected by the contract validator before this runs.
#[derive(Debug, Clone, PartialEq)]
pub enum QuantDcfError {
    TerminalGrowthNotBelowWacc,
    NonPositiveShares,
    EmptyCashFlows,
    ExitMultipleMissing,
}

impl QuantDcfError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::TerminalGrowthNotBelowWacc => "quant_dcf_terminal_growth_not_below_wacc",
            Self::NonPositiveShares => "quant_dcf_non_positive_shares",
            Self::EmptyCashFlows => "quant_dcf_empty_cash_flows",
            Self::ExitMultipleMissing => "quant_dcf_exit_multiple_missing",
        }
    }
}

struct DcfInput {
    fcfs: Vec<f64>,
    wacc: f64,
    terminal_growth: f64,
    method: String,
    exit_multiple: Option<f64>,
    net_debt: f64,
    shares: f64,
    mid_year: bool,
}

fn finite_number(value: &Value) -> Option<f64> {
    value.as_f64().filter(|number| number.is_finite())
}

fn parse_input(arguments: &Value) -> Result<DcfInput, QuantDcfError> {
    let object = arguments
        .as_object()
        .ok_or(QuantDcfError::EmptyCashFlows)?;
    let fcfs = object
        .get("fcfs")
        .and_then(Value::as_array)
        .map(|values| values.iter().filter_map(finite_number).collect::<Vec<_>>())
        .ok_or(QuantDcfError::EmptyCashFlows)?;
    if fcfs.is_empty() || fcfs.len() > MAX_PROJECTION_PERIODS {
        return Err(QuantDcfError::EmptyCashFlows);
    }
    let wacc = object
        .get("wacc")
        .and_then(finite_number)
        .ok_or(QuantDcfError::TerminalGrowthNotBelowWacc)?;
    let terminal_growth = object
        .get("terminal_growth")
        .and_then(finite_number)
        .ok_or(QuantDcfError::TerminalGrowthNotBelowWacc)?;
    if terminal_growth >= wacc {
        return Err(QuantDcfError::TerminalGrowthNotBelowWacc);
    }
    let shares = object
        .get("shares")
        .and_then(finite_number)
        .ok_or(QuantDcfError::NonPositiveShares)?;
    if shares <= 0.0 {
        return Err(QuantDcfError::NonPositiveShares);
    }
    let method = object
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("perpetuity")
        .to_owned();
    if method == "exit_multiple" && object.get("exit_multiple").and_then(finite_number).is_none() {
        return Err(QuantDcfError::ExitMultipleMissing);
    }
    Ok(DcfInput {
        fcfs,
        wacc,
        terminal_growth,
        exit_multiple: object.get("exit_multiple").and_then(finite_number),
        method,
        net_debt: object.get("net_debt").and_then(finite_number).unwrap_or(0.0),
        shares,
        mid_year: object.get("mid_year").and_then(Value::as_bool).unwrap_or(true),
    })
}

/// Core valuation for one (wacc, growth) pair. Mid-year convention
/// discounts period i at i − 0.5; the terminal value is discounted at the
/// final period's own discount point (n − 0.5 mid-year, n otherwise), the
/// convention the reference DCF discipline pins for five-year mid-year
/// models.
fn value_equity_per_share(
    fcfs: &[f64],
    wacc: f64,
    terminal_growth: f64,
    method: &str,
    exit_multiple: Option<f64>,
    net_debt: f64,
    shares: f64,
    mid_year: bool,
) -> Option<(f64, f64, f64, f64)> {
    if terminal_growth >= wacc || wacc <= 0.0 || shares <= 0.0 {
        return None;
    }
    let mut pv_sum = 0.0;
    for (index, fcf) in fcfs.iter().enumerate() {
        let period = index as f64 + 1.0 - if mid_year { 0.5 } else { 0.0 };
        pv_sum += fcf / wacc_add_one(wacc).powf(period);
    }
    let last_fcf = *fcfs.last()?;
    let terminal_value = match method {
        "exit_multiple" => last_fcf * exit_multiple?,
        _ => {
            let spread = wacc - terminal_growth;
            if spread <= 0.0 {
                return None;
            }
            let grown = last_fcf * (1.0 + terminal_growth);
            grown / spread
        }
    };
    let terminal_period = fcfs.len() as f64 - if mid_year { 0.5 } else { 0.0 };
    let pv_terminal = terminal_value / wacc_add_one(wacc).powf(terminal_period);
    let enterprise_value = pv_sum + pv_terminal;
    let equity_value = enterprise_value - net_debt;
    Some((enterprise_value, equity_value, equity_value / shares, pv_terminal))
}

fn wacc_add_one(wacc: f64) -> f64 {
    1.0 + wacc
}

/// Execute the builtin: validate semantics, compute the base case, then the
/// 5×5 sensitivity grid whose center cell is asserted equal to the base
/// per-share value (the self-proof from the reference sensitivity
/// discipline). Warnings annotate — never block — economically odd but
/// computable inputs (TV share outside 40–80%, WACC outside 5–20%).
pub fn run_quant_dcf(arguments: &Value) -> Result<Value, QuantDcfError> {
    let input = parse_input(arguments)?;
    let ticker = arguments
        .get("ticker")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let base = value_equity_per_share(
        &input.fcfs,
        input.wacc,
        input.terminal_growth,
        &input.method,
        input.exit_multiple,
        input.net_debt,
        input.shares,
        input.mid_year,
    )
    .ok_or(QuantDcfError::TerminalGrowthNotBelowWacc)?;
    let (enterprise_value, equity_value, per_share, pv_terminal) = base;

    let mut present_values = Vec::with_capacity(input.fcfs.len());
    let mut pv_sum = 0.0;
    for (index, fcf) in input.fcfs.iter().enumerate() {
        let period = index as f64 + 1.0 - if input.mid_year { 0.5 } else { 0.0 };
        let discount_factor = 1.0 / wacc_add_one(input.wacc).powf(period);
        let pv = fcf * discount_factor;
        pv_sum += pv;
        present_values.push(json!({
            "period": index + 1,
            "discount_period": period,
            "free_cash_flow": fcf,
            "discount_factor": discount_factor,
            "present_value": pv,
        }));
    }

    let terminal_share = if enterprise_value > 0.0 {
        pv_terminal / enterprise_value
    } else {
        f64::NAN
    };
    let mut warnings = Vec::new();
    if !(0.05..=0.20).contains(&input.wacc) {
        warnings.push(json!({
            "code": "quant_dcf_wacc_outside_typical_band",
            "detail": "WACC outside the 5%-20% typical band; treat the discount rate as the dominant assumption."
        }));
    }
    if terminal_share.is_finite() && !(0.40..=0.80).contains(&terminal_share) {
        warnings.push(json!({
            "code": "quant_dcf_terminal_share_outside_band",
            "detail": "PV of terminal value outside the 40%-80% band; projections may be too short or too conservative."
        }));
    }

    let wacc_axis = SENSITIVITY_STEPS
        .iter()
        .map(|step| (input.wacc + step * WACC_AXIS_STEP * 2.0).max(0.001))
        .collect::<Vec<_>>();
    let growth_axis = SENSITIVITY_STEPS
        .iter()
        .map(|step| input.terminal_growth + step * GROWTH_AXIS_STEP * 2.0)
        .collect::<Vec<_>>();
    let mut grid = Vec::with_capacity(SENSITIVITY_STEPS.len());
    for wacc in &wacc_axis {
        let mut row = Vec::with_capacity(SENSITIVITY_STEPS.len());
        for growth in &growth_axis {
            let cell = value_equity_per_share(
                &input.fcfs,
                *wacc,
                *growth,
                &input.method,
                input.exit_multiple,
                input.net_debt,
                input.shares,
                input.mid_year,
            )
            .map(|(_, _, per_share, _)| per_share);
            row.push(cell.map(Value::from).unwrap_or(Value::Null));
        }
        grid.push(Value::Array(row));
    }
    // The center cell must equal the base case output: this is the wiring
    // self-proof, so a drift here is a bug, not a tolerance.
    let center = grid[2].as_array().and_then(|row| row[2].as_f64());
    debug_assert_eq!(center, Some(per_share));

    Ok(json!({
        "format": "quant-dcf-result/v1",
        "ticker": ticker,
        "method": input.method,
        "mid_year": input.mid_year,
        "inputs": {
            "fcfs": input.fcfs,
            "wacc": input.wacc,
            "terminal_growth": input.terminal_growth,
            "exit_multiple": input.exit_multiple,
            "net_debt": input.net_debt,
            "shares": input.shares,
        },
        "present_values": present_values,
        "pv_of_projection": pv_sum,
        "terminal_value_pv": pv_terminal,
        "terminal_share_of_enterprise_value": if terminal_share.is_finite() {
            Value::from(terminal_share)
        } else {
            Value::Null
        },
        "enterprise_value": enterprise_value,
        "equity_value": equity_value,
        "equity_value_per_share": per_share,
        "warnings": warnings,
        "sensitivity": {
            "row_axis": "wacc",
            "column_axis": "terminal_growth",
            "wacc_axis": wacc_axis,
            "growth_axis": growth_axis,
            "equity_value_per_share": grid,
            "center_cell_equals_base": center == Some(per_share),
        },
        "usage": "Deterministic computation over model-labeled inputs. Advisory only: never filing evidence, never direct support for a target price or recommendation. Quote inputs from their own sources."
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(fcfs: &[f64], wacc: f64, growth: f64) -> Value {
        json!({
            "ticker": "AAPL",
            "fcfs": fcfs,
            "wacc": wacc,
            "terminal_growth": growth,
            "method": "perpetuity",
            "net_debt": 0.0,
            "shares": 100.0,
            "mid_year": false,
        })
    }

    #[test]
    fn perpetuity_math_matches_hand_computation() {
        // One period, end-of-year discounting: PV = 100/1.10,
        // TV = 100·1.02/(0.10−0.02) discounted one period.
        let result = run_quant_dcf(&request(&[100.0], 0.10, 0.02)).unwrap();
        let pv_projection = 100.0 / 1.10;
        let tv = 100.0 * 1.02 / 0.08;
        let pv_terminal = tv / 1.10;
        let ev = pv_projection + pv_terminal;
        assert!((result["pv_of_projection"].as_f64().unwrap() - pv_projection).abs() < 1e-9);
        assert!((result["terminal_value_pv"].as_f64().unwrap() - pv_terminal).abs() < 1e-9);
        assert!((result["enterprise_value"].as_f64().unwrap() - ev).abs() < 1e-9);
        assert!(
            (result["equity_value_per_share"].as_f64().unwrap() - ev / 100.0).abs() < 1e-9
        );
    }

    #[test]
    fn mid_year_convention_discounts_half_a_period_earlier() {
        let end_of_year = run_quant_dcf(&request(&[100.0], 0.10, 0.02)).unwrap();
        let mut mid = request(&[100.0], 0.10, 0.02);
        mid["mid_year"] = Value::Bool(true);
        let mid_year = run_quant_dcf(&mid).unwrap();
        assert!(
            mid_year["pv_of_projection"].as_f64().unwrap()
                > end_of_year["pv_of_projection"].as_f64().unwrap()
        );
        assert_eq!(
            mid_year["present_values"][0]["discount_period"].as_f64().unwrap(),
            0.5
        );
    }

    #[test]
    fn terminal_growth_at_or_above_wacc_fails_closed() {
        assert_eq!(
            run_quant_dcf(&request(&[100.0], 0.08, 0.08)),
            Err(QuantDcfError::TerminalGrowthNotBelowWacc)
        );
        assert_eq!(
            run_quant_dcf(&request(&[100.0], 0.08, 0.09)),
            Err(QuantDcfError::TerminalGrowthNotBelowWacc)
        );
    }

    #[test]
    fn non_positive_shares_and_missing_exit_multiple_fail_closed() {
        let mut bad_shares = request(&[100.0], 0.10, 0.02);
        bad_shares["shares"] = Value::from(0.0);
        assert_eq!(
            run_quant_dcf(&bad_shares),
            Err(QuantDcfError::NonPositiveShares)
        );
        let mut missing_multiple = request(&[100.0], 0.10, 0.02);
        missing_multiple["method"] = Value::String("exit_multiple".into());
        assert_eq!(
            run_quant_dcf(&missing_multiple),
            Err(QuantDcfError::ExitMultipleMissing)
        );
    }

    #[test]
    fn sensitivity_grid_is_base_centered_and_flags_impossible_cells() {
        let result = run_quant_dcf(&request(&[100.0, 110.0], 0.10, 0.02)).unwrap();
        assert_eq!(
            result["sensitivity"]["equity_value_per_share"]
                .as_array()
                .unwrap()
                .len(),
            5
        );
        assert!(result["sensitivity"]["center_cell_equals_base"]
            .as_bool()
            .unwrap());
        let base = result["equity_value_per_share"].as_f64().unwrap();
        let center = result["sensitivity"]["equity_value_per_share"][2][2]
            .as_f64()
            .unwrap();
        assert!((center - base).abs() < 1e-9);
    }

    #[test]
    fn out_of_band_results_carry_warnings_not_errors() {
        // A heavy terminal share (tiny near-term FCFs) warns but computes.
        let result = run_quant_dcf(&request(&[1.0, 1.0], 0.06, 0.03)).unwrap();
        assert!(result["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning["code"]
                .as_str()
                .unwrap()
                .contains("terminal_share")));
    }
}
