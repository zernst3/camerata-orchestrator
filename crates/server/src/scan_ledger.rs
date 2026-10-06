//! W1 — the pipeline-integrity ledger.
//!
//! Two real defects motivated this module, both found on an unseen repo scan:
//!
//! 1. Rules that never ran were reported as "verified clean" / "excluded from this audit"
//!    while the selector's own evidence text described the violation. Six corpus rules
//!    declared `enforcement = "mechanical"` with NO wired detector at all, and were counted
//!    as checked anyway — because "was this rule selected" and "did this rule actually run"
//!    were the SAME boolean everywhere in the pipeline (see `onboard::ScanProvenance::
//!    audited_rule_ids`, which lists every SELECTED rule, never checking execution).
//! 2. The semantic/AI tier emitted zero rows for a repo and nothing noticed — a total pass
//!    failure was indistinguishable from a genuinely clean codebase.
//!
//! Both are SILENT KNOWLEDGE LOSS between pipeline stages. This module makes that
//! impossible to hide: a [`ScanLedger`] accumulated during a scan records, per rule, whether
//! it ACTUALLY ran (never inferred from mere selection), and, per pipeline stage, a
//! reconciled row count — `rows_in == rows_out + accounted`, where `accounted` is an
//! explicit breakdown (merged away, routed to held, routed to informational, deduped, or
//! genuinely unaccounted). An `unaccounted > 0` stage is an integrity violation.
//!
//! ## Runtime vs. test-time
//!
//! At RUNTIME this ledger never drops a row and never refuses an export — an integrity
//! violation is surfaced as a [`crate::ai_audit::FailedPass`]-style disclosure (see
//! [`stage_disclosure`] / [`rule_disclosure`]), which rides the EXISTING `failed_passes`
//! mechanism into the report's methodology and executive summary. At TEST time, the
//! consolidated `export_invariants_gate` (`report_export.rs`) and this module's own unit
//! tests assert zero unaccounted rows and fail the build otherwise — see
//! `crate::mechanical_gate` for the companion corpus-level build gate (item 4 of the W1
//! plan), which closes the SOURCE of the six-phantom-rule defect rather than just detecting
//! its symptom at scan time.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

/// Which engine a rule's evaluation belongs to. Drives nothing structurally today (the
/// invariants below are tier-agnostic) but lets a report or operator ask "how much of my
/// coverage is deterministic vs. model-judged" honestly, straight from the ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleTier {
    /// Camerata's own code: the content floor (`onboard::audit_files`) or a registered
    /// `ArchChecker` / gate rule arm.
    Deterministic,
    /// A third-party static-analysis tool (Semgrep, Bandit, gosec, …) Camerata shells out to.
    ExternalTool,
    /// The LLM-judged semantic audit tier.
    Semantic,
}

/// One rule's outcome for this scan. The load-bearing invariant the rest of the pipeline
/// must respect: a rule is "verified clean" ONLY when `ran && findings_emitted == 0`. A rule
/// with `ran == false` is "not run (reason)" — NEVER clean, NEVER silently folded into
/// "excluded from this audit" without a reason attached.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleLedgerEntry {
    pub rule_id: String,
    pub tier: RuleTier,
    pub ran: bool,
    /// Why this rule did not run: "not armed for this repo", "no matching files", "tool
    /// absent", "gated off", or (the W1-item-4 defect) "declares mechanical/architectural
    /// enforcement but has no wired detector". `None` when `ran` is true, or (rare) when a
    /// rule was recorded as not-run without a caller-supplied reason — the ledger still
    /// refuses to call it clean either way.
    pub skip_reason: Option<String>,
    pub files_evaluated: usize,
    pub findings_emitted: usize,
}

impl RuleLedgerEntry {
    /// The one predicate every "what's healthy" / scorecard "Clean" derivation must use
    /// instead of inferring cleanliness from mere selection.
    pub fn verified_clean(&self) -> bool {
        self.ran && self.findings_emitted == 0
    }
}

/// One finding that was absorbed into another row during a merge stage, with the id of the
/// row it was merged INTO — the "merged-into (with target)" breakdown entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergedRow {
    pub rule_id: String,
    pub target_rule_id: String,
}

/// The explicit disposition breakdown a stage instrumentation site supplies for every row
/// that did NOT survive as its own output row. Any row not covered by one of these buckets
/// becomes `unaccounted` in the resulting [`StageLedgerEntry`] — see
/// [`ScanLedger::record_stage`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StageAccounting {
    pub merged_into: Vec<MergedRow>,
    pub routed_held: usize,
    pub routed_informational: usize,
    pub deduped: usize,
    /// Escape hatch for a stage-specific disposition that doesn't fit the four named buckets
    /// above (e.g. report_export's "excluded as a false positive" or "carved out to the
    /// dependency snapshot") — `(label, count)`. Still fully counted toward `accounted()`, so
    /// using this never masks a real integrity gap; it just names the bucket honestly instead
    /// of forcing it into a wrong-shaped one.
    pub other: Vec<(String, usize)>,
}

impl StageAccounting {
    pub fn accounted(&self) -> usize {
        self.merged_into.len()
            + self.routed_held
            + self.routed_informational
            + self.deduped
            + self.other.iter().map(|(_, n)| n).sum::<usize>()
    }

    pub fn push_merge(&mut self, rule_id: impl Into<String>, target_rule_id: impl Into<String>) {
        self.merged_into.push(MergedRow {
            rule_id: rule_id.into(),
            target_rule_id: target_rule_id.into(),
        });
    }
}

/// One pipeline stage's row-accounting for a single instrumented pass. The invariant:
/// `rows_in == rows_out + merged_into.len() + routed_held + routed_informational + deduped +
/// other-total + unaccounted`. `unaccounted` is computed by [`ScanLedger::record_stage`] as
/// the remainder — never supplied directly — so an instrumentation site cannot accidentally
/// claim a clean reconciliation it didn't actually prove.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StageLedgerEntry {
    pub stage: String,
    pub rows_in: usize,
    pub rows_out: usize,
    pub merged_into: Vec<MergedRow>,
    pub routed_held: usize,
    pub routed_informational: usize,
    pub deduped: usize,
    pub other: Vec<(String, usize)>,
    pub unaccounted: usize,
}

impl StageLedgerEntry {
    pub fn is_integral(&self) -> bool {
        self.unaccounted == 0
    }
}

/// The scan-wide evaluation ledger: per-rule execution facts + per-stage row accounting,
/// accumulated during a scan (see `onboard::audit_repos`, `ai_audit::audit_repo`) and carried
/// into the report build (`report_export::build_report_json`). See the module doc comment.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScanLedger {
    rules: HashMap<String, RuleLedgerEntry>,
    stages: Vec<StageLedgerEntry>,
}

impl ScanLedger {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty() && self.stages.is_empty()
    }

    /// Record (or, for a rule already seen this scan — e.g. a multi-repo run — ACCUMULATE)
    /// one rule's outcome. Accumulation rule: `ran` is OR'd (one repo running it is enough to
    /// call it run), `skip_reason` is kept from the first not-run observation and cleared the
    /// moment any repo actually ran it, and the usage counters sum across repos.
    pub fn record_rule(
        &mut self,
        rule_id: impl Into<String>,
        tier: RuleTier,
        ran: bool,
        skip_reason: Option<String>,
        files_evaluated: usize,
        findings_emitted: usize,
    ) -> &RuleLedgerEntry {
        let rule_id = rule_id.into();
        self.rules
            .entry(rule_id.clone())
            .and_modify(|e| {
                e.ran = e.ran || ran;
                if e.ran {
                    e.skip_reason = None;
                } else if e.skip_reason.is_none() {
                    e.skip_reason = skip_reason.clone();
                }
                e.files_evaluated += files_evaluated;
                e.findings_emitted += findings_emitted;
            })
            .or_insert(RuleLedgerEntry {
                rule_id: rule_id.clone(),
                tier,
                ran,
                skip_reason,
                files_evaluated,
                findings_emitted,
            });
        self.rules.get(&rule_id).expect("just inserted")
    }

    /// Record one stage's row accounting, computing `unaccounted` as the remainder of
    /// `rows_in - rows_out` not explained by `accounting`. Multiple entries for the SAME
    /// stage name are allowed (e.g. one per repo in a multi-repo scan) — each is reconciled
    /// independently. Returns the freshly-pushed entry so the caller can immediately check
    /// `is_integral()` / build a runtime disclosure (see [`stage_disclosure`]).
    pub fn record_stage(
        &mut self,
        stage: impl Into<String>,
        rows_in: usize,
        rows_out: usize,
        accounting: StageAccounting,
    ) -> &StageLedgerEntry {
        let lost = rows_in.saturating_sub(rows_out);
        let accounted = accounting.accounted();
        let unaccounted = lost.saturating_sub(accounted);
        self.stages.push(StageLedgerEntry {
            stage: stage.into(),
            rows_in,
            rows_out,
            merged_into: accounting.merged_into,
            routed_held: accounting.routed_held,
            routed_informational: accounting.routed_informational,
            deduped: accounting.deduped,
            other: accounting.other,
            unaccounted,
        });
        self.stages.last().expect("just pushed")
    }

    pub fn rules(&self) -> impl Iterator<Item = &RuleLedgerEntry> {
        self.rules.values()
    }

    pub fn rule(&self, rule_id: &str) -> Option<&RuleLedgerEntry> {
        self.rules.get(rule_id)
    }

    pub fn stages(&self) -> &[StageLedgerEntry] {
        &self.stages
    }

    /// Rule ids that genuinely ran and emitted zero findings — the ONLY honest definition of
    /// "verified clean" / "What's healthy". Sorted for stable output.
    pub fn healthy_rule_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self
            .rules
            .values()
            .filter(|r| r.verified_clean())
            .map(|r| r.rule_id.clone())
            .collect();
        ids.sort();
        ids
    }

    /// Rule ids that did NOT run this scan, each with its reason — the "excluded from this
    /// audit" section. Sorted by rule id for stable output.
    pub fn excluded_rules(&self) -> Vec<(&str, &str)> {
        let mut out: Vec<(&str, &str)> = self
            .rules
            .values()
            .filter(|r| !r.ran)
            .map(|r| {
                (
                    r.rule_id.as_str(),
                    r.skip_reason.as_deref().unwrap_or("did not run this scan"),
                )
            })
            .collect();
        out.sort_by_key(|(id, _)| *id);
        out
    }

    /// Every rule id that emitted at least one finding this scan — a category/scorecard
    /// badge may never read "Clean" for a rule in this set, regardless of whether that
    /// finding survived as its own row or was folded into another row's `also_matches` by a
    /// later merge stage (findings are counted here at DETECTION time, before any merge).
    pub fn fired_rule_ids(&self) -> HashSet<String> {
        self.rules
            .values()
            .filter(|r| r.findings_emitted > 0)
            .map(|r| r.rule_id.clone())
            .collect()
    }

    /// Sum of every stage's `unaccounted` count — zero on a fully-reconciled scan.
    pub fn total_unaccounted(&self) -> usize {
        self.stages.iter().map(|s| s.unaccounted).sum()
    }
}

/// A runtime disclosure for one stage's integrity gap, or `None` when the stage reconciled
/// cleanly. Rides the EXISTING `FailedPass` mechanism (`crate::ai_audit::FailedPass`) into
/// the report's methodology/executive-summary disclosure list — never a refused export, never
/// a dropped row, just an honest statement that something didn't add up.
pub fn stage_disclosure(
    repo_label: &str,
    stage: &StageLedgerEntry,
) -> Option<crate::ai_audit::FailedPass> {
    if stage.unaccounted == 0 {
        return None;
    }
    Some(crate::ai_audit::FailedPass {
        repo: repo_label.to_string(),
        pass: format!("pipeline integrity ({})", stage.stage),
        reason: format!(
            "{} of {} row(s) entering this stage have no recorded disposition (rows_out={}, \
             merged={}, held={}, informational={}, deduped={})",
            stage.unaccounted,
            stage.rows_in,
            stage.rows_out,
            stage.merged_into.len(),
            stage.routed_held,
            stage.routed_informational,
            stage.deduped,
        ),
    })
}

/// A runtime disclosure for a rule that declared mechanical/architectural enforcement but has
/// no wired detector (the W1-item-4 defect, closed at build time by `crate::mechanical_gate`
/// for the known corpus — this is the scan-time symptom-level backstop for any rule that
/// slips through, e.g. a future corpus addition before the next build-gate run). Deliberately
/// NOT raised for every not-run rule — an ordinary, honestly-scoped skip ("AI review not
/// requested this run", "deterministic scan deselected") is not a pipeline defect, just a
/// disclosed scope choice already reflected in "excluded from this audit"; only the
/// no-detector case is a genuine integrity gap worth its own alarm.
pub fn rule_disclosure(
    repo_label: &str,
    rule: &RuleLedgerEntry,
) -> Option<crate::ai_audit::FailedPass> {
    if rule.ran {
        return None;
    }
    let reason = rule.skip_reason.as_deref().unwrap_or("");
    if !reason.contains("no wired detector") {
        return None;
    }
    Some(crate::ai_audit::FailedPass {
        repo: repo_label.to_string(),
        pass: "mechanical-enforcement coverage".to_string(),
        reason: format!("{} {}", rule.rule_id, reason),
    })
}

/// One stage/merge call site's before/after sample, packaged for a caller (e.g.
/// `ai_audit::audit_repo`) that cannot hold a live `&mut ScanLedger` across an API boundary
/// without a disruptive signature change — the caller returns a `Vec<StageSample>` alongside
/// its normal result, and the orchestrator folds each sample into its own ledger via
/// [`ScanLedger::record_stage_sample`]. Mirrors the existing `FailedPass` accumulation
/// pattern (`Vec<FailedPass>` returned alongside findings) already used throughout
/// `ai_audit.rs` for the identical reason.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StageSample {
    pub stage: String,
    pub rows_in: usize,
    pub rows_out: usize,
    pub accounting: StageAccounting,
}

impl ScanLedger {
    pub fn record_stage_sample(&mut self, sample: StageSample) -> &StageLedgerEntry {
        self.record_stage(
            sample.stage,
            sample.rows_in,
            sample.rows_out,
            sample.accounting,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Rule-level ───────────────────────────────────────────────────────────────────────

    #[test]
    fn a_rule_that_ran_with_zero_findings_is_healthy() {
        let mut ledger = ScanLedger::new();
        ledger.record_rule(
            "SEC-NO-HARDCODED-SECRETS-1",
            RuleTier::Deterministic,
            true,
            None,
            10,
            0,
        );
        assert_eq!(
            ledger.healthy_rule_ids(),
            vec!["SEC-NO-HARDCODED-SECRETS-1".to_string()]
        );
        assert!(ledger.excluded_rules().is_empty());
        assert!(ledger.fired_rule_ids().is_empty());
    }

    #[test]
    fn a_rule_that_did_not_run_is_excluded_with_a_reason_never_healthy() {
        let mut ledger = ScanLedger::new();
        ledger.record_rule(
            "PYTHON-PARAMETERIZED-SQL-1",
            RuleTier::Deterministic,
            false,
            Some("declares mechanical enforcement but has no wired detector".to_string()),
            0,
            0,
        );
        assert!(ledger.healthy_rule_ids().is_empty());
        let excluded = ledger.excluded_rules();
        assert_eq!(excluded.len(), 1);
        assert_eq!(excluded[0].0, "PYTHON-PARAMETERIZED-SQL-1");
        assert!(excluded[0].1.contains("no wired detector"));
    }

    #[test]
    fn a_rule_that_fired_is_never_healthy_and_counts_as_fired() {
        let mut ledger = ScanLedger::new();
        ledger.record_rule(
            "ARCH-STRICT-LAYERING-1",
            RuleTier::Deterministic,
            true,
            None,
            5,
            2,
        );
        assert!(ledger.healthy_rule_ids().is_empty());
        assert!(ledger.excluded_rules().is_empty());
        assert!(ledger.fired_rule_ids().contains("ARCH-STRICT-LAYERING-1"));
    }

    /// A rule absorbed into another row's `also_matches` by a merge stage still genuinely
    /// fired — the ledger records `findings_emitted` at DETECTION time (before any merge), so
    /// it stays in `fired_rule_ids` / out of `healthy_rule_ids` regardless of what a later
    /// merge stage does to the finding's surviving shape. Mirrors the C5-1 regression this
    /// generalizes (`report_export.rs`'s `matched_rule_ids_by_category` doc comment).
    #[test]
    fn a_merged_away_rule_still_counts_as_fired_never_healthy() {
        let mut ledger = ScanLedger::new();
        // Detected at the semantic tier before merge absorbs it into the deterministic primary.
        ledger.record_rule("AI-SITE-DEFECT-1", RuleTier::Semantic, true, None, 3, 1);
        // The merge stage itself: one row in, one row out (absorbed into the primary).
        let mut acc = StageAccounting::default();
        acc.push_merge("AI-SITE-DEFECT-1", "SEC-DETERMINISTIC-SITE-1");
        ledger.record_stage("cross-family-merge", 2, 1, acc);
        assert!(
            ledger.fired_rule_ids().contains("AI-SITE-DEFECT-1"),
            "a merged-away rule must still count as fired"
        );
        assert!(!ledger
            .healthy_rule_ids()
            .contains(&"AI-SITE-DEFECT-1".to_string()));
    }

    #[test]
    fn accumulates_across_repos_ran_wins_and_counters_sum() {
        let mut ledger = ScanLedger::new();
        ledger.record_rule(
            "UI-UTC-DATES-1",
            RuleTier::Deterministic,
            false,
            Some("no matching files".to_string()),
            0,
            0,
        );
        ledger.record_rule("UI-UTC-DATES-1", RuleTier::Deterministic, true, None, 7, 1);
        let entry = ledger.rule("UI-UTC-DATES-1").unwrap();
        assert!(
            entry.ran,
            "one repo running it is enough to call it run overall"
        );
        assert_eq!(
            entry.skip_reason, None,
            "ran clears any earlier skip reason"
        );
        assert_eq!(entry.files_evaluated, 7);
        assert_eq!(entry.findings_emitted, 1);
    }

    // ── Stage-level ──────────────────────────────────────────────────────────────────────

    #[test]
    fn a_fully_explained_stage_has_zero_unaccounted() {
        let mut ledger = ScanLedger::new();
        let mut acc = StageAccounting::default();
        acc.push_merge("AI-FOO-1", "ARCH-BAR-1");
        acc.routed_held = 2;
        acc.routed_informational = 1;
        acc.deduped = 1;
        // rows_in=10, rows_out=5 -> 5 lost, and 1+2+1+1=5 accounted.
        let entry = ledger.record_stage("test-stage", 10, 5, acc);
        assert_eq!(entry.unaccounted, 0);
        assert!(entry.is_integral());
        assert_eq!(ledger.total_unaccounted(), 0);
    }

    /// The core integrity check: a stage that loses a row with NO recorded disposition for it
    /// must produce `unaccounted > 0`, never silently reconcile.
    #[test]
    fn a_stage_that_loses_a_row_with_no_disposition_is_unaccounted() {
        let mut ledger = ScanLedger::new();
        let acc = StageAccounting::default(); // nothing explained
        let entry = ledger.record_stage("lossy-stage", 10, 8, acc); // 2 rows vanished
        assert_eq!(entry.unaccounted, 2);
        assert!(!entry.is_integral());
        assert_eq!(ledger.total_unaccounted(), 2);
    }

    #[test]
    fn stage_disclosure_is_none_when_integral_and_some_when_not() {
        let mut ledger = ScanLedger::new();
        let clean = ledger
            .record_stage("clean-stage", 5, 5, StageAccounting::default())
            .clone();
        assert!(stage_disclosure("acme/app", &clean).is_none());

        let lossy = ledger
            .record_stage("lossy-stage", 5, 3, StageAccounting::default())
            .clone();
        let fp = stage_disclosure("acme/app", &lossy).expect("must disclose a lossy stage");
        assert_eq!(fp.repo, "acme/app");
        assert!(fp.pass.contains("lossy-stage"));
        assert!(
            fp.reason.contains('2'),
            "must name the unaccounted count: {}",
            fp.reason
        );
    }

    #[test]
    fn rule_disclosure_fires_only_for_the_no_detector_reason() {
        let mut ledger = ScanLedger::new();
        ledger.record_rule(
            "RUBY-RAILS-NO-STRING-SQL-1",
            RuleTier::Deterministic,
            false,
            Some("declares mechanical enforcement but has no wired detector".to_string()),
            0,
            0,
        );
        ledger.record_rule(
            "SEC-NO-HARDCODED-SECRETS-1",
            RuleTier::Deterministic,
            false,
            Some("deterministic scan deselected for this run".to_string()),
            0,
            0,
        );
        let phantom = ledger.rule("RUBY-RAILS-NO-STRING-SQL-1").unwrap();
        let ordinary_skip = ledger.rule("SEC-NO-HARDCODED-SECRETS-1").unwrap();
        assert!(rule_disclosure("acme/app", phantom).is_some());
        assert!(
            rule_disclosure("acme/app", ordinary_skip).is_none(),
            "an ordinary, honestly-scoped skip is not a pipeline-integrity alarm"
        );
    }

    #[test]
    fn empty_ledger_has_no_unaccounted_rows() {
        let ledger = ScanLedger::new();
        assert!(ledger.is_empty());
        assert_eq!(ledger.total_unaccounted(), 0);
        assert!(ledger.healthy_rule_ids().is_empty());
        assert!(ledger.excluded_rules().is_empty());
    }
}
