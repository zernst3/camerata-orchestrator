//! Batch C follow-up: coverage claims that were still false after the first pass. A scan whose
//! deterministic tier never ran must not say the dependencies are clean, a scan whose model
//! never ran must not describe a model review, and no client text carries a local path.

use std::collections::HashMap;
use std::path::Path;

use super::coverage_gaps::{coverage_gaps, repos_without_ai_review};
use super::tests::{empty_opts, finding, report_with};
use super::*;

fn ai_only_scan() -> crate::onboard::ScanReport {
    // Deterministic tier off: osv-scanner never ran, so there is no version and no note.
    let mut report = report_with(
        vec![finding("SEC-NO-HARDCODED-SECRETS-1", "src/a.rs", 1, "high")],
        vec!["SEC-NO-HARDCODED-SECRETS-1"],
    );
    report.provenance.osv_scanner_version = None;
    report
}

fn deterministic_only_scan() -> crate::onboard::ScanReport {
    // No model call was made: the provenance names no model.
    let mut report = report_with(
        vec![finding("SEC-NO-HARDCODED-SECRETS-1", "src/a.rs", 1, "high")],
        vec!["SEC-NO-HARDCODED-SECRETS-1"],
    );
    report.provenance.audit_model = None;
    report
}

// ── (1) an AI-only scan never says the dependencies are clean ────────────────────────────────

#[test]
fn a_scan_where_osv_scanner_never_ran_is_not_dependency_clean() {
    let json = build_report_json(&ai_only_scan().seal(), &HashMap::new(), None, &empty_opts());
    assert!(!json.dependency_snapshot.clean);
    assert!(!json.whats_healthy.dependency_clean);
}

#[test]
fn a_scan_where_osv_scanner_ran_and_found_nothing_is_still_clean() {
    let report = report_with(
        vec![finding("SEC-NO-HARDCODED-SECRETS-1", "src/a.rs", 1, "high")],
        vec!["SEC-NO-HARDCODED-SECRETS-1"],
    );
    assert!(report.provenance.osv_scanner_version.is_some());
    let json = build_report_json(&report.seal(), &HashMap::new(), None, &empty_opts());
    assert!(json.dependency_snapshot.clean);
}

#[test]
fn coverage_says_the_dependency_audit_did_not_run_when_there_is_no_osv_scanner_version() {
    let gaps = coverage_gaps(&ai_only_scan());
    assert!(
        gaps.dependency_gaps
            .iter()
            .any(|l| l.contains("did not run") && l.contains("dependenc")),
        "{gaps:?}"
    );
    let gaps = coverage_gaps(&report_with(Vec::new(), Vec::new()));
    assert!(gaps.dependency_gaps.is_empty(), "{gaps:?}");
}

#[tokio::test]
async fn the_pdf_of_an_ai_only_scan_never_prints_the_clean_dependency_sentence() {
    if super::tests::which_typst().is_none() {
        eprintln!("skipping: typst not on PATH");
        return;
    }
    let json = build_report_json(&ai_only_scan().seal(), &HashMap::new(), None, &empty_opts());
    let pdf = compile_pdf(&json).await.expect("the PDF compiles");
    let text = pdf_extract::extract_text_from_mem(&pdf).unwrap_or_default();
    let joined: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    assert!(
        !joined.contains("Noknownvulnerabledependenciesdetected"),
        "{text}"
    );
}

// ── (2) a scan whose model never ran does not describe a model review ───────────────────────

#[test]
fn a_scan_with_no_model_recorded_counts_every_repo_as_not_reviewed_by_the_model() {
    let report = deterministic_only_scan();
    assert_eq!(repos_without_ai_review(&report), ["owner/repo"]);
    let json = build_report_json(&report.seal(), &HashMap::new(), None, &empty_opts());
    let note = &json.methodology.ai_tier_note;
    assert!(
        note.contains("did not run for 1 repo; see Coverage."),
        "{note}"
    );
    assert!(!note.contains("calibrated language-model review"), "{note}");
}

#[test]
fn a_scan_with_a_model_recorded_keeps_the_review_description() {
    let report = report_with(Vec::new(), Vec::new());
    assert!(report.provenance.audit_model.is_some());
    assert!(repos_without_ai_review(&report).is_empty());
}

// ── (3) the note never says the findings are all deterministic ──────────────────────────────

#[test]
fn the_not_run_note_speaks_only_for_this_run() {
    let json = build_report_json(
        &deterministic_only_scan().seal(),
        &HashMap::new(),
        None,
        &empty_opts(),
    );
    let note = &json.methodology.ai_tier_note;
    assert!(
        !note.contains("Every finding in this export"),
        "an incremental run may carry earlier model findings: {note}"
    );
    assert!(
        note.contains("Every finding produced in this run came from the deterministic tier."),
        "{note}"
    );
}

// ── (4) the shared pass name stays public ───────────────────────────────────────────────────

#[test]
fn the_all_passes_name_is_a_public_constant() {
    assert_eq!(
        crate::report_export::coverage_gaps::AI_REVIEW_ALL_PASSES,
        "AI review (all passes)"
    );
}

// ── (5) stderr lines and local paths ────────────────────────────────────────────────────────

use crate::dep_audit::scrub_local_paths;
use crate::dep_audit::unread_files_sentence;

#[test]
fn only_real_osv_error_prefixes_are_quoted() {
    let stderr = "Scanned /w/error-handling/package-lock.json file and found 3 packages\n\
                  Error during extraction: could not parse yarn.lock\n\
                  Failed to parse go.mod\n\
                  the word error appears mid-line here\n";
    let s = unread_files_sentence(stderr, Path::new("/w")).unwrap();
    assert!(s.contains("Error during extraction"), "{s}");
    assert!(s.contains("Failed to parse go.mod"), "{s}");
    assert!(!s.contains("mid-line"), "{s}");
    assert!(!s.contains("error-handling"), "{s}");
}

#[test]
fn no_absolute_path_survives_in_client_text() {
    let text = "Error during extraction: bad /Users/auditor/work/repo/sub/yarn.lock (tooling \
                /Users/auditor/.local/share/camerata/osv/osv-scanner) and C:\\Users\\z\\x\\go.mod";
    let out = scrub_local_paths(text, Path::new("/Users/auditor/work/repo"));
    assert!(!out.contains("/Users"), "{out}");
    assert!(!out.contains("C:\\"), "{out}");
    assert!(out.contains("./sub/yarn.lock"), "{out}");
    assert!(out.contains("osv-scanner"), "{out}");
    assert!(out.contains("go.mod"), "{out}");
}

#[test]
fn the_canonical_form_of_the_clone_path_is_scrubbed_too() {
    let dir = tempfile::tempdir().unwrap();
    let canonical = std::fs::canonicalize(dir.path()).unwrap();
    let text = format!(
        "Error during extraction: bad {}/a/yarn.lock",
        canonical.display()
    );
    let out = scrub_local_paths(&text, dir.path());
    assert!(out.contains("./a/yarn.lock"), "{out}");
    assert!(!out.contains(&*canonical.to_string_lossy()), "{out}");
}
