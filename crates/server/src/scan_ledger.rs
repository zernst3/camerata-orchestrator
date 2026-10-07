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
    /// Camerata's own code: the content floor (`onboard::audit_files`) — the always-on
    /// platform/floor regex set run unconditionally over every file.
    Deterministic,
    /// Camerata's own code: the deterministic architectural engine — a registered
    /// `ArchChecker` or the gateway's own rule registry arm (`onboard::audit_architectural`).
    /// Split out from [`RuleTier::Deterministic`] (CLI-inspect ledger-summary work) so a
    /// human-readable family breakdown can tell "the content floor ran" apart from "the
    /// architectural engine ran" instead of folding both into one undifferentiated bucket.
    Architectural,
    /// A third-party static-analysis tool (Semgrep, Bandit, gosec, …) Camerata shells out to.
    ExternalTool,
    /// The LLM-judged semantic audit tier.
    Semantic,
}

impl RuleTier {
    /// Human-readable family label for this tier, used by `camerata inspect`'s ledger summary
    /// (`crates/cli/src/inspect_cmd.rs::render_ledger_summary`) to group rules the way an
    /// operator actually reasons about coverage ("did the taint pass run at all?") rather than
    /// by raw enum name.
    pub fn family_label(&self) -> &'static str {
        match self {
            RuleTier::Deterministic => "Deterministic platform/floor checks",
            RuleTier::Architectural => "Architectural checks",
            RuleTier::ExternalTool => "External-tool / taint (Semgrep, etc.)",
            RuleTier::Semantic => "Semantic / advisory tier",
        }
    }

    /// Every family, in the fixed display order the ledger summary renders them — so a family
    /// with ZERO recorded rules still gets its own line instead of being silently omitted (see
    /// `render_ledger_summary`'s doc comment).
    pub fn all() -> [RuleTier; 4] {
        [
            RuleTier::Deterministic,
            RuleTier::Architectural,
            RuleTier::ExternalTool,
            RuleTier::Semantic,
        ]
    }
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

    /// W2: the total of every NAMED disposition this entry recorded (merged + held +
    /// informational + deduped + the `other` escape-hatch buckets) — i.e. `rows_in - rows_out`
    /// minus whatever `unaccounted` already had to absorb. Mirrors [`StageAccounting::accounted`]
    /// but over the already-reconciled, persisted entry, so a caller (or test) can state the
    /// `rows_in == rows_out + accounted_total() + unaccounted` identity explicitly instead of
    /// only checking `unaccounted == 0`.
    pub fn accounted_total(&self) -> usize {
        self.merged_into.len()
            + self.routed_held
            + self.routed_informational
            + self.deduped
            + self.other.iter().map(|(_, n)| n).sum::<usize>()
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

    /// Forcibly OVERWRITE a rule's `ran`/`skip_reason`/`findings_emitted` facts — unlike
    /// [`record_rule`](Self::record_rule) (which OR-accumulates `ran` across repos within a
    /// scan, appropriate for a multi-repo run), this REPLACES whatever was recorded.
    ///
    /// # Why this exists (W3)
    ///
    /// The scan-time external-tool pass (`crate::merge_scan_preview` / `scan_tools::run_scan_tools`)
    /// runs in a SEPARATE, later stage than the one that first populates the ledger
    /// (`onboard::audit_repos`'s CI-tier rule loop, which optimistically records a
    /// semgrep-mapped rule as `ran = true, findings_emitted = 0` on the strength of "a semgrep
    /// rule EXISTS for this corpus rule," before the tool has actually run for this scan).
    /// That optimism is sometimes wrong — the tool can be absent, fail to provision, or error
    /// out entirely — and by the time the real pass's result is known, `record_rule`'s
    /// OR-accumulate semantics can no longer walk `ran` back to `false` (by design: it must
    /// never let a later "it didn't run here" silently erase an earlier real "it ran there").
    /// This method is the narrow, explicit escape hatch for that one correction: the caller
    /// (`crate::reconcile_external_tool_ledger`) uses it ONLY after the external-tool pass has
    /// actually completed, to replace a speculative pre-pass entry with the real outcome —
    /// never to downgrade a rule that genuinely ran somewhere.
    ///
    /// Preserves the existing entry's `tier`/`files_evaluated` when one is already present
    /// (there usually is, from the pre-pass optimistic record); defaults to
    /// [`RuleTier::ExternalTool`] / `0` for a rule_id the ledger hasn't seen at all yet.
    pub fn correct_rule_after_external_pass(
        &mut self,
        rule_id: impl Into<String>,
        ran: bool,
        skip_reason: Option<String>,
        findings_emitted: usize,
    ) -> &RuleLedgerEntry {
        let rule_id = rule_id.into();
        let (tier, files_evaluated) = self
            .rules
            .get(&rule_id)
            .map(|e| (e.tier, e.files_evaluated))
            .unwrap_or((RuleTier::ExternalTool, 0));
        self.rules.insert(
            rule_id.clone(),
            RuleLedgerEntry {
                rule_id: rule_id.clone(),
                tier,
                ran,
                skip_reason,
                files_evaluated,
                findings_emitted,
            },
        );
        self.rules.get(&rule_id).expect("just inserted")
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

/// The exact substring [`rule_disclosure`] keys on to recognize the W1-item-4 "declared
/// mechanical/architectural enforcement but nothing ever evaluates it" defect shape.
///
/// W3 hardening: this used to be a bare string literal duplicated at the call site
/// (`onboard::audit_repos`'s skip-reason constructor) and the check site (`rule_disclosure`
/// below) — a future wording edit to either copy would silently decouple the two and the
/// disclosure would stop firing with no test failure pointing at why. Every producer of a
/// "no detector" skip reason MUST embed this constant verbatim (not retype the phrase), and
/// `rule_disclosure` checks against the SAME constant, so the two can never drift apart.
pub const NO_WIRED_DETECTOR_REASON: &str = "no wired detector";

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
    if !reason.contains(NO_WIRED_DETECTOR_REASON) {
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

    /// W4 (root-cause fix) regression: a corpus rule that declares `mechanical` enforcement
    /// but ships NO deterministic detector is, as of W4, ALWAYS routed to the semantic/AI pass
    /// (see `onboard::semantic_rule_ids_for_repo`). This mirrors the EXACT `record_rule` call
    /// sequence `onboard::audit_repos`'s loop makes for that case: NOTHING is recorded during
    /// the deterministic phase (no detector channel matched, so that loop skips it entirely —
    /// see its doc comment), and the semantic phase records it directly as `RuleTier::Semantic`
    /// with the model's real finding count once the AI pass actually runs. The rule must land
    /// as genuinely FIRED (not healthy), and it must NEVER be recorded as excluded/not-run —
    /// the old defect this whole fix closes was exactly a no-detector rule ending up "checked"
    /// in name only; the new behavior is that it is checked for real, by the model.
    #[test]
    fn a_mechanical_rule_with_no_detector_that_the_model_evaluated_is_recorded_as_fired_semantic_never_excluded(
    ) {
        let mut ledger = ScanLedger::new();
        // The semantic-phase recording `onboard::audit_repos` makes once `audit_repo` returns:
        // one real finding for this id, at the Semantic tier.
        ledger.record_rule(
            "RUBY-FROZEN-STRING-LITERAL-1",
            RuleTier::Semantic,
            true,
            None,
            3, // files_evaluated
            1, // the model's one real finding
        );
        let entry = ledger
            .rule("RUBY-FROZEN-STRING-LITERAL-1")
            .expect("just recorded");
        assert!(entry.ran, "the semantic pass genuinely ran this rule");
        assert_eq!(entry.tier, RuleTier::Semantic);
        assert!(
            !entry.verified_clean(),
            "a rule that fired must never read as verified clean"
        );
        assert!(
            ledger.excluded_rules().is_empty(),
            "a rule the model actually ran must never appear as excluded/not-run"
        );
        assert!(
            ledger
                .fired_rule_ids()
                .contains("RUBY-FROZEN-STRING-LITERAL-1"),
            "the rule must be recorded as genuinely fired"
        );
    }

    /// The companion zero-findings case: the SAME no-detector mechanical rule, model-reviewed,
    /// came back clean this run. It is honestly healthy (the model looked and found nothing —
    /// a true claim), tagged `RuleTier::Semantic` (never silently implied to be a mechanical
    /// check), and critically still NEVER appears in `excluded_rules()`.
    #[test]
    fn a_mechanical_rule_with_no_detector_that_the_model_reviewed_clean_is_healthy_via_semantic_tier(
    ) {
        let mut ledger = ScanLedger::new();
        ledger.record_rule(
            "RUBY-FROZEN-STRING-LITERAL-1",
            RuleTier::Semantic,
            true,
            None,
            3,
            0,
        );
        let entry = ledger
            .rule("RUBY-FROZEN-STRING-LITERAL-1")
            .expect("just recorded");
        assert_eq!(entry.tier, RuleTier::Semantic);
        assert!(entry.verified_clean());
        assert!(ledger
            .healthy_rule_ids()
            .contains(&"RUBY-FROZEN-STRING-LITERAL-1".to_string()));
        assert!(ledger.excluded_rules().is_empty());
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

    /// W2: the semantic/AI tier's own accounting identity, stated as an explicit arithmetic
    /// check (not just `unaccounted == 0`) over BOTH real ai-tier stages
    /// (`ai_audit::audit_repo` records `"ai-location-merge"` and `"ai-calibration"` by these
    /// exact names): `rows_in == rows_out + accounted` for every stage a Semantic-tier rule's
    /// findings pass through. This is the regression contract for the zero-row collapse at the
    /// ledger level — a scan with `rows_in > 0` for the semantic tier can never reconcile to
    /// `rows_out == 0` with nothing accounting for the rest.
    #[test]
    fn semantic_tier_advisory_rows_reconcile_rows_out_plus_accounted_equals_rows_in() {
        let mut ledger = ScanLedger::new();
        ledger.record_rule(
            "AI-CONFIG-HANDLER-DEFECT-1",
            RuleTier::Semantic,
            true,
            None,
            2,
            3,
        );

        // ai-location-merge: 3 raw findings in, 1 deduped away, 2 location-merged into 1 row.
        let mut location_acc = StageAccounting::default();
        location_acc.deduped = 1;
        location_acc.push_merge("AI-DUP-NAME-1", "AI-CONFIG-HANDLER-DEFECT-1");
        let location_entry = ledger.record_stage("ai-location-merge", 3, 1, location_acc);
        assert_eq!(
            location_entry.rows_in,
            location_entry.rows_out + location_entry.accounted_total(),
            "ai-location-merge must reconcile exactly"
        );
        assert_eq!(location_entry.unaccounted, 0);

        // ai-calibration: never drops a row on its own (see `audit_repo`'s doc comment) — the
        // ONE row from above passes straight through.
        let calibration_entry =
            ledger.record_stage("ai-calibration", 1, 1, StageAccounting::default());
        assert_eq!(
            calibration_entry.rows_in,
            calibration_entry.rows_out + calibration_entry.accounted_total(),
            "ai-calibration must reconcile exactly"
        );
        assert_eq!(calibration_entry.unaccounted, 0);

        // The scan-wide identity holds across every stage the semantic tier's rows touched.
        assert_eq!(
            ledger.total_unaccounted(),
            0,
            "a semantic-tier scan with rows_in > 0 must never leave any row unaccounted: {:#?}",
            ledger.stages()
        );
        assert!(
            ledger
                .fired_rule_ids()
                .contains("AI-CONFIG-HANDLER-DEFECT-1"),
            "the rule that produced these rows must be recorded as fired, never silently clean"
        );
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
