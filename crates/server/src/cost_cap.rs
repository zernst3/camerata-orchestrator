//! The run-wide spend cap (`camerata inspect --max-cost-usd`) and the controls that travel with a
//! run.
//!
//! # What the cap is
//!
//! A guard against a runaway bill (a 200K-line repo with a mis-set rule set), not a saving.
//! It must therefore fail LOUDLY: a run that stops early is an incomplete run, and an
//! incomplete run says so everywhere it can (the failed-pass list, the operator ledger, the
//! export manifest, the exit code). It never drops or invents a finding.
//!
//! # How it stops
//!
//! * **Between passes, never mid-response.** Before a (chunk, rule-batch) pass starts, the run
//!   asks [`crate::ai_audit::UsageMeter::admit`]. A pass that was admitted always finishes, its
//!   truncation retry included. A pass that was refused is recorded as [`NotRunPass`] and
//!   reported as NOT RUN.
//! * **Later stages** (alternative recommendation, calibration, fix generation, the clone judge,
//!   the deep tier) check the cap at their boundary
//!   ([`crate::ai_audit::UsageMeter::stage_blocked`]) and are recorded as [`NotRunStage`].
//! * **Message Batches** cannot be cancelled cheaply once submitted, so the cap is checked
//!   BEFORE a submission, against a projection (see
//!   [`crate::ai_audit::UsageMeter::refuse_batch_submission`]). A batch that was submitted runs
//!   to completion.
//!
//! # The overshoot bound
//!
//! Passes run concurrently, so the money a pass will cost is not known when the next one is
//! admitted. A pass is admitted only while `spent + in_flight * reserve < cap`, where `reserve`
//! is the most any single call has cost so far. If every pass costs about the same, the final
//! total is therefore at most **one pass's cost over the cap**: the pass that crosses the line.
//! A pass that is larger than every pass before it can add the difference. The first pass of a
//! run is always admitted (nothing has been spent), so a cap smaller than one pass still lets
//! that one pass finish.

use serde::{Deserialize, Serialize};

use crate::efficiency::EfficiencyConfig;

/// Everything about HOW a run executes that is not WHAT it scans: which efficiency measures are
/// on and how much it may spend. Built once per run, carried on the run's
/// [`crate::ai_audit::UsageMeter`] (which every model-calling stage already receives), so no
/// stage signature grows.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct RunControls {
    pub efficiency: EfficiencyConfig,
    /// `--max-cost-usd`. `None` = no cap.
    pub max_cost_usd: Option<f64>,
    /// The pre-flight estimate of the whole run, when the caller computed one. Used ONLY to
    /// decide whether a non-cancellable batch may be submitted (a real-time run is capped on
    /// actual spend alone).
    pub estimate_hint_usd: Option<f64>,
}

impl RunControls {
    /// Defaults plus the environment's efficiency switches; no cap.
    pub fn from_env() -> Self {
        Self {
            efficiency: EfficiencyConfig::from_env_or_default(),
            ..Self::default()
        }
    }

    /// A positive, finite cap, or `Err` with the reason. Zero and negative caps are refused:
    /// a cap of zero would still let the first pass run (see the module docs), which would
    /// read as "spend nothing" and do the opposite.
    pub fn validate_cap(cap: f64) -> Result<f64, String> {
        if cap.is_finite() && cap > 0.0 {
            Ok(cap)
        } else {
            Err(format!(
                "--max-cost-usd must be a positive dollar amount (got {cap})"
            ))
        }
    }
}

/// One (chunk, rule-batch) scan pass that did not run because the cap was reached.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotRunPass {
    pub repo: String,
    /// The routing group label (`pass[all]`, `pass[rust]`, `resolution`).
    pub group: String,
    /// 1-based position.
    pub chunk: usize,
    pub n_chunks: usize,
    /// 1-based position.
    pub rule_batch: usize,
    pub n_rule_batches: usize,
    /// The rules that were in this rule-batch and so were not checked against this chunk.
    pub rule_ids: Vec<String>,
}

/// A whole stage that did not run because the cap was reached.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotRunStage {
    pub repo: String,
    /// The name the failed-pass list shows (the same names the stages use when they fail).
    pub stage: String,
}

/// What the cap did, as the operator ledger records it. Dollar figures live HERE and in the CLI
/// output only, never in the client export.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CostCapRecord {
    pub cap_usd: f64,
    /// Actual spend when the report was built.
    pub spent_usd: f64,
    pub not_run_passes: Vec<NotRunPass>,
    pub not_run_stages: Vec<NotRunStage>,
    /// Plain-language operator notes (a refused batch submission and its projection).
    pub notes: Vec<String>,
}

impl CostCapRecord {
    /// True when the cap actually stopped something. A cap that was set but never reached
    /// leaves the run complete.
    pub fn stopped_the_run(&self) -> bool {
        !self.not_run_passes.is_empty() || !self.not_run_stages.is_empty()
    }

    /// The failed-pass disclosures for everything the cap stopped. The text carries NO dollar
    /// figure (it reaches the client report); the amounts are in the ledger and the CLI output.
    pub fn failed_passes(&self) -> Vec<crate::ai_audit::FailedPass> {
        let mut out: Vec<crate::ai_audit::FailedPass> = Vec::new();
        // One entry per (repo, group, chunk), listing its rule-batches, so a 200-chunk scan is
        // not 1,600 notes while every (chunk, rule-batch) pair is still named.
        // Passes are recorded in the order they were refused, which depends on scheduling;
        // the disclosure is in position order so it reads the same on every run.
        let mut passes = self.not_run_passes.clone();
        passes.sort_by(|a, b| {
            (&a.repo, &a.group, a.chunk, a.rule_batch).cmp(&(
                &b.repo,
                &b.group,
                b.chunk,
                b.rule_batch,
            ))
        });
        let mut i = 0;
        while i < passes.len() {
            let first = &passes[i];
            let mut batches = vec![first.rule_batch];
            let mut rules = first.rule_ids.len();
            let mut j = i + 1;
            while j < passes.len()
                && passes[j].repo == first.repo
                && passes[j].group == first.group
                && passes[j].chunk == first.chunk
            {
                batches.push(passes[j].rule_batch);
                rules += passes[j].rule_ids.len();
                j += 1;
            }
            let listed = batches
                .iter()
                .map(usize::to_string)
                .collect::<Vec<_>>()
                .join(", ");
            out.push(crate::ai_audit::FailedPass {
                repo: first.repo.clone(),
                pass: format!(
                    "scan pass {} chunk {}/{}",
                    first.group, first.chunk, first.n_chunks
                ),
                reason: format!(
                    "NOT RUN: the spend limit set for this run was reached before rule-batch {listed} \
                     of {} ran, so {rules} rule check(s) were not made against this part of the \
                     code. This inspection is incomplete.",
                    first.n_rule_batches
                ),
            });
            i = j;
        }
        for stage in &self.not_run_stages {
            out.push(crate::ai_audit::FailedPass {
                repo: stage.repo.clone(),
                pass: stage.stage.clone(),
                reason: "NOT RUN: the spend limit set for this run was reached before this \
                         stage started. This inspection is incomplete."
                    .to_string(),
            });
        }
        out
    }

    /// Every rule id that was left unchecked on some part of the code.
    pub fn unchecked_rule_ids(&self, repo: &str) -> std::collections::BTreeSet<String> {
        self.not_run_passes
            .iter()
            .filter(|p| p.repo == repo)
            .flat_map(|p| p.rule_ids.iter().cloned())
            .collect()
    }
}

/// The mutable half of the cap, owned by the [`crate::ai_audit::UsageMeter`].
#[derive(Debug, Default)]
pub(crate) struct BudgetState {
    pub in_flight: u64,
    pub not_run_passes: Vec<NotRunPass>,
    pub not_run_stages: Vec<NotRunStage>,
    pub notes: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pass(chunk: usize, batch: usize, rules: &[&str]) -> NotRunPass {
        NotRunPass {
            repo: "acme/api".into(),
            group: "pass[all]".into(),
            chunk,
            n_chunks: 3,
            rule_batch: batch,
            n_rule_batches: 4,
            rule_ids: rules.iter().map(|r| r.to_string()).collect(),
        }
    }

    #[test]
    fn a_non_positive_or_non_finite_cap_is_refused() {
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert!(RunControls::validate_cap(bad).is_err(), "{bad}");
        }
        assert_eq!(RunControls::validate_cap(2.5).unwrap(), 2.5);
    }

    #[test]
    fn failed_passes_name_every_pair_once_per_chunk_and_carry_no_dollars() {
        let record = CostCapRecord {
            cap_usd: 12.34,
            spent_usd: 13.57,
            not_run_passes: vec![
                pass(2, 3, &["A-1", "A-2"]),
                pass(2, 4, &["A-3"]),
                pass(3, 1, &["A-4"]),
            ],
            not_run_stages: vec![NotRunStage {
                repo: "acme/api".into(),
                stage: "calibration".into(),
            }],
            notes: vec![],
        };
        let failed = record.failed_passes();
        assert_eq!(failed.len(), 3, "2 chunks + 1 stage: {failed:?}");
        assert!(failed[0].pass.contains("chunk 2/3"));
        assert!(
            failed[0].reason.contains("rule-batch 3, 4 of 4"),
            "{}",
            failed[0].reason
        );
        assert!(
            failed[0].reason.contains("3 rule check"),
            "{}",
            failed[0].reason
        );
        assert!(failed[1].reason.contains("rule-batch 1 of 4"));
        assert_eq!(failed[2].pass, "calibration");
        for fp in &failed {
            assert!(fp.reason.starts_with("NOT RUN"), "{}", fp.reason);
            assert!(fp.reason.contains("incomplete"), "{}", fp.reason);
            assert!(!fp.reason.contains('$') && !fp.reason.contains("12.34"));
        }
    }

    #[test]
    fn a_cap_that_stopped_nothing_leaves_the_run_complete() {
        let record = CostCapRecord {
            cap_usd: 5.0,
            spent_usd: 1.0,
            ..CostCapRecord::default()
        };
        assert!(!record.stopped_the_run());
        assert!(record.failed_passes().is_empty());
    }
}
