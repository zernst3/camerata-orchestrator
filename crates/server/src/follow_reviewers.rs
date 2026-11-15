//! Severity and kind follow the reviewers (owner ruling R-7, 2026-11-15).
//!
//! The engine has two reviewer stages: stage 1 (calibration, one call per site) answers whether a
//! site is real, and stage 2 (the group reviewer) may call a site a rule misfit or a likely false
//! alarm, or disagree with stage 1's `real`. A row either stage doubted is exactly the row a
//! severity floor must not inflate: the floor exists to stop an under-rated defect, and a row a
//! reviewer doubted is not a defect anyone has confirmed.
//!
//! This module holds the one definition of "a reviewer doubted this row" ([`reviewer_doubt`]),
//! read by the corpus floor (through `BoundedFinding::reviewer_doubt`) and by the phrase floor
//! (`ai_audit::apply_severity_calibration_rules`), and the pass that runs once the group reviewer
//! has spoken ([`follow_reviewers`]).
//!
//! Nothing here drops, hides or merges a row. A doubted row stays in the review queue, held for
//! the human reviewer, who is the only one who removes a false alarm.

use crate::onboard::Finding;
use camerata_api_types::review::{DisagreementField, EscalationReason};

/// Which reviewer stage doubted a row, and how.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Doubt {
    /// Stage 1 answered `real: no`.
    Stage1NotReal,
    /// The group reviewer recorded a disagreement with stage 1 whose `real` is `no`.
    GroupReviewerNotReal,
    /// The group reviewer's escalation reason is `rule_misfit`.
    RuleMisfit,
    /// The group reviewer's escalation reason is `likely_false_alarm_at_high_severity`.
    LikelyFalseAlarm,
}

impl Doubt {
    /// The doubt in words, for the row's severity note.
    pub fn phrase(self) -> &'static str {
        match self {
            Self::Stage1NotReal => "the per-site reviewer judged it not real",
            Self::GroupReviewerNotReal => "the group reviewer judged it not real",
            Self::RuleMisfit => "the group reviewer judged the rule a misfit for this site",
            Self::LikelyFalseAlarm => "the group reviewer judged it a likely false alarm",
        }
    }
}

/// Whether a reviewer stage doubted `f`, and which. Read from what the stages already store on
/// the row: stage 1's verdict capture, and the group reviewer's escalation reason and recorded
/// disagreements. No text of the row is read. `unclear` is not doubt: it holds the row already
/// (`needs_review`) and says the code shown does not settle it either way.
pub fn reviewer_doubt(f: &Finding) -> Option<Doubt> {
    if crate::ai_audit::stage1_of(f).is_some_and(|s| s.real == crate::ai_audit::Real::No) {
        return Some(Doubt::Stage1NotReal);
    }
    match f.review.escalation_reason {
        Some(EscalationReason::RuleMisfit) => return Some(Doubt::RuleMisfit),
        Some(EscalationReason::LikelyFalseAlarmAtHighSeverity) => {
            return Some(Doubt::LikelyFalseAlarm)
        }
        _ => {}
    }
    f.review
        .stage2_disagreement
        .iter()
        .any(|d| d.field == DisagreementField::Real && d.mine.trim().eq_ignore_ascii_case("no"))
        .then_some(Doubt::GroupReviewerNotReal)
}

/// The pass that runs once the group reviewer has spoken, over the final row set
/// (`ai_audit::group_review::review_scan_report` calls it last). In place, so it can never change
/// how many rows there are.
///
/// 1. **A doubted row returns to stage 1's severity** ([`return_doubted_row_to_reviewed_severity`]).
///    The corpus floor ran before the group reviewer read the rows (it has to: the reviewer
///    ranks by the shown severity), so a row only the group reviewer doubted was already raised.
/// 2. **The corpus bounds run again**, so the ceilings have the last word over the severities
///    this pass wrote, and a held floor leaves its note and its hold. The bounds are idempotent,
///    so a row this pass did not touch is unchanged.
pub fn follow_reviewers(findings: &mut [Finding], corpus: Option<&camerata_rules::RuleSet>) {
    for f in findings.iter_mut() {
        return_doubted_row_to_reviewed_severity(f, corpus);
    }
    crate::severity_bounds::apply_scan_severity_bounds(findings, corpus);
}

/// R-7 after the group reviewer: a row a reviewer doubted that a floor (the rule's minimum or the
/// phrase floor) raised above the severity stage 1 left on it goes back to stage 1's severity, is
/// held for a decision (`needs_review`) and its severity note says which reviewer doubted it.
///
/// Untouched: a row nobody doubted; a row with no stage 1 severity recorded (a deterministic row,
/// or a run saved before stage 1's severity was kept: nothing says what it was rated, and nothing
/// guesses); a row at or below stage 1's severity; a row no floor would raise from stage 1's
/// severity (what raised it is not a floor, so the ruling does not reach it); and a row whose
/// severity the human reviewer set, which is theirs.
fn return_doubted_row_to_reviewed_severity(
    f: &mut Finding,
    corpus: Option<&camerata_rules::RuleSet>,
) {
    use camerata_rules::severity_bounds::{normalize_severity, severity_rank};
    let Some(doubt) = reviewer_doubt(f) else {
        return;
    };
    if crate::report_export::overlay::reviewer_edited_field(
        f,
        camerata_api_types::edits::EditField::Severity,
    ) {
        return;
    }
    let Some(reviewed) = crate::ai_audit::stage1_of(f).and_then(|s| s.severity) else {
        return;
    };
    let reviewed = normalize_severity(&reviewed);
    let shown = normalize_severity(&f.severity);
    // Lower rank is MORE severe here (the rules crate's vocabulary).
    if severity_rank(&shown) >= severity_rank(&reviewed) {
        return;
    }
    if !a_floor_raises(f, &reviewed, corpus) {
        return;
    }
    crate::ai_audit::record_severity_adjustment(
        f,
        &shown,
        &reviewed,
        &format!(
            "{}, so no minimum severity raises it; it keeps the reviewed severity and is held \
             for a decision",
            doubt.phrase()
        ),
    );
    f.severity = reviewed;
    f.needs_review = true;
}

/// Whether the rule's minimum or the phrase floor ranks above `from` for `f`: the two floors the
/// ruling names. Reviewer doubt is deliberately not consulted: this asks what a floor did.
fn a_floor_raises(f: &Finding, from: &str, corpus: Option<&camerata_rules::RuleSet>) -> bool {
    use camerata_rules::severity_bounds::{normalize_severity, severity_rank};
    let above = |floor: &str| severity_rank(&normalize_severity(floor)) < severity_rank(from);
    let rule_floor = crate::report_export::corpus_rule_for(&f.rule_id, corpus)
        .and_then(|r| r.min_severity.as_deref())
        .is_some_and(above);
    rule_floor
        || crate::ai_audit::severity_calibration_floor(f).is_some_and(|(floor, _)| above(floor))
}

#[cfg(test)]
#[path = "follow_reviewers_tests.rs"]
mod tests;
