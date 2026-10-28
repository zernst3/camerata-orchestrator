//! Per-stage cost telemetry for one inspection (operator-only).
//!
//! The scan's [`crate::ai_audit::UsageMeter`] used to fold every model call into ONE aggregate,
//! so "where did the money go" could only be answered by hand: output was ~60% of the hearth
//! bill, but no per-stage split existed to say whether that was scan passes, fix retries or the
//! deep tier, and three runs of the same zip cost $12.3, $16.4 and $14.6 with nothing to say why
//! (`SPIKE_TOKEN_EFFICIENCY_RESULTS.md`, item 14). This module is that split:
//!
//! * [`Stage`] names the pipeline stages that call a model;
//! * [`StageMeter`] accumulates tokens, cache read/write and cost per stage, plus the tokens a
//!   stage spent on attempts whose output was thrown away ([`StageMeter::record_wasted`]);
//! * [`StageTelemetry`] is the immutable snapshot, joined to the scan's counted-line total so it
//!   can state cost per 1K counted lines, the unit that stays comparable across repos;
//! * [`cost_regression`] compares two snapshots of the same fixture and flags a cost-per-1K-lines
//!   increase above a threshold, so a later fixture sweep can fail on a regression.
//!
//! # Operator-only, never exported
//!
//! The snapshot lives on [`crate::scan_ledger::ScanLedger`] (the operator ledger the CLI persists
//! and prints), NOT on the client export. Nothing here is read by `report_export`, the workbook
//! or the manifest; the export format is a protected area and cost figures are not client
//! content. `report_export` has a test that pins this.

use std::collections::BTreeMap;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::llm::LlmResponse;

/// A pipeline stage that makes model calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    /// Chunk x rule-batch scan passes (`audit_pass`, batch items included).
    ScanPass,
    /// Alternative-option recommendation for multi-option rules.
    Alternatives,
    /// The post-merge regeneration of full finding detail, after a terse scan
    /// (`terse_output`; `detail_regen`).
    DetailRegeneration,
    /// The skeptic pass: verification, severity/confidence calibration and hour estimates
    /// (`verify_findings`).
    Calibration,
    /// Codebase-specific fix generation, retries included.
    FixGeneration,
    /// The opt-in deep compliance and security tier.
    DeepTier,
    /// The clone-advisory judge.
    CloneJudge,
}

impl Stage {
    /// Every stage, in pipeline order (the order the table prints).
    pub const ALL: [Stage; 7] = [
        Stage::ScanPass,
        Stage::Alternatives,
        Stage::DetailRegeneration,
        Stage::Calibration,
        Stage::FixGeneration,
        Stage::DeepTier,
        Stage::CloneJudge,
    ];

    /// The short name the table and the logs use.
    pub fn label(self) -> &'static str {
        match self {
            Stage::ScanPass => "scan",
            Stage::Alternatives => "alternatives",
            Stage::DetailRegeneration => "detail",
            Stage::Calibration => "calibration",
            Stage::FixGeneration => "fix",
            Stage::DeepTier => "deep",
            Stage::CloneJudge => "clone-judge",
        }
    }
}

/// What one stage spent. `input_tokens` is the billed input side INCLUDING cache read and
/// write (the same convention as [`LlmResponse::input_tokens`]).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StageUsage {
    pub calls: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub cost_usd: f64,
    /// Calls that reported a cost. `cost_usd` is a partial sum when this is below `calls`.
    #[serde(default)]
    pub cost_reported_calls: u64,
    /// Calls whose output was discarded (a rejected fix, a truncated pass that was retried).
    /// A SUBSET of `calls`, already counted in every field above; never added to totals twice.
    #[serde(default)]
    pub wasted_calls: u64,
    #[serde(default)]
    pub wasted_input_tokens: u64,
    #[serde(default)]
    pub wasted_output_tokens: u64,
    #[serde(default)]
    pub wasted_cost_usd: f64,
}

impl StageUsage {
    /// True when every call in this stage reported a cost, so `cost_usd` is not an
    /// understatement. An empty stage is trivially complete.
    pub fn cost_complete(&self) -> bool {
        self.cost_reported_calls == self.calls
    }

    fn fold(&mut self, r: &LlmResponse) {
        self.calls += 1;
        self.input_tokens += r.input_tokens.unwrap_or(0);
        self.output_tokens += r.output_tokens.unwrap_or(0);
        self.cache_read_input_tokens += r.cache_read_input_tokens;
        self.cache_creation_input_tokens += r.cache_creation_input_tokens;
        if let Some(c) = r.cost_usd {
            self.cost_usd += c;
            self.cost_reported_calls += 1;
        }
    }

    fn fold_wasted(&mut self, r: &LlmResponse, share: f64) {
        let share = share.clamp(0.0, 1.0);
        let prorate = |tokens: Option<u64>| (tokens.unwrap_or(0) as f64 * share).round() as u64;
        self.wasted_calls += 1;
        self.wasted_input_tokens += prorate(r.input_tokens);
        self.wasted_output_tokens += prorate(r.output_tokens);
        self.wasted_cost_usd += r.cost_usd.unwrap_or(0.0) * share;
    }

    fn add(&mut self, other: &StageUsage) {
        self.calls += other.calls;
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        self.cache_read_input_tokens += other.cache_read_input_tokens;
        self.cache_creation_input_tokens += other.cache_creation_input_tokens;
        self.cost_usd += other.cost_usd;
        self.cost_reported_calls += other.cost_reported_calls;
        self.wasted_calls += other.wasted_calls;
        self.wasted_input_tokens += other.wasted_input_tokens;
        self.wasted_output_tokens += other.wasted_output_tokens;
        self.wasted_cost_usd += other.wasted_cost_usd;
    }
}

/// Thread-safe per-stage accumulator (scan passes run concurrently).
#[derive(Default)]
pub struct StageMeter {
    stages: Mutex<BTreeMap<Stage, StageUsage>>,
}

impl StageMeter {
    /// Fold one completion into `stage`.
    pub fn record(&self, stage: Stage, r: &LlmResponse) {
        self.with_stage(stage, |u| u.fold(r));
    }

    /// Mark a completion that [`Self::record`] already counted as one whose output was thrown
    /// away. Adds to the stage's `wasted_*` fields only; the totals are untouched.
    pub fn record_wasted(&self, stage: Stage, r: &LlmResponse) {
        self.record_wasted_share(stage, r, 1.0);
    }

    /// Like [`Self::record_wasted`] for a completion only PART of whose output was thrown away
    /// (a batched fix call where some fixes passed the self-check and some did not): `share` is
    /// the discarded fraction, clamped to `0.0..=1.0`, and the tokens and cost are prorated by
    /// it. The call still counts once in `wasted_calls`.
    pub fn record_wasted_share(&self, stage: Stage, r: &LlmResponse, share: f64) {
        self.with_stage(stage, |u| u.fold_wasted(r, share));
    }

    /// Snapshot, joined to the scan's counted-line total.
    pub fn snapshot(&self, counted_lines: usize) -> StageTelemetry {
        let stages = self.lock();
        StageTelemetry {
            counted_lines,
            efficiency: None,
            stages: Stage::ALL
                .iter()
                .filter_map(|stage| {
                    stages
                        .get(stage)
                        .filter(|u| u.calls > 0)
                        .map(|usage| StageRecord {
                            stage: *stage,
                            usage: usage.clone(),
                        })
                })
                .collect(),
        }
    }

    fn with_stage(&self, stage: Stage, f: impl FnOnce(&mut StageUsage)) {
        f(self.lock().entry(stage).or_default());
    }

    /// Telemetry must never take the scan down: a poisoned lock still holds valid counters
    /// (every critical section is a handful of additions), so recover the guard.
    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<Stage, StageUsage>> {
        self.stages
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// One stage's row in a [`StageTelemetry`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StageRecord {
    pub stage: Stage,
    pub usage: StageUsage,
}

/// Immutable per-stage snapshot of one inspection.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StageTelemetry {
    /// The `camerata count` line total for the scanned repo (same filter as the scan).
    pub counted_lines: usize,
    /// Only stages that made at least one call, in pipeline order.
    pub stages: Vec<StageRecord>,
    /// Which efficiency measures were on for this run ([`crate::efficiency`]), so a cost figure
    /// is never read without knowing what produced it. `None` for a record written before the
    /// switches existed.
    #[serde(default)]
    pub efficiency: Option<crate::efficiency::EfficiencyConfig>,
}

impl StageTelemetry {
    /// All stages summed.
    pub fn total(&self) -> StageUsage {
        let mut t = StageUsage::default();
        for s in &self.stages {
            t.add(&s.usage);
        }
        t
    }

    /// Dollars per 1,000 counted lines; `None` when there are no counted lines.
    pub fn cost_per_1k_lines(&self) -> Option<f64> {
        per_1k(self.total().cost_usd, self.counted_lines)
    }

    /// The fixed-width per-stage table printed at the end of `camerata inspect`.
    pub fn render_table(&self) -> String {
        use std::fmt::Write as _;
        let mut out = String::new();
        writeln!(
            out,
            "== STAGE TELEMETRY (operator only; not part of the client export) =="
        )
        .ok();
        if let Some(efficiency) = &self.efficiency {
            writeln!(out, "  efficiency measures: {}", efficiency.render()).ok();
        }
        if self.stages.is_empty() {
            writeln!(out, "  (no model call ran this scan)").ok();
            writeln!(out, "  counted lines: {}", self.counted_lines).ok();
            return out;
        }
        writeln!(
            out,
            "  {:<13} {:>5} {:>10} {:>9} {:>10} {:>10} {:>9} {:>11} {:>19}",
            "stage",
            "calls",
            "input",
            "output",
            "cache-rd",
            "cache-wr",
            "cost",
            "$/1K lines",
            "wasted (in/out)"
        )
        .ok();
        let row = |out: &mut String, name: &str, u: &StageUsage, lines: usize| {
            let partial = if u.cost_complete() { "" } else { " PARTIAL" };
            let per_1k =
                per_1k(u.cost_usd, lines).map_or_else(|| "n/a".to_string(), |v| format!("{v:.4}"));
            writeln!(
                out,
                "  {:<13} {:>5} {:>10} {:>9} {:>10} {:>10} {:>9} {:>11} {:>19}{partial}",
                name,
                u.calls,
                u.input_tokens,
                u.output_tokens,
                u.cache_read_input_tokens,
                u.cache_creation_input_tokens,
                format!("${:.4}", u.cost_usd),
                per_1k,
                format!("{}/{}", u.wasted_input_tokens, u.wasted_output_tokens),
            )
            .ok();
        };
        for s in &self.stages {
            row(&mut out, s.stage.label(), &s.usage, self.counted_lines);
        }
        row(&mut out, "total", &self.total(), self.counted_lines);
        writeln!(out, "  counted lines: {}", self.counted_lines).ok();
        out
    }
}

fn per_1k(cost_usd: f64, counted_lines: usize) -> Option<f64> {
    if counted_lines == 0 {
        return None;
    }
    Some(cost_usd / (counted_lines as f64 / 1000.0))
}

/// The default regression threshold, per the spike's acceptance rule: cost per 1K counted lines
/// may not rise more than 15% against the previous sweep without a noted change.
pub const DEFAULT_COST_REGRESSION_PCT: f64 = 15.0;

/// Outcome of comparing two telemetry records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegressionVerdict {
    Within,
    Regressed,
    /// The records cannot be compared (no lines, no cost, or partial cost). Callers treat this
    /// as a failure to check, never as a pass.
    NotComparable,
}

/// One stage whose cost per 1K lines moved between the two records.
#[derive(Debug, Clone, PartialEq)]
pub struct StageCostChange {
    pub stage: Stage,
    pub baseline_per_1k: f64,
    pub current_per_1k: f64,
    pub change_pct: f64,
}

/// The result of [`cost_regression`].
#[derive(Debug, Clone, PartialEq)]
pub struct CostRegression {
    pub verdict: RegressionVerdict,
    pub threshold_pct: f64,
    pub baseline_per_1k: Option<f64>,
    pub current_per_1k: Option<f64>,
    /// Percent change of the overall cost per 1K lines (`None` when not comparable).
    pub change_pct: Option<f64>,
    /// Why the records were not comparable (empty otherwise).
    pub reason: String,
    /// Stages (present in both records) whose own cost per 1K lines rose above the threshold.
    pub stage_regressions: Vec<StageCostChange>,
}

impl CostRegression {
    pub fn is_regression(&self) -> bool {
        self.verdict == RegressionVerdict::Regressed
    }

    /// A short operator-readable verdict.
    pub fn render(&self) -> String {
        use std::fmt::Write as _;
        let mut out = String::new();
        match self.verdict {
            RegressionVerdict::NotComparable => {
                write!(
                    out,
                    "cost regression check: NOT COMPARABLE ({})",
                    self.reason
                )
                .ok();
            }
            verdict => {
                let label = if verdict == RegressionVerdict::Regressed {
                    "REGRESSION"
                } else {
                    "ok"
                };
                write!(
                    out,
                    "cost regression check: {label}: ${:.4} -> ${:.4} per 1K counted lines \
                     ({:+.1}%, threshold +{:.1}%)",
                    self.baseline_per_1k.unwrap_or(0.0),
                    self.current_per_1k.unwrap_or(0.0),
                    self.change_pct.unwrap_or(0.0),
                    self.threshold_pct,
                )
                .ok();
                for c in &self.stage_regressions {
                    write!(
                        out,
                        "\n  stage {}: ${:.4} -> ${:.4} per 1K ({:+.1}%)",
                        c.stage.label(),
                        c.baseline_per_1k,
                        c.current_per_1k,
                        c.change_pct,
                    )
                    .ok();
                }
            }
        }
        out
    }
}

/// Compare two telemetry records of the SAME fixture. Flags a regression when the current cost
/// per 1K counted lines exceeds the baseline's by more than `threshold_pct` percent.
pub fn cost_regression(
    baseline: &StageTelemetry,
    current: &StageTelemetry,
    threshold_pct: f64,
) -> CostRegression {
    let not_comparable = |reason: String| CostRegression {
        verdict: RegressionVerdict::NotComparable,
        threshold_pct,
        baseline_per_1k: baseline.cost_per_1k_lines(),
        current_per_1k: current.cost_per_1k_lines(),
        change_pct: None,
        reason,
        stage_regressions: Vec::new(),
    };
    for (name, t) in [("baseline", baseline), ("current", current)] {
        if t.counted_lines == 0 {
            return not_comparable(format!("the {name} record has no counted lines"));
        }
        let total = t.total();
        if total.calls == 0 {
            return not_comparable(format!("the {name} record made no model call"));
        }
        if !total.cost_complete() {
            return not_comparable(format!(
                "the {name} record's cost is partial (some calls reported no cost)"
            ));
        }
    }
    let (Some(base), Some(cur)) = (baseline.cost_per_1k_lines(), current.cost_per_1k_lines())
    else {
        return not_comparable("a record has no cost per 1K lines".to_string());
    };
    if base <= 0.0 {
        return not_comparable("the baseline cost is zero".to_string());
    }
    let change_pct = (cur - base) / base * 100.0;
    let stage_regressions = Stage::ALL
        .iter()
        .filter_map(|stage| {
            let b = per_1k(stage_cost(baseline, *stage)?, baseline.counted_lines)?;
            let c = per_1k(stage_cost(current, *stage)?, current.counted_lines)?;
            (b > 0.0 && (c - b) / b * 100.0 > threshold_pct).then(|| StageCostChange {
                stage: *stage,
                baseline_per_1k: b,
                current_per_1k: c,
                change_pct: (c - b) / b * 100.0,
            })
        })
        .collect();
    CostRegression {
        verdict: if change_pct > threshold_pct {
            RegressionVerdict::Regressed
        } else {
            RegressionVerdict::Within
        },
        threshold_pct,
        baseline_per_1k: Some(base),
        current_per_1k: Some(cur),
        change_pct: Some(change_pct),
        reason: String::new(),
        stage_regressions,
    }
}

fn stage_cost(t: &StageTelemetry, stage: Stage) -> Option<f64> {
    t.stages
        .iter()
        .find(|s| s.stage == stage)
        .map(|s| s.usage.cost_usd)
}

#[cfg(test)]
mod tests;
