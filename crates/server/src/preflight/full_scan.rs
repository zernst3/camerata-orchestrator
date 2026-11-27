//! The full-scan estimate `camerata count --estimate` prints and the app's pre-flight shows: the
//! point figure, a range and a suggested spend limit, from the counted lines.
//!
//! The formula is exactly `counted lines / 1,000 * dollars per 1,000 counted lines`, the same unit
//! `camerata inspect` prints in its stage table (`StageTelemetry::cost_per_1k_lines`). Without an
//! operator's own rate the documented constant [`MEASURED_USD_PER_1K_LINES`] is used and the line
//! says so. The range ratios (0.69 and 2.16 of the point figure) come from the same cost model: its
//! low and high cases on a large real project, against the measured rate. Moved here from the
//! command line crate so the command line and the app compute it in one place.

/// The only measurement at scale (one run of the earlier engine on a large real project), per
/// 1,000 counted lines. A stand-in for operators with no run of their own; the agent finder has
/// no measurement at scale yet.
pub const MEASURED_USD_PER_1K_LINES: f64 = 1.71;

/// Low end of the range as a share of the point figure (the cost model's low case).
pub const LOW_FACTOR: f64 = 0.69;
/// High end of the range as a share of the point figure (the cost model's bad case).
pub const HIGH_FACTOR: f64 = 2.16;
/// The suggested spend limit as a share of the point figure.
pub const LIMIT_FACTOR: f64 = 1.5;

/// Said on every estimate.
pub const NOTE: &str =
    "Measured on the earlier engine, not the agent finder. An estimate, not a quote.";

/// The full-scan estimate: `lines / 1,000 * usd_per_1k`.
pub fn full_scan_estimate_usd(counted_lines: usize, usd_per_1k: f64) -> f64 {
    counted_lines as f64 / 1_000.0 * usd_per_1k
}

/// One estimate: the point figure, the range and the suggested spend limit.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FullScanEstimate {
    pub lines: usize,
    pub rate: f64,
    /// The rate came from the operator (`--cost-per-1k-usd`), not the documented constant.
    pub from_flag: bool,
    pub point: f64,
    pub low: f64,
    pub high: f64,
    /// The suggested spend limit.
    pub limit: f64,
}

impl FullScanEstimate {
    /// The estimate for `lines` counted lines, at the operator's rate when given, else the
    /// documented constant.
    pub fn of(lines: usize, rate_flag: Option<f64>) -> Self {
        let rate = rate_flag.unwrap_or(MEASURED_USD_PER_1K_LINES);
        let point = full_scan_estimate_usd(lines, rate);
        Self {
            lines,
            rate,
            from_flag: rate_flag.is_some(),
            point,
            low: point * LOW_FACTOR,
            high: point * HIGH_FACTOR,
            limit: point * LIMIT_FACTOR,
        }
    }

    /// The figures as `camerata count --estimate --json` prints them.
    pub fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "usd": self.point,
            "low_usd": self.low,
            "high_usd": self.high,
            "suggested_spend_limit_usd": self.limit,
            "counted_lines": self.lines,
            "usd_per_1k_counted_lines": self.rate,
            "rate_from": if self.from_flag { "flag" } else { "documented constant" },
            "note": NOTE,
        })
    }

    /// The figures as `camerata count --estimate` prints them, under `label`.
    pub fn text(&self, label: &str) -> String {
        let basis = if self.from_flag {
            "from --cost-per-1k-usd"
        } else {
            "the documented constant measured at scale on an earlier engine; pass \
             --cost-per-1k-usd from your own run to replace it"
        };
        format!(
            "{label}: about ${:.2} ({} counted lines / 1,000 x ${:.2} per 1,000 counted lines; \
             {basis}).\n  Range: ${:.2} to ${:.2} (low = {LOW_FACTOR} x and high = {HIGH_FACTOR} x the \
             point figure, from the documented best guess and bad case).\n  Suggested spend limit: \
             ${:.2} ({LIMIT_FACTOR} x the point figure).\n  {NOTE}",
            self.point, self.lines, self.rate, self.low, self.high, self.limit
        )
    }
}
