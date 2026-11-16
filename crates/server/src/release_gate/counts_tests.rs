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
