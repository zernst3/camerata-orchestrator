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

/// The `Finding::captures` key holding the operator-only detail of a held severity: which
/// reviewer doubted the row and which floor was held back, one sentence per line. A client
/// package never prints it (`severity_adjustments` are the client's notes and say only "Severity
/// held at <level> pending a decision."); the internal workbook shows it in its triage columns
/// ([`crate::report_export::internal_triage`]), the way it shows the escalation reason.
pub const SEVERITY_HOLD_DETAIL_CAPTURE: &str = "severity_hold_detail";

/// The `Finding::captures` key marking a row that a source nobody doubted was folded into (a
/// scan-tool row collapsed into it, or an identical row the reviewers did not doubt). Such a row
/// counts as undoubted for severity: the minimum and the merged severity apply (R-7, owner
/// ruling on a merge of a doubted and an undoubted source), and the row stays held.
pub const UNDOUBTED_SOURCE_CAPTURE: &str = "undoubted_source_folded";

/// Whether a source nobody doubted is folded into `f`: the marker a merge leaves, or a scan-tool
/// row among the rows collapsed into it (`flagged_by` with a tool: a tool's row never went
/// through a reviewer stage).
pub fn undoubted_source_folded_in(f: &Finding) -> bool {
    f.captures.contains_key(UNDOUBTED_SOURCE_CAPTURE)
        || f.flagged_by.iter().any(|fb| fb.tool.is_some())
}

/// Mark `f` as holding a source nobody doubted.
pub fn mark_undoubted_source_folded(f: &mut Finding) {
    f.captures
        .insert(UNDOUBTED_SOURCE_CAPTURE.to_string(), "1".to_string());
}

/// The doubt a severity floor must respect: [`reviewer_doubt`], unless a source nobody doubted is
/// folded into the row ([`undoubted_source_folded_in`]), when the row counts as undoubted for
/// severity.
pub fn doubt_for_severity(f: &Finding) -> Option<Doubt> {
    reviewer_doubt(f).filter(|_| !undoubted_source_folded_in(f))
}

/// Record that `f` is held at its severity: the neutral sentence a client reads (always kept,
/// see [`crate::severity_bounds::record_adjustment`]) and the operator-only `detail`. Does not
/// set `needs_review`; the caller does, so the hold stays visible where it is decided.
pub(crate) fn record_hold(f: &mut Finding, detail: &str) {
    crate::severity_bounds::record_adjustment(f, held_note(&f.severity));
    let entry = f
        .captures
        .entry(SEVERITY_HOLD_DETAIL_CAPTURE.to_string())
        .or_default();
    if entry.lines().any(|l| l == detail) {
        return;
    }
    if !entry.is_empty() {
        entry.push('\n');
    }
    entry.push_str(detail);
}

/// The sentence a client reads when a row's severity is held.
pub(crate) fn held_note(severity: &str) -> String {
    format!("{HELD_NOTE_PREFIX}{severity} pending a decision")
}

/// How every held-severity note starts (see [`held_note`]).
pub(crate) const HELD_NOTE_PREFIX: &str = "severity held at ";

/// How the note of a row returned to its reviewed severity starts (see [`returned_note`]).
pub(crate) const RETURNED_NOTE_PREFIX: &str = "severity returned to ";

/// The one sentence a client reads when a floor raised a row and a reviewer's doubt sent it back
/// to its reviewed severity. It replaces the raise note and the held note; the raise goes to the
/// operator-only hold detail.
pub(crate) fn returned_note(severity: &str) -> String {
    format!("{RETURNED_NOTE_PREFIX}{severity} pending a decision")
}

/// Whether `note` says a row is held for a decision (held in place, or returned and held). Such a
/// note is never dropped when a row is full.
pub(crate) fn is_hold_note(note: &str) -> bool {
    note.starts_with(HELD_NOTE_PREFIX) || note.starts_with(RETURNED_NOTE_PREFIX)
}

/// `source` was just folded into `row` by a merge. A source nobody doubted marks the row as
/// holding one ([`UNDOUBTED_SOURCE_CAPTURE`]); a source a reviewer doubted, folded into a row
/// nobody doubted, holds the row with a note that one source was doubted. Either way a doubted
/// row that now holds an undoubted source is held ([`hold_partly_doubted`]).
pub fn fold_source_doubt(row: &mut Finding, source: &Finding) {
    match reviewer_doubt(source) {
        None => mark_undoubted_source_folded(row),
        Some(doubt) if reviewer_doubt(row).is_none() => {
            row.needs_review = true;
            record_hold(
                row,
                &format!(
                    "one source folded into this row was doubted ({}); another was not, so the \
                     minimum severity and the merged severity apply and the row is held for a \
                     decision",
                    doubt.phrase()
                ),
            );
        }
        Some(_) => {}
    }
    hold_partly_doubted(row);
}

/// A row a reviewer doubted that a source nobody doubted was folded into: the floor and the
/// merged severity apply, and the row is held for a decision with a detail saying one source was
/// doubted. Idempotent. Run wherever the bounds run, so a merge before the reviewers and a
/// reviewer after the merge reach the same row.
pub fn hold_partly_doubted(f: &mut Finding) {
    let Some(doubt) = reviewer_doubt(f) else {
        return;
    };
    if !undoubted_source_folded_in(f) {
        return;
    }
    f.needs_review = true;
    record_hold(
        f,
        &format!(
            "one source folded into this row was doubted ({}); another was not, so the \
             minimum severity and the merged severity apply and the row is held for a decision",
            doubt.phrase()
        ),
    );
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
/// 1. **Kind follows the claim** ([`security_deviation_becomes_defect`]). A deviation the group
///    reviewer gave a security or data-loss reason is a defect.
/// 2. **A doubted row returns to stage 1's severity** ([`return_doubted_row_to_reviewed_severity`]),
///    when a floor is what raised it and no source nobody doubted is folded into it.
///    The corpus floor ran before the group reviewer read the rows (it has to: the reviewer
///    ranks by the shown severity), so a row only the group reviewer doubted was already raised.
/// 3. **The corpus bounds run again**, so the ceilings have the last word over the severities
///    this pass wrote, and a held floor leaves its note and its hold. The bounds are idempotent,
///    so a row this pass did not touch is unchanged.
pub fn follow_reviewers(findings: &mut [Finding], corpus: Option<&camerata_rules::RuleSet>) {
    for f in findings.iter_mut() {
        security_deviation_becomes_defect(f);
        return_doubted_row_to_reviewed_severity(f, corpus);
    }
    crate::severity_bounds::apply_scan_severity_bounds(findings, corpus);
}

/// Whether `reason` is one of the closed list's security or data-loss reasons: the security
/// flaws ([`EscalationReason::is_security_flaw`]: unauthenticated reach, secret exposure,
/// cross-tenant access, authorization bypass, injection) and data loss.
pub fn is_security_or_data_loss(reason: EscalationReason) -> bool {
    reason.is_security_flaw() || reason == EscalationReason::DataLoss
}

/// Cause 4 of the 2026-11-15 false-alarm diagnosis: kind followed the rule id, not the claim.
/// Every row of a multi-option rule with a decided convention is filed a
/// [`FindingKind::Deviation`] (`ai_audit::assign_deviation_kinds`) and capped at the deviation
/// ceiling, so "PATCH updates any record by id without authorizing ownership" shipped as a
/// consistency deviation capped at medium.
///
/// After the group reviewer: a Deviation whose row carries a security or data-loss escalation
/// reason ([`is_security_or_data_loss`], read from the closed list the group reviewer writes on
/// the row, never from its text) and that stage 1 rated becomes a [`FindingKind::Defect`], at any
/// severity (owner ruling, consistent with A1: a row stage 1 rated low is a defect too). It is a
/// defect BEFORE the deviation cap: the cap ran before the group reviewer, so the severity it took
/// off (read back from the cap's own note) is restored, and the bounds that run next do not cap a
/// defect. The note says why; the cap's earlier note stays, so the row's history is whole.
///
/// The review tree already turns a convention row's security reason into `rule_misfit` when a
/// row of a security rule shares its site, so a reason still here is the reviewer's view of this
/// row alone. Untouched: any other kind, a reason off the list, and no reason.
fn security_deviation_becomes_defect(f: &mut Finding) {
    use camerata_api_types::audit::FindingKind;
    use camerata_rules::severity_bounds::{
        deviation_cap_lowered_from, normalize_severity, severity_rank,
    };
    if !matches!(f.kind, Some(FindingKind::Deviation(_))) {
        return;
    }
    let Some(reason) = f
        .review
        .escalation_reason
        .filter(|r| is_security_or_data_loss(*r))
    else {
        return;
    };
    f.kind = Some(FindingKind::Defect);
    // This sentence reaches the client: it names the problem, not the reviewer stage.
    let why = format!(
        "{} was named at this site, so the row is a defect rather than a consistency \
         deviation, and the deviation cap does not apply",
        reason.phrase()
    );
    let shown = normalize_severity(&f.severity);
    let capped_from = crate::severity_bounds::recorded_adjustments(f)
        .iter()
        .rev()
        .find_map(|n| deviation_cap_lowered_from(n).map(normalize_severity));
    match capped_from {
        Some(from) if severity_rank(&from) < severity_rank(&shown) => {
            crate::ai_audit::record_severity_adjustment(f, &shown, &from, &why);
            f.severity = from;
        }
        _ => crate::severity_bounds::record_adjustment(
            f,
            format!("kind changed from deviation to defect: {why}"),
        ),
    }
}

/// R-7 after the group reviewer: a row a reviewer doubted whose shown severity came from a floor
/// (the rule's minimum or the phrase floor) goes back to stage 1's severity and is held for a
/// decision (`needs_review`). The client reads "Severity held at <level> pending a decision."; the
/// operator's detail says which reviewer doubted it ([`SEVERITY_HOLD_DETAIL_CAPTURE`]).
///
/// "Came from a floor" means the shown severity is above stage 1's and no more severe than the
/// strongest floor that applies to the row ([`strongest_floor`]); a row more severe than every
/// floor was raised by something else and the ruling does not reach it.
///
/// A source nobody doubted folded into the row (a scan tool's row, an identical row the reviewers
/// did not doubt) makes the row undoubted for severity: it keeps the merged severity, the floor
/// applies, and it is held with a note that one source was doubted ([`hold_partly_doubted`]).
///
/// A row with no stage 1 severity (only the group reviewer spoke on it) cannot be returned to a
/// severity nobody recorded: when a floor touches it, it keeps its severity and is held.
///
/// Untouched: a row nobody doubted; a row at or below stage 1's severity; a row no floor
/// touches (not held here: the possible-false-positive flag marks it); and a row whose
/// severity the human reviewer set, which is theirs.
fn return_doubted_row_to_reviewed_severity(
    f: &mut Finding,
    corpus: Option<&camerata_rules::RuleSet>,
) {
    use camerata_rules::severity_bounds::{normalize_severity, severity_rank};
    let Some(doubt) = reviewer_doubt(f) else {
        return;
    };
    if undoubted_source_folded_in(f) {
        hold_partly_doubted(f);
        return;
    }
    if crate::report_export::overlay::reviewer_edited_field(
        f,
        camerata_api_types::edits::EditField::Severity,
    ) {
        return;
    }
    let Some(floor) = strongest_floor(f, corpus) else {
        return;
    };
    let shown = normalize_severity(&f.severity);
    // Lower rank is MORE severe here (the rules crate's vocabulary). A shown severity above
    // every floor did not come from one.
    if severity_rank(&shown) < severity_rank(&floor) {
        return;
    }
    match crate::ai_audit::stage1_of(f).and_then(|s| s.severity) {
        Some(reviewed) => {
            let reviewed = normalize_severity(&reviewed);
            if severity_rank(&shown) >= severity_rank(&reviewed)
                || severity_rank(&floor) >= severity_rank(&reviewed)
            {
                return;
            }
            f.severity = reviewed.clone();
            f.needs_review = true;
            // The client reads one sentence. The raise notes that put the row at `shown` go to
            // the operator-only detail, and the held note is not written twice.
            let raises = take_raise_notes(f, &shown);
            crate::severity_bounds::record_adjustment(f, returned_note(&reviewed));
            let raised = if raises.is_empty() {
                String::new()
            } else {
                format!(" ({})", raises.join("; "))
            };
            record_hold(
                f,
                &format!(
                    "{}, so no minimum severity raises it; it keeps the reviewed severity \
                     ({reviewed}) instead of {shown}{raised} and is held for a decision",
                    doubt.phrase()
                ),
            );
        }
        None => {
            f.needs_review = true;
            record_hold(
                f,
                &format!(
                    "{}, and no reviewed severity was recorded, so the row keeps {shown} and is \
                     held for a decision",
                    doubt.phrase()
                ),
            );
        }
    }
}

/// Remove from `f`'s recorded adjustments the notes that raised it to `to`, and return them.
fn take_raise_notes(f: &mut Finding, to: &str) -> Vec<String> {
    let suffix = format!(" to {to}");
    let mut taken = Vec::new();
    let mut kept: Vec<String> = Vec::new();
    for note in crate::severity_bounds::recorded_adjustments(f) {
        let head = note.split(':').next().unwrap_or("");
        if note.starts_with("severity raised from ") && head.ends_with(&suffix) {
            taken.push(note);
        } else {
            kept.push(note);
        }
    }
    if !taken.is_empty() {
        f.captures.insert(
            crate::severity_bounds::SEVERITY_ADJUSTMENTS_CAPTURE.to_string(),
            kept.join("\n"),
        );
    }
    taken
}

/// The strongest floor that applies to `f`: the rule's minimum or the phrase floor, whichever
/// ranks higher, normalized; `None` when neither applies. The two floors the ruling names.
/// Reviewer doubt is deliberately not consulted: this asks what a floor could have done.
fn strongest_floor(f: &Finding, corpus: Option<&camerata_rules::RuleSet>) -> Option<String> {
    use camerata_rules::severity_bounds::{normalize_severity, severity_rank};
    let rule_floor = crate::report_export::corpus_rule_for(&f.rule_id, corpus)
        .filter(|_| !crate::ai_audit::is_agent_finder_row(f))
        .and_then(|r| r.min_severity.as_deref())
        .map(normalize_severity);
    let phrase_floor =
        crate::ai_audit::severity_calibration_floor(f).map(|(floor, _)| normalize_severity(floor));
    [rule_floor, phrase_floor]
        .into_iter()
        .flatten()
        .min_by_key(|floor| severity_rank(floor))
}

#[cfg(test)]
#[path = "follow_reviewers_tests.rs"]
mod tests;
