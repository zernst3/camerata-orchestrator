//! M5: a row whose quoted evidence was not found is held for review until the reviewer decides it.

use std::collections::HashMap;

use super::tests::{empty_opts, finding, report_with};
use super::*;
use crate::onboard::evidence_check::EVIDENCE_UNVERIFIED_CAPTURE;

fn unverified() -> Finding {
    let mut f = finding("SEC-NO-HARDCODED-SECRETS-1", "src/a.rs", 10, "high");
    f.captures.insert(
        EVIDENCE_UNVERIFIED_CAPTURE.to_string(),
        "the quoted snippet was not found within 3 lines of line 10".to_string(),
    );
    f
}

#[test]
fn an_unverified_row_is_held_until_the_reviewer_decides_it() {
    let f = unverified();
    assert!(is_held_for_review(&f, Disposition::Unresolved));
    assert_eq!(
        effective_bucket(&f, Disposition::Unresolved, "high", None, 0, None),
        "held"
    );
    // A reviewer's decision supersedes the hold, the same as for a calibration hedge.
    for decided in [
        Disposition::Confirmed,
        Disposition::Ignored,
        Disposition::TechDebtNow,
    ] {
        assert!(!is_held_for_review(&f, decided));
    }
}

#[test]
fn a_verified_row_is_never_held_by_this_rule() {
    let f = finding("SEC-NO-HARDCODED-SECRETS-1", "src/a.rs", 10, "high");
    assert!(!is_held_for_review(&f, Disposition::Unresolved));
}

#[test]
fn the_exported_row_reads_as_held_and_needs_review() {
    let report = report_with(vec![unverified()], vec!["SEC-NO-HARDCODED-SECRETS-1"]).seal();
    let json = build_report_json(&report, &HashMap::new(), None, &empty_opts());
    assert_eq!(json.executive_summary.held_for_review, 1);
}
