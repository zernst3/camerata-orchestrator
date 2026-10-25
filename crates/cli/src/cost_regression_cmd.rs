//! `camerata cost-regression`, the cost check a fixture sweep calls between two runs.
//!
//! ```text
//! camerata cost-regression --baseline <LEDGER> --current <LEDGER> [--threshold-pct 15]
//! ```
//!
//! Reads the two operator ledgers `camerata inspect` persisted for the SAME fixture (each carries
//! its per-stage telemetry and counted-line total), runs
//! [`camerata_server::stage_telemetry::cost_regression`] and exits:
//!
//! - **`0`**: cost per 1K counted lines is within the threshold (or fell);
//! - **`1`**: cost per 1K counted lines rose above the threshold;
//! - **`2`**: the check could not run (a ledger is missing, unreadable, or carries no usable
//!   telemetry). Never mistaken for `0`: a sweep that treated "could not compare" as "no
//!   regression" would stop catching cost regressions the first time a ledger went missing.
//!
//! No model call, no network. The comparison is a pure function of two small JSON files.

use std::path::{Path, PathBuf};

use camerata_server::scan_ledger::ScanLedger;
use camerata_server::stage_telemetry::{
    cost_regression, CostRegression, RegressionVerdict, StageTelemetry,
};

/// Exit code for "the check itself could not run" (also a not-comparable pair).
pub const EXIT_CHECK_UNAVAILABLE: i32 = 2;
/// Exit code for "cost per 1K lines regressed".
pub const EXIT_REGRESSED: i32 = 1;

/// `cost-regression`'s arguments, clap-free.
#[derive(Debug, Clone)]
pub struct CostRegressionArgs {
    pub baseline: PathBuf,
    pub current: PathBuf,
    pub threshold_pct: f64,
}

/// The process exit code for a comparison result.
pub fn exit_code(result: &CostRegression) -> i32 {
    match result.verdict {
        RegressionVerdict::Within => 0,
        RegressionVerdict::Regressed => EXIT_REGRESSED,
        RegressionVerdict::NotComparable => EXIT_CHECK_UNAVAILABLE,
    }
}

fn load_telemetry(path: &Path) -> Result<StageTelemetry, String> {
    let bytes = std::fs::read(path)
        .map_err(|e| format!("could not read the scan ledger at {}: {e}", path.display()))?;
    let ledger: ScanLedger = serde_json::from_slice(&bytes)
        .map_err(|e| format!("{} is not a readable scan ledger: {e}", path.display()))?;
    ledger.stage_telemetry().cloned().ok_or_else(|| {
        format!(
            "{} carries no stage telemetry (the scan made no model call, or predates it)",
            path.display()
        )
    })
}

/// Load both ledgers and compare. `Err` only when a ledger cannot be loaded; a regression is a
/// successful check and comes back as `Ok`.
pub fn run_cost_regression(args: &CostRegressionArgs) -> Result<CostRegression, String> {
    let baseline = load_telemetry(&args.baseline)?;
    let current = load_telemetry(&args.current)?;
    Ok(cost_regression(&baseline, &current, args.threshold_pct))
}

#[cfg(test)]
mod tests {
    use super::*;
    use camerata_server::llm::LlmResponse;
    use camerata_server::stage_telemetry::{Stage, StageMeter, DEFAULT_COST_REGRESSION_PCT};

    fn ledger_file(dir: &Path, name: &str, lines: usize, cost: f64) -> PathBuf {
        let meter = StageMeter::default();
        meter.record(
            Stage::ScanPass,
            &LlmResponse {
                text: String::new(),
                model: "m".to_string(),
                backend: "api".to_string(),
                cost_usd: Some(cost),
                input_tokens: Some(1_000),
                output_tokens: Some(100),
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
                or_cache_discount: None,
                truncated: false,
            },
        );
        let mut ledger = ScanLedger::new();
        ledger.record_stage_telemetry(meter.snapshot(lines));
        let path = dir.join(name);
        std::fs::write(&path, serde_json::to_vec(&ledger).unwrap()).unwrap();
        path
    }

    fn args(baseline: PathBuf, current: PathBuf) -> CostRegressionArgs {
        CostRegressionArgs {
            baseline,
            current,
            threshold_pct: DEFAULT_COST_REGRESSION_PCT,
        }
    }

    #[test]
    fn a_cost_rise_above_the_threshold_exits_one() {
        let dir = tempfile::tempdir().unwrap();
        let base = ledger_file(dir.path(), "base.json", 10_000, 10.0);
        let cur = ledger_file(dir.path(), "cur.json", 10_000, 12.0);
        let result = run_cost_regression(&args(base, cur)).expect("the check runs");
        assert!(result.is_regression());
        assert_eq!(exit_code(&result), EXIT_REGRESSED);
    }

    #[test]
    fn a_flat_or_cheaper_run_exits_zero() {
        let dir = tempfile::tempdir().unwrap();
        let base = ledger_file(dir.path(), "base.json", 10_000, 10.0);
        let cur = ledger_file(dir.path(), "cur.json", 10_000, 9.0);
        let result = run_cost_regression(&args(base, cur)).expect("the check runs");
        assert_eq!(exit_code(&result), 0);
    }

    #[test]
    fn a_missing_ledger_is_unavailable_never_a_pass() {
        let dir = tempfile::tempdir().unwrap();
        let base = ledger_file(dir.path(), "base.json", 10_000, 10.0);
        let err = run_cost_regression(&args(base, dir.path().join("nope.json")))
            .expect_err("a missing ledger cannot be compared");
        assert!(err.contains("could not read"), "{err}");
    }

    #[test]
    fn a_ledger_without_telemetry_is_unavailable_never_a_pass() {
        let dir = tempfile::tempdir().unwrap();
        let base = ledger_file(dir.path(), "base.json", 10_000, 10.0);
        let bare = dir.path().join("bare.json");
        std::fs::write(&bare, serde_json::to_vec(&ScanLedger::new()).unwrap()).unwrap();
        let err = run_cost_regression(&args(base, bare)).expect_err("no telemetry");
        assert!(err.contains("no stage telemetry"), "{err}");
    }

    #[test]
    fn records_that_cannot_be_compared_exit_two() {
        let dir = tempfile::tempdir().unwrap();
        let base = ledger_file(dir.path(), "base.json", 10_000, 10.0);
        let cur = ledger_file(dir.path(), "cur.json", 0, 10.0);
        let result = run_cost_regression(&args(base, cur)).expect("the check runs");
        assert_eq!(exit_code(&result), EXIT_CHECK_UNAVAILABLE);
    }
}
