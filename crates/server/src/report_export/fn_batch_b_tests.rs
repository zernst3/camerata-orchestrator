//! Batch B: the repos the report says the advisory tier did not run for are exactly the repos it
//! did not run for, and a rule that looked at only part of the files says so where the client
//! reads.

use std::collections::HashMap;

use super::coverage_gaps::{coverage_gaps, repos_without_ai_review, AI_REVIEW_ALL_PASSES};
use super::tests::{empty_opts, finding, report_with};
use super::*;
use crate::scan_ledger::RuleTier;

fn two_repo_scan() -> crate::onboard::ScanReport {
    let mut report = report_with(
        vec![finding("SEC-NO-HARDCODED-SECRETS-1", "src/a.rs", 1, "high")],
        vec!["SEC-NO-HARDCODED-SECRETS-1"],
    );
    report.repos = vec!["owner/one".to_string(), "owner/two".to_string()];
    report.provenance.audit_model = Some("model-x".to_string());
    report
}

fn whole_repo_failure(repo: &str) -> crate::ai_audit::FailedPass {
    crate::ai_audit::FailedPass {
        repo: repo.to_string(),
        pass: AI_REVIEW_ALL_PASSES.to_string(),
        reason: "scripted".to_string(),
        files_not_reviewed: 4,
        class: crate::ai_audit::FailureClass::Content,
        files: Vec::new(),
        attempts: Vec::new(),
        scope_note: false,
    }
}

#[test]
fn an_ai_error_on_one_of_two_repos_lists_only_that_repo() {
    let mut report = two_repo_scan();
    report.ai_error = Some("owner/two: AI audit skipped (scripted)".to_string());
    report.failed_passes.push(whole_repo_failure("owner/two"));
    assert_eq!(repos_without_ai_review(&report), ["owner/two"]);

    let json = build_report_json(&report.seal(), &HashMap::new(), None, &empty_opts());
    let note = &json.methodology.ai_tier_note;
    assert!(
        note.contains("did not run for 1 repo; see Coverage."),
        "{note}"
    );
}

#[test]
fn a_block_or_a_missing_model_still_lists_every_repo() {
    let mut blocked = two_repo_scan();
    blocked.ai_blocked_reason = Some("no key".to_string());
    assert_eq!(
        repos_without_ai_review(&blocked),
        ["owner/one", "owner/two"]
    );

    let mut no_model = two_repo_scan();
    no_model.provenance.audit_model = None;
    assert_eq!(
        repos_without_ai_review(&no_model),
        ["owner/one", "owner/two"]
    );

    // An error with no row naming the repo (a report saved before the rows existed): which repo
    // failed is unknown, so none is assumed reviewed.
    let mut legacy = two_repo_scan();
    legacy.ai_error = Some("owner/two: AI audit skipped (scripted)".to_string());
    assert_eq!(repos_without_ai_review(&legacy), ["owner/one", "owner/two"]);

    // An error with no model recorded is the same: nothing reviewed anything.
    let mut error_no_model = two_repo_scan();
    error_no_model.provenance.audit_model = None;
    error_no_model.ai_error = Some("scripted".to_string());
    assert_eq!(repos_without_ai_review(&error_no_model).len(), 2);
}

// ── a rule that looked at only part of the files says so where the client reads ─────────────

fn scoped_scan() -> crate::onboard::ScanReport {
    let mut report = two_repo_scan();
    // RUST-A-1 is language-scoped: all 10 files of repo one are Rust, 6 of the 40 in repo two.
    report
        .ledger
        .record_rule("", "RUST-A-1", RuleTier::Semantic, true, None, 50, 0);
    report
        .ledger
        .note_scope_subset("RUST-A-1", "", 10, 10, "language", false);
    report
        .ledger
        .note_scope_subset("RUST-A-1", "", 6, 40, "language", false);
    // RUST-B-1 carries another coverage note already; the scope fact must not be dropped.
    report.ledger.note_config_gated_rule("", "RUST-B-1");
    report
        .ledger
        .record_rule("", "RUST-B-1", RuleTier::Semantic, true, None, 50, 0);
    report
        .ledger
        .note_scope_subset("RUST-B-1", "", 6, 40, "declared", false);
    // ARCH-C-1 looked at every file of both repos: nothing to say.
    report
        .ledger
        .record_rule("", "ARCH-C-1", RuleTier::Semantic, true, None, 50, 0);
    report
        .ledger
        .note_scope_subset("ARCH-C-1", "", 40, 40, "declared", false);
    report
}

#[test]
fn the_coverage_gaps_aggregate_a_rules_scope_across_repos() {
    let report = scoped_scan();
    assert!(
        report
            .ledger
            .rule("RUST-B-1")
            .unwrap()
            .coverage_note
            .is_some(),
        "the rule already carries another note"
    );
    let gaps = coverage_gaps(&report);
    let ids: Vec<&str> = gaps
        .scope_limited_rules
        .iter()
        .map(|r| r.rule_id.as_str())
        .collect();
    assert_eq!(ids, ["RUST-A-1", "RUST-B-1"], "{gaps:?}");
    let a = &gaps.scope_limited_rules[0];
    assert_eq!((a.files_checked, a.files_total), (16, 50), "{a:?}");
    assert_eq!(a.scope, "language");
    let note = gaps.scope_limited_note.as_deref().unwrap();
    assert!(
        note.starts_with("2 rules were checked only against"),
        "{note}"
    );
    assert!(!note.contains('\u{2014}'), "no em dashes: {note}");
    assert!(!gaps.is_empty());
}

#[test]
fn the_scope_sentence_promises_counts_not_file_lists() {
    let gaps = coverage_gaps(&scoped_scan());
    let note = gaps.scope_limited_note.as_deref().unwrap();
    assert!(
        note.contains(
            "The counts per rule are on the workbook's Coverage sheet and in findings.json."
        ),
        "{note}"
    );
    assert!(!note.contains("files each was checked against"), "{note}");
    assert!(note.contains("16 of 50 files"), "{note}");
}

#[test]
fn an_incremental_runs_scope_counts_say_they_are_over_the_changed_files() {
    let mut report = two_repo_scan();
    report
        .ledger
        .record_rule("", "RUST-A-1", RuleTier::Semantic, true, None, 3, 0);
    report
        .ledger
        .note_scope_subset("RUST-A-1", "", 2, 5, "language", true);
    let gaps = coverage_gaps(&report);
    assert!(gaps.scope_limited_rules[0].changed_files_only);
    let note = gaps.scope_limited_note.as_deref().unwrap();
    assert!(note.contains("2 of the 5 changed files"), "{note}");

    let bytes =
        crate::xlsx_export::build_workbook(&report.seal(), &HashMap::new(), None, &empty_opts())
            .unwrap();
    let all = crate::release_gate::xlsx_strings(&bytes)
        .expect("a workbook")
        .join("\n");
    assert!(
        all.contains("files changed since the earlier inspection"),
        "{all}"
    );
}

#[test]
fn failure_text_loses_machine_paths_when_the_report_is_sealed() {
    let mut report = two_repo_scan();
    report.ai_error = Some(
        "owner/two: AI audit skipped (could not read /Users/auditor/clones/two/src/a.rs)"
            .to_string(),
    );
    report.ai_blocked_reason = Some("blocked: /home/ci/work/key.json is missing".to_string());
    report.failed_passes.push(crate::ai_audit::FailedPass {
        repo: "owner/two".to_string(),
        pass: AI_REVIEW_ALL_PASSES.to_string(),
        reason: "cannot open C:\\Users\\auditor\\two\\src\\a.rs".to_string(),
        files_not_reviewed: 4,
        class: crate::ai_audit::FailureClass::Content,
        files: Vec::new(),
        attempts: Vec::new(),
        scope_note: false,
    });
    let sealed = report.seal();
    let sealed: &crate::onboard::ScanReport = &sealed;
    let all = format!(
        "{:?} {:?} {}",
        sealed.ai_error, sealed.ai_blocked_reason, sealed.failed_passes[0].reason
    );
    for local in ["/Users/auditor", "/home/ci", "C:\\Users", "clones/two"] {
        assert!(!all.contains(local), "{local} in {all}");
    }
    assert!(all.contains("a.rs") && all.contains("key.json"), "{all}");
}

#[test]
fn a_scan_whose_rules_all_saw_every_file_has_no_scope_line() {
    let mut report = two_repo_scan();
    report
        .ledger
        .record_rule("", "ARCH-C-1", RuleTier::Semantic, true, None, 50, 0);
    report
        .ledger
        .note_scope_subset("ARCH-C-1", "", 40, 40, "declared", false);
    let gaps = coverage_gaps(&report);
    assert!(gaps.scope_limited_rules.is_empty());
    assert!(gaps.scope_limited_note.is_none());
}

#[test]
fn the_workbook_and_findings_json_carry_the_scope_lines() {
    let report = scoped_scan().seal();
    let bytes =
        crate::xlsx_export::build_workbook(&report, &HashMap::new(), None, &empty_opts()).unwrap();
    let strings = crate::release_gate::xlsx_strings(&bytes).expect("a workbook");
    let all = strings.join("\n");
    assert!(all.contains("2 rules were checked only against"), "{all}");
    assert!(all.contains("RUST-A-1"), "{all}");

    let json = build_report_json(&report, &HashMap::new(), None, &empty_opts());
    let export = crate::xlsx_export::build_findings_export(
        &report,
        &HashMap::new(),
        None,
        &json,
        &HashMap::new(),
    );
    let value = serde_json::to_value(&export).unwrap();
    assert_eq!(
        value["coverage"]["scope_limited_rules"][0]["rule_id"],
        "RUST-A-1"
    );
    assert_eq!(
        value["coverage"]["scope_limited_rules"][0]["files_checked"],
        16
    );
}

#[tokio::test]
async fn the_pdf_prints_the_scope_sentence() {
    if super::tests::which_typst().is_none() {
        eprintln!("skipping: typst not on PATH");
        return;
    }
    let json = build_report_json(&scoped_scan().seal(), &HashMap::new(), None, &empty_opts());
    let pdf = compile_pdf(&json).await.expect("the PDF compiles");
    let text = pdf_extract::extract_text_from_mem(&pdf).unwrap_or_default();
    let joined: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    assert!(joined.contains("2ruleswerecheckedonlyagainst"), "{text}");
}
