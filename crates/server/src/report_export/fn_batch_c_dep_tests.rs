//! False-negative audit, Batch C, M9 to M12: the dependency section never reads clean when it
//! was not fully checked, never buries an advisory by merging it with another, and never rates
//! an advisory it cannot score as medium without saying so.

use std::collections::HashMap;
use std::path::Path;

use super::tests::{empty_opts, finding, report_with};
use super::*;
use crate::dep_audit::parse_osv_json;
use crate::onboard::CoverageNote;

/// One package with two advisories at the same version in one lockfile.
fn two_advisories_one_package() -> &'static str {
    r#"{"results":[{"source":{"path":"/w/Cargo.lock","type":"lockfile"},
      "packages":[{"package":{"name":"time","version":"0.1.45","ecosystem":"crates.io"},
      "vulnerabilities":[
        {"id":"RUSTSEC-2020-0071","summary":"first","database_specific":{"severity":"HIGH"}},
        {"id":"RUSTSEC-2021-0001","summary":"second","database_specific":{"severity":"HIGH"}}
      ]}]}]}"#
}

fn dep_rows() -> Vec<crate::onboard::Finding> {
    parse_osv_json("acme/api", Path::new("/w"), two_advisories_one_package()).unwrap()
}

fn false_positive() -> DispositionWire {
    DispositionWire {
        state: "FalsePositive".to_string(),
        reason: "not reachable".to_string(),
        bucket: String::new(),
        confirmed_by_client: false,
    }
}

// ── M9: two advisories on one package are two findings ───────────────────────────────────────

#[test]
fn each_dependency_row_records_its_advisory_id() {
    let rows = dep_rows();
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows[0].captures.get("advisory").map(String::as_str),
        Some("RUSTSEC-2020-0071")
    );
    assert_eq!(
        rows[1].captures.get("advisory").map(String::as_str),
        Some("RUSTSEC-2021-0001")
    );
}

#[test]
fn two_advisories_on_one_package_version_have_different_keys() {
    let rows = dep_rows();
    assert_eq!(rows[0].snippet, rows[1].snippet);
    assert_ne!(finding_key(&rows[0]), finding_key(&rows[1]));
}

#[test]
fn the_key_of_every_other_row_is_unchanged() {
    let f = finding("RULE-X", "src/a.rs", 7, "high");
    assert_eq!(
        finding_key(&f),
        "owner/repo\u{0}RULE-X\u{0}src/a.rs\u{0}7\u{0}snippet-7"
    );
    // A non-dependency row with an unrelated capture of the same name keeps its key.
    let mut g = f.clone();
    g.captures.insert("advisory".to_string(), "X".to_string());
    assert_eq!(finding_key(&g), finding_key(&f));
}

#[test]
fn marking_one_advisory_a_false_positive_leaves_the_other_in_the_report() {
    let rows = dep_rows();
    let mut dispositions = HashMap::new();
    dispositions.insert(finding_key(&rows[0]), false_positive());
    let mut report = report_with(Vec::new(), Vec::new());
    report.findings = rows;
    let json = build_report_json(&report.seal(), &dispositions, None, &empty_opts());
    assert_eq!(
        json.dependency_snapshot.rows.len(),
        1,
        "{:?}",
        json.dependency_snapshot
    );
    assert!(json.dependency_snapshot.rows[0]
        .advisory
        .contains("RUSTSEC-2021-0001"));
    assert!(!json.dependency_snapshot.clean);
}

// ── M11: clean means checked ─────────────────────────────────────────────────────────────────

fn osv_note(message: &str) -> CoverageNote {
    CoverageNote {
        tool_failed: false,
        repo: None,
        tool: "osv-scanner".to_string(),
        message: message.to_string(),
    }
}

fn clean_report_with_note(note: Option<CoverageNote>) -> crate::onboard::ScanReport {
    let mut report = report_with(
        vec![finding("SEC-NO-HARDCODED-SECRETS-1", "src/a.rs", 1, "high")],
        vec!["SEC-NO-HARDCODED-SECRETS-1"],
    );
    report.coverage_notes.extend(note);
    report
}

#[test]
fn no_rows_and_no_note_is_clean() {
    let json = build_report_json(
        &clean_report_with_note(None).seal(),
        &HashMap::new(),
        None,
        &empty_opts(),
    );
    assert!(json.dependency_snapshot.clean);
    assert!(json.whats_healthy.dependency_clean);
}

#[test]
fn a_dependency_audit_that_did_not_run_is_not_clean() {
    let note = osv_note("dependency audit (osv-scanner) did not run: no network");
    let json = build_report_json(
        &clean_report_with_note(Some(note)).seal(),
        &HashMap::new(),
        None,
        &empty_opts(),
    );
    assert!(!json.dependency_snapshot.clean);
    assert!(!json.whats_healthy.dependency_clean);
}

#[test]
fn a_dependency_audit_that_found_no_lockfiles_still_counts_as_checked() {
    let note = osv_note("dependency audit (osv-scanner) found no lockfiles in acme/api");
    let json = build_report_json(
        &clean_report_with_note(Some(note)).seal(),
        &HashMap::new(),
        None,
        &empty_opts(),
    );
    assert!(json.dependency_snapshot.clean);
}

#[test]
fn a_note_about_unaudited_manifests_is_not_clean() {
    let note = osv_note(
        "Dependency manifests not audited (no lockfile): package.json. osv-scanner reads \
         lockfiles, so the packages these declare were not checked for known vulnerabilities.",
    );
    let json = build_report_json(
        &clean_report_with_note(Some(note)).seal(),
        &HashMap::new(),
        None,
        &empty_opts(),
    );
    assert!(!json.dependency_snapshot.clean);
}

#[test]
fn a_dependency_audit_that_did_not_run_is_listed_under_coverage() {
    let note = osv_note("dependency audit (osv-scanner) did not run: no network");
    let report = clean_report_with_note(Some(note));
    let gaps = super::coverage_gaps::coverage_gaps(&report);
    assert!(
        gaps.dependency_gaps
            .iter()
            .any(|l| l.contains("ependency audit") && l.contains("no network")),
        "{gaps:?}"
    );
    // A repo with no lockfiles is checked, not a gap.
    let report = clean_report_with_note(Some(osv_note(
        "dependency audit (osv-scanner) found no lockfiles in acme/api",
    )));
    assert!(super::coverage_gaps::coverage_gaps(&report)
        .dependency_gaps
        .is_empty());
}

#[tokio::test]
async fn the_pdf_does_not_say_no_vulnerable_dependencies_when_the_audit_did_not_run() {
    if super::tests::which_typst().is_none() {
        eprintln!("skipping the PDF dependency test: typst not on PATH");
        return;
    }
    let note = osv_note("dependency audit (osv-scanner) did not run: no network");
    let json = build_report_json(
        &clean_report_with_note(Some(note)).seal(),
        &HashMap::new(),
        None,
        &empty_opts(),
    );
    let pdf = compile_pdf(&json).await.expect("the PDF compiles");
    let text = pdf_extract::extract_text_from_mem(&pdf).unwrap_or_default();
    let joined: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    assert!(
        !joined.contains("Noknownvulnerabledependenciesdetected"),
        "{text}"
    );
    assert!(joined.contains("Dependencieswerenotfullychecked"), "{text}");
    assert!(joined.contains("dependencyaudit"), "{text}");
}
