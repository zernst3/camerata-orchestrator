//! False-negative audit, Batch C: the exporters and the dependency audit must never drop,
//! soften or hide a real finding. Each section below is one audit item (H4, H6, M2/M8, H11,
//! M9 to M12), tests first.

use std::collections::HashMap;

use super::coverage_gaps::{coverage_gaps, AI_REVIEW_ALL_PASSES};
use super::tests::{empty_opts, finding, report_with};
use super::*;
use crate::ai_audit::FailedPass;

// ── H4: the methodology never claims a model review that did not run ─────────────────────────

fn two_repo_report(failed_repo: Option<&str>) -> crate::onboard::ScanReport {
    let mut report = report_with(
        vec![finding(
            "SEC-NO-HARDCODED-SECRETS-1",
            "src/a.rs",
            10,
            "high",
        )],
        vec!["SEC-NO-HARDCODED-SECRETS-1"],
    );
    report.repos = vec!["owner/a".to_string(), "owner/b".to_string()];
    if let Some(repo) = failed_repo {
        report.failed_passes.push(FailedPass {
            repo: repo.to_string(),
            pass: AI_REVIEW_ALL_PASSES.to_string(),
            reason: "the model service returned an error".to_string(),
            files_not_reviewed: 7,
            class: crate::ai_audit::FailureClass::Content,
            files: Vec::new(),
            attempts: Vec::new(),
            scope_note: false,
        });
    }
    report
}

#[test]
fn the_ai_tier_note_says_when_the_tier_did_not_run_for_some_repos() {
    let report = two_repo_report(Some("owner/b"));
    let json = build_report_json(&report.seal(), &HashMap::new(), None, &empty_opts());
    let note = &json.methodology.ai_tier_note;
    assert!(
        note.contains("did not run for 1 repo; see Coverage."),
        "{note}"
    );
}

#[test]
fn the_ai_tier_note_drops_the_calibrated_claim_when_the_tier_ran_nowhere() {
    let mut report = two_repo_report(None);
    report.ai_error = Some("401 unauthorized".to_string());
    let json = build_report_json(&report.seal(), &HashMap::new(), None, &empty_opts());
    let note = &json.methodology.ai_tier_note;
    assert!(
        note.contains("did not run for 2 repos; see Coverage."),
        "{note}"
    );
    assert!(
        !note.contains("calibrated language-model review"),
        "a tier that ran nowhere must not be described as applied: {note}"
    );
}

#[test]
fn a_blocked_ai_review_is_disclosed_like_a_failed_one() {
    let mut report = two_repo_report(None);
    report.ai_blocked_reason = Some("no Anthropic key".to_string());
    let json = build_report_json(&report.seal(), &HashMap::new(), None, &empty_opts());
    assert!(json
        .methodology
        .ai_tier_note
        .contains("did not run for 2 repos; see Coverage."));
}

#[test]
fn a_clean_ai_run_keeps_the_original_note() {
    let json = build_report_json(
        &two_repo_report(None).seal(),
        &HashMap::new(),
        None,
        &empty_opts(),
    );
    let note = &json.methodology.ai_tier_note;
    assert!(note.contains("calibrated language-model review"), "{note}");
    assert!(!note.contains("did not run"), "{note}");
}

#[test]
fn coverage_has_a_line_for_every_repo_the_ai_tier_did_not_run_on() {
    // A whole-repo failure with no files counted still gets a line.
    let mut report = two_repo_report(None);
    report.failed_passes.push(FailedPass {
        repo: "owner/b".to_string(),
        pass: AI_REVIEW_ALL_PASSES.to_string(),
        reason: "boom".to_string(),
        files_not_reviewed: 0,
        class: crate::ai_audit::FailureClass::Content,
        files: Vec::new(),
        attempts: Vec::new(),
        scope_note: false,
    });
    let gaps = coverage_gaps(&report);
    assert!(
        gaps.failed_ai_passes.iter().any(|l| l.contains("owner/b")),
        "{:?}",
        gaps.failed_ai_passes
    );

    // An error or block that left no per-repo record is listed for each repo.
    let mut report = two_repo_report(None);
    report.ai_error = Some("401".to_string());
    let gaps = coverage_gaps(&report);
    assert_eq!(
        gaps.failed_ai_passes.len(),
        2,
        "{:?}",
        gaps.failed_ai_passes
    );
    assert!(gaps
        .failed_ai_passes
        .iter()
        .all(|l| l.contains("did not run")));
}

#[test]
fn a_repo_with_a_per_file_line_is_not_listed_twice() {
    let report = two_repo_report(Some("owner/b"));
    let gaps = coverage_gaps(&report);
    let for_b = gaps
        .failed_ai_passes
        .iter()
        .filter(|l| l.contains("owner/b"))
        .count();
    assert_eq!(for_b, 1, "{:?}", gaps.failed_ai_passes);
}
