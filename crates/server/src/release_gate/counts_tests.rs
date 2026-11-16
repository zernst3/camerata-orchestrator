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
use super::counts_check::{counts_problems_with, CountsChecks};
use super::tests::{client_options, drop_sheet_rows_with_text, rewrite_xlsx_part, scan_report};
use super::*;
use crate::onboard::Finding;
use crate::report_export::{build_report_json, render_data_json};

const STEM: &str = "acme-inspection";

fn package_for(report: crate::onboard::ScanReport) -> ProductPackage {
    package_with_dispositions(report, &HashMap::new())
}

/// A package whose reviewer dispositions are `dispositions` (keyed by finding key).
fn package_with_dispositions(
    report: crate::onboard::ScanReport,
    dispositions: &HashMap<String, crate::report_export::DispositionWire>,
) -> ProductPackage {
    package_with_dispositions_via(report, dispositions, true)
}

/// As [`package_with_dispositions`]; `unfold = false` is the backstop path, a reviewed report the
/// overlay did not unfold (the carried places stay on the ruled-out row).
fn package_with_dispositions_via(
    report: crate::onboard::ScanReport,
    dispositions: &HashMap<String, crate::report_export::DispositionWire>,
    unfold: bool,
) -> ProductPackage {
    let sealed = report.seal();
    let options = client_options();
    let reviewed = || {
        if unfold {
            crate::report_export::ReviewedReport::unreviewed(&sealed, dispositions)
        } else {
            crate::report_export::ReviewedReport::unreviewed_without_unfold(&sealed, dispositions)
        }
    };
    let json = build_report_json(&reviewed(), None, &options);
    let data = render_data_json(&json).expect("data.json renders");
    assemble_package(
        &reviewed(),
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

/// A package whose surfaces all agree with the counts: today's one-row package.
fn consistent() -> (BTreeMap<String, Vec<u8>>, AuditReportJson) {
    entries_of(package_for(scan_report()))
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
    report
        .methodology
        .reconciliation
        .push_str(" Plus 441 more.");
    report.cover.strip = report.cover.strip.replacen("0 low", "3 low", 1);
    report.priority_grid.left_out = "Not in this grid: 3 high places.".to_string();
    let problems = counts_problems(&entries, &report);
    for location in [
        "cover.stats.medium",
        "cover.strip",
        "methodology.reconciliation",
        "priority_grid.left_out",
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
fn a_package_built_by_todays_code_passes_the_counts_check() {
    // Work order B recorded the to-do list of the surfaces that counted on their own; work order C
    // made them read the object, so the list is empty.
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
    assert!(problems.is_empty(), "{}", messages(&problems));
}

// ── Round 1 fixes ─────────────────────────────────────────────────────────────────────────

fn xlsx_name() -> String {
    format!("{STEM}-findings.xlsx")
}

/// The (file, location) of every problem.
fn located(problems: &[super::report::Problem]) -> std::collections::BTreeSet<(String, String)> {
    problems
        .iter()
        .map(|p| (p.file.clone(), p.location.clone()))
        .collect()
}

/// The mixed report's package as the zip holds it (the counts check is not on the release path,
/// so this is not run through `release`), with the PDF data it was built from.
fn mixed_entries() -> (BTreeMap<String, Vec<u8>>, AuditReportJson) {
    let sealed = mixed_report().seal();
    let options = client_options();
    let reviewed = crate::report_export::ReviewedReport::unreviewed(&sealed, &HashMap::new());
    let json = build_report_json(&reviewed, None, &options);
    let data = render_data_json(&json).expect("data.json renders");
    let package = assemble_package(
        &reviewed,
        None,
        &options,
        STEM,
        (json, data, b"%PDF-1.7 stand-in for the gate tests".to_vec()),
    )
    .expect("the package assembles");
    let report_json = package.report_json.clone();
    let bytes = super::zip_package(&package, b"%PDF-1.7 stand-in for the gate tests").unwrap();
    (read_zip_entries(&bytes).unwrap(), report_json)
}

/// The problems a tampered package has that the untouched one does not.
fn new_problems(
    entries: &BTreeMap<String, Vec<u8>>,
    report: &AuditReportJson,
    baseline: &std::collections::BTreeSet<(String, String)>,
) -> std::collections::BTreeSet<(String, String)> {
    located(&counts_problems(entries, report))
        .difference(baseline)
        .cloned()
        .collect()
}

fn has(set: &std::collections::BTreeSet<(String, String)>, file: &str, location: &str) -> bool {
    set.iter().any(|(f, l)| f == file && l == location)
}

// Defect 1: an open place carried by a ruled-out row.

fn ruled_out_host_carrying_another_rules_place() -> (BTreeMap<String, Vec<u8>>, AuditReportJson) {
    let mut report = scan_report();
    report.findings[0].also_matches = vec!["SYN-B-1".to_string()];
    let sealed = report.clone().seal();
    let mut dispositions = HashMap::new();
    dispositions.insert(
        crate::report_export::finding_key(&sealed.findings[0]),
        crate::report_export::DispositionWire {
            state: "FalsePositive".to_string(),
            reason: "the value is a constant".to_string(),
            ..Default::default()
        },
    );
    let package = package_with_dispositions_via(report, &dispositions, false);
    let report_json = package.report_json.clone();
    let bytes = super::zip_package(&package, b"%PDF-1.7 stand-in for the gate tests").unwrap();
    (read_zip_entries(&bytes).unwrap(), report_json)
}

#[test]
fn the_gate_recount_counts_a_place_a_ruled_out_row_carries_as_a_problem_to_decide() {
    let (entries, report) = ruled_out_host_carrying_another_rules_place();
    // Anti-vacuity: the object counts the place, and findings.json lists it.
    assert_eq!(report.counts.problems.total, 1);
    assert_eq!(report.counts.problems.needs_decision, 1);
    let findings: Value = serde_json::from_slice(&entries["findings.json"]).unwrap();
    assert_eq!(findings["findings"].as_array().unwrap().len(), 0);
    assert_eq!(
        findings["open_beside_ruled_out"].as_array().unwrap().len(),
        1
    );
    let at_counts = |problems: &[super::report::Problem]| {
        problems
            .iter()
            .filter(|p| p.location.starts_with("counts."))
            .map(|p| format!("{} {}", p.file, p.location))
            .collect::<Vec<_>>()
    };
    // The recount agrees with the object (the surfaces that do not show the place yet are the
    // other locations, work order C's).
    let honest = counts_problems(&entries, &report);
    assert!(at_counts(&honest).is_empty(), "{}", messages(&honest));
    // Remove the key: the recount sees no problem and the object says one.
    let mut tampered = entries.clone();
    edit_json(&mut tampered, "findings.json", |v| {
        v.as_object_mut().unwrap().remove("open_beside_ruled_out");
    });
    let problems = counts_problems(&tampered, &report);
    assert!(
        problems
            .iter()
            .any(|p| p.file == "findings.json" && p.location == "counts.problems.total"),
        "{}",
        messages(&problems)
    );
}

// Defect 2: the gate cannot recount.

#[test]
fn the_gate_refuses_a_findings_json_with_no_fixes_array() {
    let (mut entries, report) = consistent();
    edit_json(&mut entries, "findings.json", |v| {
        v.as_object_mut().unwrap().remove("fixes");
    });
    let problems = counts_problems(&entries, &report);
    assert!(
        problems
            .iter()
            .any(|p| p.file == "findings.json" && p.message.contains("fixes array")),
        "{}",
        messages(&problems)
    );
}

#[test]
fn the_gate_refuses_a_dataset_that_is_not_a_json_array() {
    let (mut entries, report) = consistent();
    entries.insert("json/dependencies.json".to_string(), b"{}".to_vec());
    let problems = counts_problems(&entries, &report);
    assert!(
        problems
            .iter()
            .any(|p| p.file == "json/dependencies.json" && p.message.contains("not a JSON array")),
        "{}",
        messages(&problems)
    );
}

#[test]
fn the_gate_refuses_a_missing_dataset() {
    let (mut entries, report) = consistent();
    entries.remove("json/false-positives.json");
    let problems = counts_problems(&entries, &report);
    assert!(
        problems
            .iter()
            .any(|p| p.file == "json/false-positives.json" && p.message.contains("missing")),
        "{}",
        messages(&problems)
    );
}

#[test]
fn the_gate_refuses_a_workbook_with_a_sheet_renamed() {
    let (mut entries, report) = consistent();
    let name = xlsx_name();
    entries.insert(
        name.clone(),
        rewrite_xlsx_part(&entries[&name], "xl/workbook.xml", |xml| {
            assert!(xml.contains("name=\"Fix plan\""), "anti-vacuity");
            xml.replace("name=\"Fix plan\"", "name=\"Fix planned\"")
        }),
    );
    let problems = counts_problems(&entries, &report);
    assert!(
        problems
            .iter()
            .any(|p| p.file == name && p.location == "sheet \"Fix plan\" rows"),
        "{}",
        messages(&problems)
    );
}

#[test]
fn the_gate_refuses_a_start_here_sheet_with_a_sheet_row_removed() {
    let (mut entries, report) = consistent();
    let name = xlsx_name();
    entries.insert(
        name.clone(),
        drop_sheet_rows_with_text(&entries[&name], "Fix plan"),
    );
    let problems = counts_problems(&entries, &report);
    assert!(
        problems.iter().any(|p| p.file == name
            && p.location == "sheet \"Start here\" row count, sheet Fix plan"
            && p.message.contains("no row count")),
        "{}",
        messages(&problems)
    );
}

#[test]
fn the_gate_refuses_a_package_with_no_workbook_or_no_ticket_file() {
    let (mut entries, report) = consistent();
    entries.remove(&xlsx_name());
    entries.remove("csv/tickets.csv");
    let problems = counts_problems(&entries, &report);
    assert!(
        problems.iter().any(|p| p.file == xlsx_name()),
        "{}",
        messages(&problems)
    );
    assert!(
        problems.iter().any(|p| p.file == "csv/tickets.csv"),
        "{}",
        messages(&problems)
    );
}

// Defect 3: one tamper test for each surface check 3 compares.

/// Set the number in the third column of the Start here row for `sheet`.
fn set_start_here_count(xlsx: &[u8], sheet: &str, to: usize) -> Vec<u8> {
    let grid = super::archive::xlsx_sheet_grid(xlsx, "Start here").unwrap();
    let row = grid
        .iter()
        .position(|r| r.first().is_some_and(|c| c == sheet))
        .unwrap_or_else(|| panic!("Start here has no row for {sheet}"))
        + 1;
    let cell = format!("r=\"C{row}\"");
    rewrite_xlsx_part(xlsx, "xl/worksheets/sheet1.xml", |xml| {
        let at = xml.find(&cell).expect("the count cell");
        let v0 = xml[at..].find("<v>").expect("a value") + at + 3;
        let v1 = xml[v0..].find("</v>").expect("a closed value") + v0;
        format!("{}{to}{}", &xml[..v0], &xml[v1..])
    })
}

#[test]
fn the_gate_refuses_a_row_removed_from_all_problems() {
    let (entries, report) = mixed_entries();
    let baseline = located(&counts_problems(&entries, &report));
    let mut tampered = entries.clone();
    let name = xlsx_name();
    tampered.insert(
        name.clone(),
        drop_sheet_rows_with_text(&tampered[&name], "src/b.rs"),
    );
    let new = new_problems(&tampered, &report, &baseline);
    assert!(has(&new, &name, "sheet \"All problems\" rows"), "{new:?}");
}

#[test]
fn the_gate_refuses_a_row_removed_from_needs_a_decision() {
    let (entries, report) = mixed_entries();
    let baseline = located(&counts_problems(&entries, &report));
    let mut tampered = entries.clone();
    let name = xlsx_name();
    tampered.insert(
        name.clone(),
        drop_sheet_rows_with_text(&tampered[&name], "src/a.rs"),
    );
    let new = new_problems(&tampered, &report, &baseline);
    assert!(
        has(&new, &name, "sheet \"Needs a decision\" rows"),
        "{new:?}"
    );
}

#[test]
fn the_gate_refuses_a_start_here_count_that_was_changed() {
    let (entries, report) = mixed_entries();
    let baseline = located(&counts_problems(&entries, &report));
    let mut tampered = entries.clone();
    let name = xlsx_name();
    tampered.insert(
        name.clone(),
        set_start_here_count(&tampered[&name], "Needs a decision", 99),
    );
    let new = new_problems(&tampered, &report, &baseline);
    assert!(
        has(
            &new,
            &name,
            "sheet \"Start here\" row count, sheet Needs a decision"
        ),
        "{new:?}"
    );
}

#[test]
fn the_gate_refuses_a_grid_cell_that_was_emptied() {
    let (entries, mut report) = mixed_entries();
    let baseline = located(&counts_problems(&entries, &report));
    let cell = report
        .priority_grid
        .rows
        .iter_mut()
        .flat_map(|r| r.cells.iter_mut())
        .find(|c| !c.findings.is_empty())
        .expect("anti-vacuity: the grid holds a problem");
    cell.findings.clear();
    let new = new_problems(&entries, &report, &baseline);
    assert!(has(&new, "PDF data", "priority_grid"), "{new:?}");
}

#[test]
fn the_sheet_row_count_finds_its_header_by_name_on_every_sheet() {
    let (entries, report) = mixed_entries();
    let xlsx = &entries[&xlsx_name()];
    let want = &report.counts;
    for (sheet, rows) in [
        ("All problems", want.problems.total),
        ("Needs a decision", want.problems.needs_decision),
        ("Fix plan", want.fixes.to_do),
        ("Dependencies", want.rows.dependency),
        ("Your conventions", want.rows.convention),
    ] {
        assert_eq!(
            super::counts_check::sheet_rows(xlsx, sheet),
            Ok(rows),
            "{sheet}"
        );
    }
    assert!(
        want.problems.needs_decision > 0 && want.fixes.to_do > 0,
        "anti-vacuity"
    );
}

// The identities are the gate's own arithmetic, on the object too.

#[test]
fn the_gate_refuses_an_object_that_breaks_an_identity() {
    let (mut entries, report) = consistent();
    edit_json(&mut entries, "findings.json", |v| {
        v["counts"]["problems"]["accepted"] = json!(1);
    });
    let problems = counts_problems(&entries, &report);
    assert!(
        problems
            .iter()
            .any(|p| p.file == "findings.json" && p.location == "identities"),
        "{}",
        messages(&problems)
    );
}

// The next steps must state the sentence.

#[test]
fn the_gate_refuses_next_steps_that_do_not_state_the_decision_sentence() {
    let (entries, mut report) = consistent();
    report.methodology.next_steps = "Fix the first group, then the next.".to_string();
    let problems = counts_problems(&entries, &report);
    assert!(
        problems
            .iter()
            .any(|p| p.location == "methodology.next_steps"),
        "{}",
        messages(&problems)
    );
}

// Whole-file places per rule.

#[test]
fn the_gate_recount_counts_whole_file_problems_of_three_rules_as_three_places() {
    let mut report = scan_report();
    let mut rows = Vec::new();
    for rule in ["SYN-A-1", "SYN-B-1", "SYN-C-1"] {
        let mut f = report.findings[0].clone();
        f.path = "src/whole.rs".to_string();
        f.line = 0;
        f.rule_id = rule.to_string();
        rows.push(f);
    }
    report.findings = rows;
    let (entries, report_json) = entries_of(package_for(report));
    assert_eq!(report_json.counts.problems.total, 3, "the object");
    let honest = counts_problems(&entries, &report_json);
    assert!(
        !honest.iter().any(|p| p.location.starts_with("counts.")),
        "the recount agrees: {}",
        messages(&honest)
    );
}

// ── Needs a decision is one number on every surface ──────────────────────────────────────

/// Three places: one with a row to act on and a held row of lower severity (the head row is the
/// one to act on), one to act on, one held.
fn decision_report() -> crate::onboard::ScanReport {
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
    let mut held_beside = row("SYN-B-1", "src/a.rs", 5, "medium");
    held_beside.needs_review = true;
    let mut held_alone = row("SYN-A-1", "src/c.rs", 3, "medium");
    held_alone.needs_review = true;
    let mut report = scan_report();
    report.findings = vec![
        row("SYN-A-1", "src/a.rs", 5, "high"),
        held_beside,
        row("SYN-A-1", "src/b.rs", 9, "high"),
        held_alone,
    ];
    report
}

/// The integer written just before `marker` in `text`.
fn integer_before(text: &str, marker: &str) -> Option<usize> {
    let at = text.find(marker)?;
    text[..at]
        .trim_end()
        .rsplit(' ')
        .next()?
        .replace(',', "")
        .parse()
        .ok()
}

#[test]
fn needs_a_decision_is_one_number_on_every_surface() {
    let sealed = decision_report().seal();
    let options = client_options();
    let reviewed = crate::report_export::ReviewedReport::unreviewed(&sealed, &HashMap::new());
    let json = build_report_json(&reviewed, None, &options);
    let data = render_data_json(&json).expect("data.json renders");
    let package = assemble_package(
        &reviewed,
        None,
        &options,
        STEM,
        (json, data, b"%PDF-1.7 stand-in for the gate tests".to_vec()),
    )
    .expect("the package assembles");
    let report = package.report_json.clone();
    let bytes = super::zip_package(&package, b"%PDF-1.7 stand-in for the gate tests").unwrap();
    let entries = read_zip_entries(&bytes).unwrap();

    // The object: any held row makes the place need a decision.
    let n = report.counts.problems.needs_decision;
    assert_eq!((n, report.counts.problems.total), (2, 3), "anti-vacuity");

    // The narrative.
    let narrative = &report.executive_summary.narrative;
    assert_eq!(
        integer_before(narrative, " need a"),
        Some(n),
        "narrative: {narrative:?}"
    );
    // The next steps sentence.
    let sentence = format!("{n} of 3 problems need a reviewer's decision");
    assert!(
        report.methodology.next_steps.contains(&sentence),
        "next steps: {:?}",
        report.methodology.next_steps
    );
    // The grid's count of problems it leaves out for a decision.
    assert_eq!(report.priority_grid.held_count, n, "the grid");
    // The sheet, the Start here count and findings.json.
    let xlsx = &entries[&xlsx_name()];
    assert_eq!(
        super::counts_check::sheet_rows(xlsx, "Needs a decision"),
        Ok(n),
        "the Needs a decision sheet"
    );
    let start = super::archive::xlsx_sheet_grid(xlsx, "Start here").unwrap();
    let said = start
        .iter()
        .find(|r| r.first().is_some_and(|c| c == "Needs a decision"))
        .and_then(|r| r.get(2))
        .and_then(|c| c.parse::<f64>().ok())
        .map(|x| x as usize);
    assert_eq!(said, Some(n), "Start here");
    let findings: Value = serde_json::from_slice(&entries["findings.json"]).unwrap();
    assert_eq!(
        findings["counts"]["problems"]["needs_decision"],
        json!(n),
        "findings.json counts"
    );
    // The summary's sentence states the same number.
    let summary = findings["summary"]["narrative"]
        .as_str()
        .unwrap_or_default();
    assert_eq!(
        integer_before(summary, " need a"),
        Some(n),
        "findings.json summary: {summary:?}"
    );
    // And the gate agrees with all of them.
    let problems = counts_problems(&entries, &report);
    assert!(problems.is_empty(), "{}", messages(&problems));
}

// ── Work order C: the gate is on the release path, and it holds the rest of the surfaces ──

#[test]
fn a_package_whose_printed_numbers_differ_from_its_files_is_refused_on_the_release_path() {
    let mut package = package_for(scan_report());
    // One digit of the narrative changed after the package was built.
    package.report_json.executive_summary.narrative = package
        .report_json
        .executive_summary
        .narrative
        .replacen("10 files", "11 files", 1);
    match release(package) {
        Err(ReleaseError::Blocked(failure)) => {
            let counts = failure
                .report
                .checks
                .iter()
                .find(|c| c.check == CheckId::Counts)
                .expect("the counts check ran");
            assert!(
                counts
                    .problems
                    .iter()
                    .any(|p| p.location == "executive_summary.narrative"),
                "{:?}",
                counts.problems
            );
        }
        other => panic!("a package whose numbers differ must be refused: {other:?}"),
    }
}

/// Set the number in `column` of the Start here row whose first cell is `label`.
fn set_start_here_cell(xlsx: &[u8], label: &str, column: char, to: usize) -> Vec<u8> {
    let grid = super::archive::xlsx_sheet_grid(xlsx, "Start here").unwrap();
    let row = grid
        .iter()
        .position(|r| r.first().is_some_and(|c| c == label))
        .unwrap_or_else(|| panic!("Start here has no row for {label}"))
        + 1;
    let cell = format!("r=\"{column}{row}\"");
    rewrite_xlsx_part(xlsx, "xl/worksheets/sheet1.xml", |xml| {
        let at = xml.find(&cell).expect("the cell");
        let v0 = xml[at..].find("<v>").expect("a value") + at + 3;
        let v1 = xml[v0..].find("</v>").expect("a closed value") + v0;
        format!("{}{to}{}", &xml[..v0], &xml[v1..])
    })
}

#[test]
fn the_gate_refuses_a_scorecard_total_that_was_changed() {
    let (entries, report) = mixed_entries();
    let baseline = located(&counts_problems(&entries, &report));
    let mut tampered = entries.clone();
    let name = xlsx_name();
    tampered.insert(
        name.clone(),
        set_start_here_cell(&tampered[&name], "Total", 'F', 99),
    );
    let new = new_problems(&tampered, &report, &baseline);
    assert!(
        has(&new, &name, "sheet \"Start here\" scorecard Total"),
        "{new:?}"
    );
}

#[test]
fn the_gate_refuses_a_not_checked_sheet_that_lost_the_row_of_a_rule_that_could_not_be_completed() {
    let (entries, report) = mixed_entries();
    assert_eq!(report.counts.rows.not_checked, 1, "anti-vacuity");
    let baseline = located(&counts_problems(&entries, &report));
    let mut tampered = entries.clone();
    let name = xlsx_name();
    tampered.insert(
        name.clone(),
        drop_sheet_rows_with_text(&tampered[&name], "SYN-D-1"),
    );
    let new = new_problems(&tampered, &report, &baseline);
    assert!(has(&new, &name, "sheet \"Not checked\" rows"), "{new:?}");
}

#[test]
fn the_gate_refuses_a_grid_note_that_states_other_counts() {
    let (entries, mut report) = mixed_entries();
    let baseline = located(&counts_problems(&entries, &report));
    report.priority_grid.accepted_line = "5 problems are accepted risk.".to_string();
    report.priority_grid.held_count += 4;
    let new = new_problems(&entries, &report, &baseline);
    assert!(
        has(&new, "PDF data", "priority_grid.accepted_line"),
        "{new:?}"
    );
    assert!(has(&new, "PDF data", "priority_grid.held_count"), "{new:?}");
}

/// Test 9 (single counts): a row a baseline record closed, with no reviewer's decision.
fn with_a_baseline_row(disposition_kind: &str) -> Vec<super::report::Problem> {
    let (mut entries, report) = consistent();
    edit_json(&mut entries, "json/all-findings.json", |v| {
        v[0]["status"] = json!("carried-baseline");
        v[0]["disposition_kind"] = json!(disposition_kind);
    });
    counts_problems(&entries, &report)
}

#[test]
fn an_accepted_baseline_row_without_a_decision_blocks_release() {
    let problems = with_a_baseline_row("BaselineAccepted");
    assert!(
        problems.iter().any(|p| p
            .message
            .contains("no reviewer has decided it this session")),
        "{}",
        messages(&problems)
    );
}

#[test]
fn a_baseline_row_a_reviewer_decided_does_not_block_release() {
    let problems = with_a_baseline_row("Confirmed");
    assert!(
        !problems.iter().any(|p| p
            .message
            .contains("no reviewer has decided it this session")),
        "{}",
        messages(&problems)
    );
}

#[test]
fn the_gate_refuses_a_findings_json_summary_that_contradicts_the_counts() {
    let (mut entries, report) = consistent();
    edit_json(&mut entries, "findings.json", |v| {
        v["summary"]["held_for_review"] = json!(99);
        v["summary"]["curated_total"] = json!(1);
    });
    let problems = counts_problems(&entries, &report);
    let text = messages(&problems);
    assert!(text.contains("summary.held_for_review"), "{text}");
    assert!(text.contains("summary.curated_total"), "{text}");
}

// ── Follow ups to work order C ────────────────────────────────────────────────────────────

fn row_of(
    rule: &str,
    path: &str,
    line: usize,
    kind: camerata_api_types::audit::FindingKind,
) -> Finding {
    Finding {
        repo: "acme/portal".to_string(),
        path: path.to_string(),
        line,
        rule_id: rule.to_string(),
        severity: "medium".to_string(),
        snippet: format!("snippet {line}"),
        detail: format!("detail {line}"),
        kind: Some(kind),
        ..Finding::default()
    }
}

fn departure() -> camerata_api_types::audit::FindingKind {
    camerata_api_types::audit::FindingKind::Deviation(camerata_api_types::audit::DeviationKind {
        deviating_sites: 1,
        assessed_sites: Some(4),
        convention_option_id: None,
        convention_label: "the usual way".to_string(),
    })
}

fn convention() -> camerata_api_types::audit::FindingKind {
    camerata_api_types::audit::FindingKind::ConventionObserved(
        camerata_api_types::audit::ConventionObservedKind {
            convention_option_id: "x".to_string(),
            convention_label: "the usual way".to_string(),
        },
    )
}

fn fix_plan_rows(bytes: &[u8]) -> usize {
    let entries = read_zip_entries(bytes).expect("zip");
    let xlsx = entries
        .iter()
        .find(|(n, _)| n.ends_with("-findings.xlsx"))
        .expect("workbook")
        .1;
    let grid = super::archive::xlsx_sheet_grid(xlsx, crate::xlsx_export::vocabulary::FIX_PLAN)
        .expect("fix plan sheet");
    let col = grid[0]
        .iter()
        .position(|h| h == "Fix id")
        .expect("a Fix id column");
    // A row that stands for a Fix names it; the empty sheet holds one line saying so.
    grid.iter()
        .skip(1)
        .filter(|r| r.get(col).is_some_and(|c| !c.is_empty()))
        .count()
}

#[test]
fn an_accepted_departure_beside_a_convention_row_releases_and_the_fix_plan_has_no_row_for_it() {
    let mut report = scan_report();
    report.findings = vec![
        row_of("SEC-NO-RAW-SQL-CONCAT-1", "src/db.rs", 12, departure()),
        row_of("SEC-NO-RAW-SQL-CONCAT-1", "src/db.rs", 40, convention()),
    ];
    let sealed = report.clone().seal();
    let mut dispositions = HashMap::new();
    dispositions.insert(
        crate::report_export::finding_key(&sealed.findings[0]),
        crate::report_export::DispositionWire {
            state: "Ignored".to_string(),
            reason: "we accept this one".to_string(),
            ..Default::default()
        },
    );
    let package = package_with_dispositions(report, &dispositions);
    assert_eq!(
        package.report_json.counts.problems.accepted, 1,
        "anti-vacuity"
    );
    let released = release(package).expect("the package releases");
    assert_eq!(fix_plan_rows(&released.bytes), 0);
}

#[test]
fn a_reviewed_package_with_accepted_ruled_out_and_confirmed_rows_clears_the_counts_check() {
    let mut report = scan_report();
    report.findings = vec![
        row_of(
            "SEC-NO-RAW-SQL-CONCAT-1",
            "src/a.rs",
            1,
            camerata_api_types::audit::FindingKind::Defect,
        ),
        row_of(
            "SEC-NO-RAW-SQL-CONCAT-1",
            "src/b.rs",
            2,
            camerata_api_types::audit::FindingKind::Defect,
        ),
        row_of(
            "SEC-NO-RAW-SQL-CONCAT-1",
            "src/c.rs",
            3,
            camerata_api_types::audit::FindingKind::Defect,
        ),
    ];
    let sealed = report.clone().seal();
    let mut dispositions = HashMap::new();
    for (i, (state, reason)) in [
        ("Ignored", "accepted"),
        ("FalsePositive", "a constant"),
        ("Confirmed", ""),
    ]
    .into_iter()
    .enumerate()
    {
        dispositions.insert(
            crate::report_export::finding_key(&sealed.findings[i]),
            crate::report_export::DispositionWire {
                state: state.to_string(),
                reason: reason.to_string(),
                ..Default::default()
            },
        );
    }
    let package = package_with_dispositions(report, &dispositions);
    let report_json = package.report_json.clone();
    assert_eq!(report_json.counts.rows.ruled_out, 1, "anti-vacuity");
    assert_eq!(report_json.counts.problems.accepted, 1, "anti-vacuity");
    release(package).expect("a reviewed package clears the gate with the counts check on");
}

#[test]
fn the_gate_refuses_an_and_n_more_line_that_disagrees_with_the_counts() {
    let (entries, report) = consistent();
    assert!(
        report.dependency_snapshot.more_line.is_empty(),
        "anti-vacuity"
    );
    let sheet = crate::xlsx_export::vocabulary::DEPENDENCIES;
    let mut wrong = report.clone();
    wrong.dependency_snapshot.more_line = crate::xlsx_export::vocabulary::more_in_sheet(7, sheet);
    let text = messages(&counts_problems(&entries, &wrong));
    assert!(text.contains("dependency_snapshot.more_line"), "{text}");

    let mut wrong = report.clone();
    wrong.escalations.more_line =
        crate::xlsx_export::vocabulary::more_in_sheet(3, crate::xlsx_export::vocabulary::FIX_PLAN);
    let text = messages(&counts_problems(&entries, &wrong));
    assert!(text.contains("escalations.more_line"), "{text}");

    let mut wrong = report.clone();
    wrong.conventions.more_line = crate::xlsx_export::vocabulary::more_in_sheet(
        2,
        crate::xlsx_export::vocabulary::DEPARTURES,
    );
    let text = messages(&counts_problems(&entries, &wrong));
    assert!(text.contains("conventions.more_line"), "{text}");
}

// Single counts D, round 1: one order rule on every surface.

/// One fix with a high place that has no effort estimate and a medium place, a low place held for
/// a decision (a fix of its own), one accepted place and one ruled out place.
fn order_rule_package() -> (BTreeMap<String, Vec<u8>>, AuditReportJson) {
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
    let mut held = row("SYN-H-1", "src/h.rs", 3, "low");
    held.needs_review = true;
    let mut report = scan_report();
    report.findings = vec![
        row("SYN-A-1", "src/a.rs", 5, "high"),
        row("SYN-A-1", "src/b.rs", 9, "medium"),
        held,
        row("SYN-I-1", "src/i.rs", 2, "medium"),
        row("SYN-F-1", "src/f.rs", 4, "medium"),
    ];
    let sealed = report.clone().seal();
    let key_of = |rule: &str| {
        crate::report_export::finding_key(
            sealed.findings.iter().find(|f| f.rule_id == rule).unwrap(),
        )
    };
    let mut dispositions = HashMap::new();
    dispositions.insert(
        key_of("SYN-I-1"),
        crate::report_export::DispositionWire {
            state: "Ignored".to_string(),
            reason: "we accept this one".to_string(),
            ..Default::default()
        },
    );
    dispositions.insert(
        key_of("SYN-F-1"),
        crate::report_export::DispositionWire {
            state: "FalsePositive".to_string(),
            reason: "a constant".to_string(),
            ..Default::default()
        },
    );
    let package = package_with_dispositions(report, &dispositions);
    let report_json = package.report_json.clone();
    let bytes = super::zip_package(&package, b"%PDF-1.7 stand-in for the gate tests").unwrap();
    (read_zip_entries(&bytes).unwrap(), report_json)
}

#[test]
fn every_surface_gives_a_fix_the_one_order_and_the_gate_passes() {
    let (entries, report) = order_rule_package();
    let p = &report.counts.problems;
    // Anti-vacuity: the high place with no estimate is First, the medium one Later, the held one
    // needs a decision, one is accepted.
    assert_eq!(
        (p.first, p.next, p.later, p.needs_decision, p.accepted),
        (1, 0, 1, 1, 1),
        "{p:?}"
    );
    // The summary, the box of three things and the fix lines say First for the one fix to do.
    assert!(
        report
            .executive_summary
            .narrative
            .contains("1 First, 0 Next, 1 Later"),
        "{}",
        report.executive_summary.narrative
    );
    assert_eq!(report.three_things.items.len(), 1);
    let lines: Vec<_> = report
        .other_fixes
        .groups
        .iter()
        .flat_map(|g| &g.lines)
        .collect();
    assert_eq!(lines.len(), 1, "the held fix is not a fix to do: {lines:?}");
    assert_eq!(lines[0].fix_order, "First");
    assert!(!lines[0].fix_id.is_empty());
    // The workbook: one Fix plan row, First; All problems says each place's own word.
    let xlsx = &entries[&xlsx_name()];
    let plan = super::archive::xlsx_sheet_grid(xlsx, "Fix plan").unwrap();
    let order = plan[0].iter().position(|h| h == "Fix order").unwrap();
    assert_eq!(plan.len(), 2, "{plan:?}");
    assert_eq!(plan[1][order], "First");
    let all = super::archive::xlsx_sheet_grid(xlsx, "All problems").unwrap();
    let order = all[0].iter().position(|h| h == "Fix order").unwrap();
    let mut words: Vec<&str> = all.iter().skip(1).map(|r| r[order].as_str()).collect();
    words.sort_unstable();
    assert_eq!(
        words,
        ["Accepted risk", "First", "Later", "Needs a decision"]
    );
    // The tickets file lists the one fix.
    let tickets = csv::Reader::from_reader(entries["csv/tickets.csv"].as_slice())
        .records()
        .count();
    assert_eq!(tickets, 1);
    assert_eq!(report.counts.fixes.to_do, 1);
    let problems = counts_problems(&entries, &report);
    assert!(problems.is_empty(), "{}", messages(&problems));
}

#[test]
fn the_gate_refuses_a_forged_fix_plan_order_and_a_forged_fix_line() {
    let (entries, report) = order_rule_package();
    let baseline = located(&counts_problems(&entries, &report));
    let name = xlsx_name();
    let mut tampered = entries.clone();
    tampered.insert(
        name.clone(),
        rewrite_xlsx_part(&tampered[&name], "xl/sharedStrings.xml", |xml| {
            assert!(xml.contains("<t>First</t>"), "anti-vacuity");
            xml.replace("<t>First</t>", "<t>Later</t>")
        }),
    );
    let new = new_problems(&tampered, &report, &baseline);
    assert!(
        new.iter()
            .any(|(f, l)| f == &name && l.starts_with("sheet \"Fix plan\", fix ")),
        "{new:?}"
    );
    assert!(
        new.iter()
            .any(|(f, l)| f == &name && l.starts_with("sheet \"All problems\" Fix order")),
        "{new:?}"
    );
    // A fix line in the PDF data that says another order than the places give.
    let mut forged = report.clone();
    forged.other_fixes.groups[0].lines[0].fix_order = "Next".to_string();
    let new = new_problems(&entries, &forged, &baseline);
    assert!(
        new.iter()
            .any(|(f, l)| f == "PDF data" && l.starts_with("other_fixes, fix ")),
        "{new:?}"
    );
    // A box of three things that is empty while a problem is First.
    let mut empty_box = report.clone();
    empty_box.three_things.items.clear();
    let new = new_problems(&entries, &empty_box, &baseline);
    assert!(has(&new, "PDF data", "three_things"), "{new:?}");
}

// Single counts D, round 2.

/// One synthetic package from `rows`, with the given reviewer states by rule id.
fn package_of_rows(
    rows: Vec<Finding>,
    states: &[(&str, &str)],
) -> (BTreeMap<String, Vec<u8>>, AuditReportJson) {
    // `states` pairs a path with the reviewer's state for the row there.
    let mut report = scan_report();
    report.findings = rows;
    let sealed = report.clone().seal();
    let mut dispositions = HashMap::new();
    for (rule, state) in states {
        let f = sealed.findings.iter().find(|f| f.path == *rule).unwrap();
        dispositions.insert(
            crate::report_export::finding_key(f),
            crate::report_export::DispositionWire {
                state: (*state).to_string(),
                reason: "decided".to_string(),
                ..Default::default()
            },
        );
    }
    let package = package_with_dispositions(report, &dispositions);
    let report_json = package.report_json.clone();
    let bytes = super::zip_package(&package, b"%PDF-1.7 stand-in for the gate tests").unwrap();
    (read_zip_entries(&bytes).unwrap(), report_json)
}

fn plain_row(rule: &str, path: &str, line: usize, severity: &str) -> Finding {
    Finding {
        repo: "acme/portal".to_string(),
        path: path.to_string(),
        line,
        rule_id: rule.to_string(),
        severity: severity.to_string(),
        snippet: format!("let x{line} = 1;"),
        detail: format!("detail {rule}"),
        ..Finding::default()
    }
}

#[test]
fn a_fix_with_one_accepted_place_and_one_held_place_is_not_a_fix_to_do() {
    let mut held = plain_row("SYN-A-1", "src/b.rs", 9, "medium");
    held.needs_review = true;
    let (entries, report) = package_of_rows(
        vec![plain_row("SYN-A-1", "src/a.rs", 5, "medium"), held],
        &[("src/a.rs", "Ignored")],
    );
    let p = &report.counts.problems;
    assert_eq!(
        (p.accepted, p.needs_decision),
        (1, 1),
        "anti-vacuity: {p:?}"
    );
    assert_eq!(report.counts.fixes.to_do, 0, "{:?}", report.counts.fixes);
    let plan = super::archive::xlsx_sheet_grid(&entries[&xlsx_name()], "Fix plan").unwrap();
    let id = plan[0].iter().position(|h| h == "Fix id").unwrap();
    let rows = plan.iter().skip(1).filter(|r| !r[id].is_empty()).count();
    assert_eq!(rows, 0, "{plan:?}");
    let tickets = csv::Reader::from_reader(entries["csv/tickets.csv"].as_slice())
        .records()
        .count();
    assert_eq!(tickets, 0);
    let problems = counts_problems(&entries, &report);
    assert!(problems.is_empty(), "{}", messages(&problems));
}

const ONE_DEPENDENCY_FIRST: &str =
    "No code problem is due first this run. 1 dependency fix is due first; see Dependencies.";

/// A medium code problem (Later) and a high advisory (First): the box is empty, one dependency fix
/// is due first.
fn dependency_first_package() -> (BTreeMap<String, Vec<u8>>, AuditReportJson) {
    package_of_rows(
        vec![
            plain_row("SYN-A-1", "src/a.rs", 5, "medium"),
            plain_row(crate::dep_audit::DEP_AUDIT_RULE_ID, "Cargo.lock", 0, "high"),
        ],
        &[],
    )
}

#[test]
fn an_empty_box_names_the_dependency_fix_due_first_and_the_gate_checks_the_number() {
    let (entries, report) = dependency_first_package();
    assert!(report.three_things.items.is_empty(), "anti-vacuity");
    assert_eq!(report.three_things.total_hours_label, ONE_DEPENDENCY_FIRST);
    let problems = counts_problems(&entries, &report);
    assert!(problems.is_empty(), "{}", messages(&problems));
    // The old sentence, with a dependency fix First, is refused; so is a wrong number.
    for forged in [
        "No fix is due first this run.",
        "No code problem is due first this run. 2 dependency fixes are due first; see Dependencies.",
    ] {
        let mut tampered = report.clone();
        tampered.three_things.total_hours_label = forged.to_string();
        let problems = counts_problems(&entries, &tampered);
        assert!(
            problems
                .iter()
                .any(|p| p.file == "PDF data" && p.location == "three_things.total_hours_label"),
            "{forged}: {}",
            messages(&problems)
        );
    }
}

#[test]
fn an_empty_box_with_no_dependency_fix_first_keeps_the_old_sentence() {
    let (entries, report) =
        package_of_rows(vec![plain_row("SYN-A-1", "src/a.rs", 5, "medium")], &[]);
    assert_eq!(
        report.three_things.total_hours_label,
        "No fix is due first this run."
    );
    let problems = counts_problems(&entries, &report);
    assert!(problems.is_empty(), "{}", messages(&problems));
}

// Single counts D, round 2, item 3: the order check fails closed.

fn fix_order_problems(
    entries: &BTreeMap<String, Vec<u8>>,
    report: &AuditReportJson,
) -> Vec<super::report::Problem> {
    counts_problems(entries, report)
        .into_iter()
        .filter(|p| p.location.contains("fix order"))
        .collect()
}

#[test]
fn the_order_check_pushes_a_problem_for_a_missing_workbook() {
    let (mut entries, report) = consistent();
    assert!(
        fix_order_problems(&entries, &report).is_empty(),
        "anti-vacuity"
    );
    entries.remove(&xlsx_name());
    let found = fix_order_problems(&entries, &report);
    assert!(
        found.iter().any(|p| p.file == xlsx_name()),
        "{}",
        messages(&found)
    );
}

#[test]
fn the_order_check_pushes_a_problem_for_an_unreadable_sheet() {
    for sheet in ["Fix plan", "All problems"] {
        let (mut entries, report) = consistent();
        let name = xlsx_name();
        entries.insert(
            name.clone(),
            rewrite_xlsx_part(&entries[&name], "xl/workbook.xml", |xml| {
                assert!(xml.contains(&format!("name=\"{sheet}\"")), "anti-vacuity");
                xml.replace(&format!("name=\"{sheet}\""), &format!("name=\"{sheet} x\""))
            }),
        );
        let found = fix_order_problems(&entries, &report);
        assert!(
            found
                .iter()
                .any(|p| p.location == format!("sheet \"{sheet}\" fix order")),
            "{sheet}: {}",
            messages(&found)
        );
    }
}

#[test]
fn the_order_check_pushes_a_problem_for_a_missing_column() {
    // (the header renamed, the sheet whose check must notice)
    for (header, sheet) in [
        ("Fix order", "All problems"),
        ("Fix order", "Fix plan"),
        ("Severity", "All problems"),
        ("Fix id", "Fix plan"),
    ] {
        let (mut entries, report) = consistent();
        let name = xlsx_name();
        entries.insert(
            name.clone(),
            rewrite_xlsx_part(&entries[&name], "xl/sharedStrings.xml", |xml| {
                let cell = format!(">{header}<");
                assert!(xml.contains(&cell), "anti-vacuity: {header}");
                xml.replace(&cell, &format!(">{header} renamed<"))
            }),
        );
        let found = fix_order_problems(&entries, &report);
        assert!(
            found
                .iter()
                .any(|p| p.location == format!("sheet \"{sheet}\" fix order")
                    && p.message.contains(header)),
            "{header} on {sheet}: {}",
            messages(&found)
        );
    }
}
