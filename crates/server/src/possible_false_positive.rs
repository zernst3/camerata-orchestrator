//! The "Possible false positive" flag: the engine's own reasons to doubt a row, for the operator
//! to look at first (an operator review aid).
//!
//! # What it is
//!
//! The operator reviews every row before a client sees anything, and a false alarm at medium or
//! higher costs the most review time. A row the engine itself has reasons to doubt carries this
//! flag, with each reason from a closed list ([`FpReason`]), so the operator can open those rows
//! first and rule them out quickly.
//!
//! # What it never does
//!
//! The flag never hides, drops, merges or lowers anything, and it changes no severity, kind,
//! escalation or decision. Anything that might leave a real violation unremediated is a defect
//! (the owner's rule): the reviewer, not the engine, removes a false alarm, and the product's
//! word for that is "Ruled out". A flagged row stays a problem until the operator rules it out.
//!
//! # Where it is worked out
//!
//! Once, here, from data the run already holds, with no model call: [`FpIndex::of`]. The review
//! view and the internal exports call it; none works a reason out itself. The flag reaches only
//! the operator: the review UI, the internal workbook's technical band and an internal
//! `findings.json`. A client package carries neither the flag nor its reasons.

use std::collections::HashSet;

use camerata_api_types::edits::EditField;
pub use camerata_api_types::review::{FpReason, PossibleFalsePositive};

use crate::onboard::{Finding, SitePlace};

/// The places of a report's deterministic rows, built once per report, and the flag of each row.
#[derive(Debug, Default)]
pub struct FpIndex {
    /// Where a deterministic row (the floor, a native checker, a tool preview) sits.
    deterministic_places: HashSet<SitePlace>,
}

impl FpIndex {
    /// Index the rows of one report: the rows a review or an export reads.
    pub fn new<'a>(rows: impl IntoIterator<Item = &'a Finding>) -> Self {
        let deterministic_places = rows
            .into_iter()
            .filter(|f| is_deterministic(f))
            .map(Finding::place)
            .collect();
        Self {
            deterministic_places,
        }
    }

    /// The flag of `f`, or `None` when the engine has no reason to doubt it.
    pub fn of(&self, f: &Finding) -> Option<PossibleFalsePositive> {
        let deterministic_peer = self.deterministic_places.contains(&f.place());
        PossibleFalsePositive::from_reasons(reasons_of(f, deterministic_peer))
    }
}

/// Whether a deterministic producer wrote `f` (not the model).
fn is_deterministic(f: &Finding) -> bool {
    crate::ai_audit::finding_origin(f) == crate::ai_audit::Origin::Deterministic
        && !is_reviewer_added(f)
}

/// Whether the reviewer added `f` by hand: the engine has nothing to doubt about it.
fn is_reviewer_added(f: &Finding) -> bool {
    f.captures
        .get(crate::report_export::overlay::PROVENANCE_CAPTURE)
        .is_some_and(|v| v == crate::report_export::overlay::REVIEWER_PROVENANCE)
}

/// Every reason the engine has to doubt `f`. `deterministic_peer` says whether a deterministic row
/// sits at the same place.
pub fn reasons_of(f: &Finding, deterministic_peer: bool) -> Vec<FpReason> {
    if is_reviewer_added(f) {
        return Vec::new();
    }
    let checks = [
        (FpReason::GroupReviewerDoubt, group_reviewer_doubts(f)),
        (FpReason::CalibrationDoubt, calibration_doubts(f)),
        (FpReason::TestCodeOnly, in_test_code_only(f)),
        (
            FpReason::ConventionRuleAtMediumOrHigher,
            convention_rule_at_medium_or_higher(f),
        ),
        (
            FpReason::EvidenceNotFound,
            f.captures
                .contains_key(crate::onboard::evidence_check::EVIDENCE_UNVERIFIED_CAPTURE),
        ),
        (
            FpReason::SeverityFromRuleMinimum,
            severity_from_rule_minimum(f),
        ),
        (
            FpReason::ModelOnly,
            !deterministic_peer && !is_deterministic(f) && low_confidence(f),
        ),
    ];
    checks
        .into_iter()
        .filter_map(|(reason, fires)| fires.then_some(reason))
        .collect()
}

/// Higher is more severe (`critical` 4 down to anything else 0).
fn rank(severity: &str) -> u8 {
    crate::ai_audit::severity_rank(&severity.trim().to_ascii_lowercase())
}

/// (a) The group reviewer marked the row an outlier for being a different object or an intended
/// pattern, or disagreed with it downward: a lower severity than stage 1 or the row shows, or
/// "not real" (or "unclear" against stage 1's "yes").
fn group_reviewer_doubts(f: &Finding) -> bool {
    use camerata_api_types::review::{DisagreementField, OutlierReason};
    let outlier = matches!(
        f.review.outlier_reason,
        Some(OutlierReason::DifferentObject | OutlierReason::CouldBeIntentional)
    );
    let downward = f.review.stage2_disagreement.iter().any(|d| match d.field {
        DisagreementField::Severity => {
            let mine = rank(&d.mine);
            mine < rank(&d.stage1) || mine < rank(&f.severity)
        }
        DisagreementField::Real => {
            let mine = d.mine.trim().to_ascii_lowercase();
            mine == "no" || (mine == "unclear" && d.stage1.trim().eq_ignore_ascii_case("yes"))
        }
    });
    outlier || downward
}

/// Whether calibration's confidence in `f` is anything but high. A row calibration never saw (the
/// deterministic floor, a tool preview) has no confidence and so no doubt.
fn low_confidence(f: &Finding) -> bool {
    f.confidence
        .as_deref()
        .is_some_and(|c| !c.trim().eq_ignore_ascii_case("high"))
}

/// (b) Calibration left the row waiting for a reviewer, or its confidence is low. Calibration also
/// sets `needs_review` on every test-scoped row; that alone is test scope (reason (c)), not doubt,
/// so a secret committed in a test is not flagged by the back door.
fn calibration_doubts(f: &Finding) -> bool {
    low_confidence(f) || (f.needs_review && !f.in_test)
}

/// Whether `f` is about a committed secret: a secret rule, or a row classed as secret exposure.
fn is_secret(f: &Finding) -> bool {
    crate::ai_audit::is_secret_rule(&f.rule_id)
        || crate::onboard::credential_cross_ref::is_secret_exposure(f)
}

/// (c) The row is in test or fixture code, and it is not about a secret: a real secret committed
/// in a test still counts.
fn in_test_code_only(f: &Finding) -> bool {
    (f.in_test || crate::onboard::is_test_or_fixture_path(&f.path)) && !is_secret(f)
}

/// (d) A style or convention rule (a corpus rule that is not a security rule) rated the row
/// medium or higher.
fn convention_rule_at_medium_or_higher(f: &Finding) -> bool {
    crate::ai_audit::is_convention_rule_row(f) && rank(&f.severity) >= rank("medium")
}

/// (f) The severity the row shows comes only from its rule's minimum: a rule minimum raised it
/// (the recorded note says so), stage 1 rated it lower, and the group reviewer, when it rated it
/// at all, rated it lower too. A severity the reviewer set is the reviewer's own.
fn severity_from_rule_minimum(f: &Finding) -> bool {
    use camerata_api_types::review::DisagreementField;
    if crate::report_export::overlay::reviewer_edited_field(f, EditField::Severity) {
        return false;
    }
    let raised_by_minimum = crate::severity_bounds::recorded_adjustments(f)
        .iter()
        .any(|n| camerata_rules::severity_bounds::is_min_severity_note(n));
    let shown = rank(&f.severity);
    let stage1_below = crate::ai_audit::stage1_of(f)
        .and_then(|s| s.severity)
        .is_some_and(|s1| rank(&s1) < shown);
    let stage2_below = f
        .review
        .stage2_disagreement
        .iter()
        .filter(|d| d.field == DisagreementField::Severity)
        .all(|d| rank(&d.mine) < shown);
    raised_by_minimum && stage1_below && stage2_below
}

#[cfg(test)]
mod tests;
