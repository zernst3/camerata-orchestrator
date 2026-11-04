//! WO9, design 3B item 3: the review level of every row, computed in `apply_overlay` and nowhere
//! else.

use camerata_api_types::decisions::DecisionState;
use camerata_api_types::edits::EditField;
use camerata_api_types::review::{EscalationState, ReviewLevel};

use super::support::*;
use crate::report_export::{apply_overlay, ReviewedReport};

fn level_of(reviewed: &ReviewedReport, path: &str) -> Option<ReviewLevel> {
    let f = reviewed
        .report()
        .findings
        .iter()
        .find(|f| f.path == path)
        .unwrap_or_else(|| panic!("no row in {path}"));
    reviewed.review_level(f).cloned()
}

#[test]
fn a_row_nobody_decided_is_engine_verified() {
    let sealed = sealed_of(2);
    let reviewed = apply_overlay(&sealed, &[], &[], &[]);
    assert_eq!(
        level_of(&reviewed, "src/file_0.rs"),
        Some(ReviewLevel::EngineVerified)
    );
}

#[test]
fn a_site_decision_an_edit_and_a_decided_escalation_are_personally_reviewed() {
    let mut report = sealed_of(4).into_inner();
    report.findings[2].review.escalation_state = EscalationState::Escalated;
    let sealed = report.seal();
    let key = |i: usize| sealed.findings[i].site_key.clone();

    // A site-scope decision.
    let site = decision("dec-1", &key(0), DecisionState::Confirmed, "");
    // An applied edit.
    let edit = edit_of(
        1,
        &sealed.findings[1],
        EditField::Headline,
        "Query from input",
    );
    // An escalated site whose decision came with a group decision: the reviewer looked at it.
    let group = node_decision(
        "dec-2",
        "rule:SEC-NO-RAW-SQL-CONCAT-1",
        &[key(2), key(3)],
        DecisionState::Confirmed,
        "same fix",
    );
    let mut records = vec![site];
    records.extend(group);
    let reviewed = apply_overlay(&sealed, &records, &[edit], &[]);

    assert_eq!(
        level_of(&reviewed, "src/file_0.rs"),
        Some(ReviewLevel::PersonallyReviewed),
        "a site-scope decision"
    );
    assert_eq!(
        level_of(&reviewed, "src/file_1.rs"),
        Some(ReviewLevel::PersonallyReviewed),
        "an applied edit"
    );
    assert_eq!(
        level_of(&reviewed, "src/file_2.rs"),
        Some(ReviewLevel::PersonallyReviewed),
        "a decided escalation"
    );
    // The same group decision on a row nobody escalated is a pattern review.
    assert_eq!(
        level_of(&reviewed, "src/file_3.rs"),
        Some(ReviewLevel::ReviewedAsPattern(
            "rule:SEC-NO-RAW-SQL-CONCAT-1".to_string()
        ))
    );
}

#[test]
fn a_stale_edit_a_revoked_decision_and_a_bare_restore_do_not_count() {
    let sealed = sealed_of(3);
    let key = |i: usize| sealed.findings[i].site_key.clone();
    let mut stale = edit_of(1, &sealed.findings[0], EditField::Headline, "x");
    stale.fingerprint_at_edit = "an-older-fingerprint".to_string();
    let mut revoked = decision("dec-1", &key(1), DecisionState::Confirmed, "");
    revoked.revoked_at = Some(NOW.to_string());
    let reviewed = apply_overlay(&sealed, &[revoked], &[stale], &[]);
    assert_eq!(
        level_of(&reviewed, "src/file_0.rs"),
        Some(ReviewLevel::EngineVerified),
        "a stale edit was not applied"
    );
    assert_eq!(
        level_of(&reviewed, "src/file_1.rs"),
        Some(ReviewLevel::EngineVerified),
        "a revoked decision is no decision"
    );
}

#[test]
fn a_held_row_with_no_ruling_has_no_level() {
    let mut report = sealed_of(2).into_inner();
    report.findings[0].captures.insert(
        crate::onboard::evidence_check::EVIDENCE_UNVERIFIED_CAPTURE.to_string(),
        "the quoted code is not at the line".to_string(),
    );
    let sealed = report.seal();
    let reviewed = apply_overlay(&sealed, &[], &[], &[]);
    assert_eq!(
        level_of(&reviewed, "src/file_0.rs"),
        None,
        "unverified evidence is not engine_verified"
    );
    assert_eq!(
        level_of(&reviewed, "src/file_1.rs"),
        Some(ReviewLevel::EngineVerified)
    );
    // Once the reviewer decides it, it is personally reviewed.
    let key = sealed.findings[0].site_key.clone();
    let decided = apply_overlay(
        &sealed,
        &[decision("dec-1", &key, DecisionState::Confirmed, "")],
        &[],
        &[],
    );
    assert_eq!(
        level_of(&decided, "src/file_0.rs"),
        Some(ReviewLevel::PersonallyReviewed)
    );
}

#[test]
fn an_older_clients_dispositions_make_their_rows_personally_reviewed() {
    let sealed = sealed_of(2);
    let key = sealed.findings[0].site_key.clone();
    let mut dispositions = std::collections::HashMap::new();
    dispositions.insert(
        key,
        crate::report_export::DispositionWire {
            state: "Confirmed".to_string(),
            reason: String::new(),
            bucket: String::new(),
            confirmed_by_client: false,
        },
    );
    let reviewed = ReviewedReport::unreviewed(&sealed, &dispositions);
    assert_eq!(
        level_of(&reviewed, "src/file_0.rs"),
        Some(ReviewLevel::PersonallyReviewed)
    );
    assert_eq!(
        level_of(&reviewed, "src/file_1.rs"),
        Some(ReviewLevel::EngineVerified)
    );
    let later = apply_overlay(&sealed, &[], &[], &[]).with_legacy_dispositions(dispositions);
    assert_eq!(
        level_of(&later, "src/file_0.rs"),
        Some(ReviewLevel::PersonallyReviewed)
    );
}

/// Every `.rs` file under `dir`, recursively.
fn rust_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n == "target") {
                continue;
            }
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn review_level_is_computed_only_in_apply_overlay() {
    let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut files = Vec::new();
    rust_files(&crates, &mut files);
    // Paths are judged below the crates folder: the checkout itself may be named anything.
    let relative = |p: &std::path::Path| {
        let root = crates.canonicalize().unwrap_or_default();
        let p = p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
        p.strip_prefix(&root)
            .map(|r| r.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    let is_test_file = |p: &std::path::Path| {
        let s = relative(p);
        s.contains("/tests/")
            || s.contains("/wo9/")
            || s.ends_with("_tests.rs")
            || s.ends_with("/tests.rs")
    };
    let allowed = |p: &std::path::Path| {
        let s = relative(p);
        s.ends_with("api-types/src/review.rs") || s.ends_with("report_export/overlay.rs")
    };
    let mut seen_in_overlay = false;
    let mut offenders = Vec::new();
    for path in &files {
        if is_test_file(path) {
            continue;
        }
        let text = std::fs::read_to_string(path).unwrap_or_default();
        // Production code only: the file's `#[cfg(test)]` tail is test code.
        let production = text.split("#[cfg(test)]").next().unwrap_or_default();
        let builds = ["PersonallyReviewed", "ReviewedAsPattern", "EngineVerified"]
            .iter()
            .any(|v| production.contains(&format!("ReviewLevel::{v}")));
        if relative(path).ends_with("report_export/overlay.rs") {
            seen_in_overlay = builds;
        }
        if builds && !allowed(path) {
            offenders.push(path.display().to_string());
        }
    }
    assert!(
        seen_in_overlay,
        "apply_overlay must be where the review level is computed"
    );
    assert!(
        offenders.is_empty(),
        "a review level is built outside apply_overlay: {offenders:?}"
    );
}
