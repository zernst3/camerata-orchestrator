//! The gate's check of every count (single counts, work order B). A real package is built through
//! `assemble_package` around a stand-in PDF, like the gate's other tests, so these run without
//! Typst. Synthetic rows only.
//!
//! `counts_problems` is built and tested here but NOT wired into the release path: the surfaces
//! still count on their own, so it refuses today's package (the last test records by how much).
//! Work order C makes the surfaces read the object and then wires the check in.

use std::collections::{BTreeMap, HashMap};

use serde_json::{json, Value};

use super::consistency::counts_problems;
use super::counts_check::{counts_problems_with, expected_narrative, CountsChecks};
use super::tests::{client_options, scan_report};
use super::*;
use crate::onboard::Finding;
use crate::report_export::{build_report_json, render_data_json};

const STEM: &str = "acme-inspection";

fn package_for(report: crate::onboard::ScanReport) -> ProductPackage {
    let sealed = report.seal();
    let options = client_options();
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&sealed, &HashMap::new()),
        None,
        &options,
    );
    let data = render_data_json(&json).expect("data.json renders");
    assemble_package(
        &crate::report_export::ReviewedReport::unreviewed(&sealed, &HashMap::new()),
        None,
        &options,
        STEM,
        (json, data, b"%PDF-1.7 stand-in for the gate tests".to_vec()),
    )
    .expect("the package assembles")
}

fn entries_of(package: ProductPackage) -> (BTreeMap<String, Vec<u8>>, AuditReportJson) {
    let report_json = package.report_json.clone();
    let released = release(package).expect("a clean package clears the gate");
    (
        read_zip_entries(&released.bytes).expect("the zip reads"),
        report_json,
    )
}

/// A package whose surfaces all agree with the counts: today's one-row package, with the one
/// sentence the surfaces word differently (the narrative) set to the wording the counts give.
fn consistent() -> (BTreeMap<String, Vec<u8>>, AuditReportJson) {
    let (entries, mut report) = entries_of(package_for(scan_report()));
    report.executive_summary.narrative = expected_narrative(&report.counts);
    (entries, report)
}

fn edit_json(entries: &mut BTreeMap<String, Vec<u8>>, name: &str, f: impl FnOnce(&mut Value)) {
    let mut value: Value = serde_json::from_slice(&entries[name]).unwrap();
    f(&mut value);
    entries.insert(name.to_string(), serde_json::to_vec(&value).unwrap());
}

fn messages(problems: &[super::report::Problem]) -> String {
    problems
        .iter()
        .map(|p| format!("{} | {} | {}", p.file, p.location, p.message))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn a_package_whose_surfaces_agree_with_the_counts_passes() {
    let (entries, report) = consistent();
    assert_eq!(report.counts.problems.total, 1, "anti-vacuity");
    let problems = counts_problems(&entries, &report);
    assert!(problems.is_empty(), "{}", messages(&problems));
}

#[test]
fn the_gate_refuses_a_count_sentence_that_differs_from_the_counts() {
    let (entries, mut report) = consistent();
    // One digit of the narrative changed: the files analyzed.
    report.executive_summary.narrative = report
        .executive_summary
        .narrative
        .replacen("10 files", "11 files", 1);
    let problems = counts_problems(&entries, &report);
    assert!(
        problems
            .iter()
            .any(|p| p.location == "executive_summary.narrative"),
        "{}",
        messages(&problems)
    );
}

#[test]
fn the_gate_refuses_a_next_steps_sentence_that_states_other_counts() {
    let (entries, mut report) = consistent();
    report.methodology.next_steps =
        "Fix rate not yet established: 5 of 9 problems need a reviewer's decision.".to_string();
    let problems = counts_problems(&entries, &report);
    assert!(
        problems
            .iter()
            .any(|p| p.location == "methodology.next_steps"),
        "{}",
        messages(&problems)
    );
}

#[test]
fn the_gate_refuses_an_override_with_a_number_that_is_no_count() {
    let (entries, mut report) = consistent();
    report.executive_summary.is_override = true;
    report.executive_summary.narrative =
        "Our review found 1 problem across 10 files and we expect 77 to follow.".to_string();
    let problems = counts_problems(&entries, &report);
    let stray: Vec<_> = problems
        .iter()
        .filter(|p| p.location == "executive_summary.narrative")
        .collect();
    assert_eq!(stray.len(), 1, "{}", messages(&problems));
    assert!(stray[0].message.contains("77"));
}

#[test]
fn an_override_that_states_only_counts_of_the_package_passes() {
    let (entries, mut report) = consistent();
    report.executive_summary.is_override = true;
    report.executive_summary.narrative = "We found 1 problem across 10 files.".to_string();
    let problems = counts_problems(&entries, &report);
    assert!(problems.is_empty(), "{}", messages(&problems));
}

#[test]
fn the_gate_refuses_counts_in_findings_json_that_the_files_do_not_give() {
    let (mut entries, report) = consistent();
    edit_json(&mut entries, "findings.json", |v| {
        v["counts"]["problems"]["total"] = json!(2);
    });
    let problems = counts_problems(&entries, &report);
    assert!(
        problems
            .iter()
            .any(|p| p.location == "counts.problems.total" && p.file == "findings.json"),
        "{}",
        messages(&problems)
    );
}

#[test]
fn the_gate_refuses_a_row_dropped_from_the_dataset_the_counts_were_taken_from() {
    let (mut entries, report) = consistent();
    entries.insert("json/all-findings.json".to_string(), b"[]".to_vec());
    let problems = counts_problems(&entries, &report);
    assert!(
        problems.iter().any(|p| p.location == "counts.rows.problem"),
        "{}",
        messages(&problems)
    );
}

#[test]
fn the_gate_refuses_a_pdf_data_object_that_is_not_findings_jsons() {
    let (entries, mut report) = consistent();
    report.counts.problems.later += 1;
    let problems = counts_problems(&entries, &report);
    assert!(
        problems
            .iter()
            .any(|p| p.file == "PDF data" && p.location == "counts.problems.later"),
        "{}",
        messages(&problems)
    );
}

#[test]
fn the_gate_refuses_a_cover_or_methodology_figure_that_is_not_a_count() {
    let (entries, mut report) = consistent();
    report.cover.stats.medium += 3;
    report.methodology.candidates_reviewed += 441;
    report.methodology.held_for_review += 1;
    let problems = counts_problems(&entries, &report);
    for location in [
        "cover.stats.medium",
        "methodology.candidates_reviewed",
        "methodology.held_for_review",
    ] {
        assert!(
            problems.iter().any(|p| p.location == location),
            "{location}: {}",
            messages(&problems)
        );
    }
}

#[test]
fn the_gate_refuses_a_ticket_file_with_a_ticket_missing() {
    let (mut entries, report) = consistent();
    let csv = String::from_utf8(entries["csv/tickets.csv"].clone()).unwrap();
    let header = csv.lines().next().unwrap().to_string();
    assert!(
        csv.lines().count() > 1,
        "anti-vacuity: the file has a ticket"
    );
    entries.insert(
        "csv/tickets.csv".to_string(),
        format!("{header}\n").into_bytes(),
    );
    let problems = counts_problems(&entries, &report);
    assert!(
        problems.iter().any(|p| p.file == "csv/tickets.csv"),
        "{}",
        messages(&problems)
    );
}

#[test]
fn the_gate_refuses_a_row_the_identities_cannot_place() {
    let (mut entries, report) = consistent();
    // A row of a kind nobody counts: source rows no longer add up.
    edit_json(&mut entries, "json/all-findings.json", |v| {
        let mut odd = v[0].clone();
        odd["kind"] = json!("mystery");
        v.as_array_mut().unwrap().push(odd);
    });
    let problems = counts_problems(&entries, &report);
    assert!(
        problems.iter().any(|p| p.location == "identities"),
        "{}",
        messages(&problems)
    );
}

// ── Checks that belong to work order D (the flag is off today) ─────────────────────────────

/// Two rows at one place, as the dataset holds them.
fn two_rows_at_one_place(carried_by: &str) -> (BTreeMap<String, Vec<u8>>, AuditReportJson) {
    let (mut entries, report) = consistent();
    edit_json(&mut entries, "json/all-findings.json", |v| {
        let mut head = v[0].clone();
        head["facts"] = Value::Null;
        head["site_id"] = json!("head-1");
        head["carried_by"] = json!("");
        let mut second = head.clone();
        second["site_id"] = json!("second-1");
        second["carried_by"] = json!(carried_by);
        *v = json!([head, second]);
    });
    (entries, report)
}

#[test]
fn work_order_d_check_7_is_off_until_d_switches_it_on() {
    let (entries, report) = two_rows_at_one_place("");
    let problems = counts_problems_with(&entries, &report, CountsChecks::default());
    assert!(
        !problems.iter().any(|p| p.location.starts_with("place ")),
        "{}",
        messages(&problems)
    );
}

#[test]
fn work_order_d_check_7_a_folded_row_must_name_its_head() {
    let on = CountsChecks {
        folded_rows_name_their_head: true,
    };
    let (entries, report) = two_rows_at_one_place("");
    let problems = counts_problems_with(&entries, &report, on);
    assert!(
        problems.iter().any(|p| p.location.starts_with("place ")),
        "{}",
        messages(&problems)
    );
    let (entries, report) = two_rows_at_one_place("head-1");
    let problems = counts_problems_with(&entries, &report, on);
    assert!(
        !problems.iter().any(|p| p.location.starts_with("place ")),
        "{}",
        messages(&problems)
    );
}

// ── Today's package ──────────────────────────────────────────────────────────────────────

/// Rows of every unit at once, so the surfaces that count on their own disagree: two rows at one
/// place (one held), a dependency advisory, a convention row and a row for a rule that could not
/// be completed.
fn mixed_report() -> crate::onboard::ScanReport {
    use camerata_api_types::audit::{
        ConventionObservedKind, FindingKind, NotAssessedKind, NotAssessedReason,
    };
    let row = |rule: &str, path: &str, line: usize, severity: &str| Finding {
        repo: "acme/portal".to_string(),
        path: path.to_string(),
        line,
        rule_id: rule.to_string(),
        severity: severity.to_string(),
        snippet: format!("let x{line} = 1;"),
        detail: format!("detail {rule}"),
        ..Finding::default()
    };
    let mut held = row("SYN-B-1", "src/a.rs", 5, "medium");
    held.needs_review = true;
    let mut convention = row("SYN-C-1", "src/c.rs", 1, "info");
    convention.kind = Some(FindingKind::ConventionObserved(ConventionObservedKind {
        convention_option_id: "x".to_string(),
        convention_label: "A convention".to_string(),
    }));
    let mut unchecked = row("SYN-D-1", "src/u.rs", 0, "info");
    unchecked.kind = Some(FindingKind::NotAssessed(NotAssessedKind {
        reason: NotAssessedReason::NoFilesMatched,
        reason_detail: "none".to_string(),
    }));
    let mut report = scan_report();
    report.findings = vec![
        row("SYN-A-1", "src/a.rs", 5, "medium"),
        held,
        row("SYN-A-1", "src/b.rs", 9, "high"),
        row(crate::dep_audit::DEP_AUDIT_RULE_ID, "Cargo.lock", 0, "high"),
        convention,
        unchecked,
    ];
    report
}

#[test]
fn the_counts_check_against_a_package_built_by_todays_code_records_the_to_do_list() {
    let sealed = mixed_report().seal();
    let options = client_options();
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&sealed, &HashMap::new()),
        None,
        &options,
    );
    let data = render_data_json(&json).expect("data.json renders");
    let package = assemble_package(
        &crate::report_export::ReviewedReport::unreviewed(&sealed, &HashMap::new()),
        None,
        &options,
        STEM,
        (json, data, b"%PDF-1.7 stand-in for the gate tests".to_vec()),
    )
    .expect("the package assembles");
    let report_json = package.report_json.clone();
    let bytes = super::zip_package(&package, b"%PDF-1.7 stand-in for the gate tests").unwrap();
    let entries = read_zip_entries(&bytes).unwrap();

    // Anti-vacuity: the object itself is right (two places, three problem rows, one held).
    let c = &report_json.counts;
    assert_eq!(
        (c.problems.total, c.rows.problem, c.problems.needs_decision),
        (2, 3, 1)
    );
    assert_eq!(c.identity_failures(), Vec::<&str>::new());

    let problems = counts_problems(&entries, &report_json);
    let list = messages(&problems);
    // The to-do list for work order C: what the surfaces say that the counts do not.
    let out = std::env::var("COUNTS_TODO_OUT").ok();
    if let Some(path) = out {
        std::fs::write(path, &list).unwrap();
    }
    assert!(
        !problems.is_empty(),
        "the surfaces still count on their own, so the check must refuse today's package"
    );
    // The object and the files agree (work order A did that); what is left is the surfaces.
    assert!(
        !problems.iter().any(|p| p.location.starts_with("counts.")),
        "{list}"
    );
}
