//! The release gate and the "Patterns across services" section: it is an observation, so it must
//! change no severity total and no finding count, and the gate holds the package to that.
//! A real package is built through `assemble_package` around a stand-in PDF, like the gate's
//! other tests, so these run without Typst.

use std::collections::{BTreeMap, HashMap};

use serde_json::{json, Value};

use super::tests::{client_options, scan_report};
use super::*;
use crate::report_export::{build_report_json, render_data_json};
use crate::scan_ledger::RuleTier;

const STEM: &str = "acme-inspection";
const RULE: &str = "SEC-NO-RAW-SQL-CONCAT-1";

/// `acme/portal` violates the rule (one finding) and `acme/web` was checked and is clean: one
/// pattern row.
fn two_repo_report() -> crate::onboard::ScanReport {
    let mut report = scan_report();
    report.repos.push("acme/web".to_string());
    report
        .ledger
        .record_rule("acme/portal", RULE, RuleTier::Semantic, true, None, 5, 1);
    report
        .ledger
        .record_rule("acme/web", RULE, RuleTier::Semantic, true, None, 5, 0);
    report
}

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

#[test]
fn a_multi_repo_package_with_a_patterns_section_clears_the_gate() {
    let (entries, report) = entries_of(package_for(two_repo_report()));
    assert_eq!(
        report.patterns_across_services.as_ref().unwrap().rows.len(),
        1,
        "anti-vacuity: the section has its one row"
    );
    assert!(patterns_section_problems(&entries, &report).is_empty());
    let findings: Value = serde_json::from_slice(&entries["json/inspection.json"]).unwrap();
    assert_eq!(
        findings["patterns_across_services"]["rows"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(findings["findings"].as_array().unwrap().len(), 1);
}

#[test]
fn a_one_repo_package_has_no_patterns_key_and_the_gate_is_clean() {
    let (entries, report) = entries_of(package_for(scan_report()));
    assert!(report.patterns_across_services.is_none());
    assert!(patterns_section_problems(&entries, &report).is_empty());
    let findings: Value = serde_json::from_slice(&entries["json/inspection.json"]).unwrap();
    assert!(findings.get("patterns_across_services").is_none());
}

#[test]
fn the_gate_flags_a_pattern_row_counted_among_the_findings() {
    let (mut entries, report) = entries_of(package_for(two_repo_report()));
    let mut findings: Value = serde_json::from_slice(&entries["json/inspection.json"]).unwrap();
    findings["findings"]
        .as_array_mut()
        .unwrap()
        .push(json!({"rule_id": RULE, "statement": "x", "cells": []}));
    entries.insert(
        "json/inspection.json".to_string(),
        serde_json::to_vec(&findings).unwrap(),
    );
    let problems = patterns_section_problems(&entries, &report);
    assert!(
        problems
            .iter()
            .any(|p| p.message.contains("inside the findings array")),
        "{problems:?}"
    );
}

#[test]
fn the_gate_flags_a_severity_inside_the_section_and_a_missing_or_extra_key() {
    let (mut entries, report) = entries_of(package_for(two_repo_report()));
    let original: Value = serde_json::from_slice(&entries["json/inspection.json"]).unwrap();

    // A severity on a pattern row.
    let mut with_severity = original.clone();
    with_severity["patterns_across_services"]["rows"][0]["severity"] = json!("high");
    entries.insert(
        "json/inspection.json".to_string(),
        serde_json::to_vec(&with_severity).unwrap(),
    );
    assert!(
        patterns_section_problems(&entries, &report)
            .iter()
            .any(|p| p.message.contains("carries a severity")),
        "severity in the section"
    );

    // The key dropped although the report has the section.
    let mut without_key = original.clone();
    without_key
        .as_object_mut()
        .unwrap()
        .remove("patterns_across_services");
    entries.insert(
        "json/inspection.json".to_string(),
        serde_json::to_vec(&without_key).unwrap(),
    );
    assert!(
        patterns_section_problems(&entries, &report)
            .iter()
            .any(|p| p.message.contains("no patterns_across_services key")),
        "missing key"
    );

    // The key present although the report has none.
    let mut one_repo = report.clone();
    one_repo.patterns_across_services = None;
    entries.insert(
        "json/inspection.json".to_string(),
        serde_json::to_vec(&original).unwrap(),
    );
    assert!(
        patterns_section_problems(&entries, &one_repo)
            .iter()
            .any(|p| p.message.contains("has no such section")),
        "extra key"
    );
}

#[test]
fn the_gate_flags_a_workbook_sheet_the_report_does_not_have() {
    let (entries, report) = entries_of(package_for(two_repo_report()));
    let mut one_repo = report.clone();
    one_repo.patterns_across_services = None;
    let problems = patterns_section_problems(&entries, &one_repo);
    assert!(
        problems
            .iter()
            .any(|p| p.message.contains("Patterns across services sheet")),
        "{problems:?}"
    );
}

/// `findings.json` with the patterns cell of `repo` for the one pattern row set to clean.
fn plant_clean_cell(entries: &mut BTreeMap<String, Vec<u8>>, repo: &str) {
    let mut findings: Value = serde_json::from_slice(&entries["json/inspection.json"]).unwrap();
    let cells = findings["patterns_across_services"]["rows"][0]["cells"]
        .as_array_mut()
        .unwrap();
    let cell = cells
        .iter_mut()
        .find(|c| c["repo"] == repo)
        .expect("the row has a cell for the repo");
    *cell = json!({"repo": repo, "state": "clean", "label": "Clean"});
    entries.insert(
        "json/inspection.json".to_string(),
        serde_json::to_vec(&findings).unwrap(),
    );
}

#[test]
fn the_gate_refuses_a_patterns_cell_that_says_clean_beside_an_open_problem() {
    let (mut entries, report) = entries_of(package_for(two_repo_report()));
    assert!(
        patterns_section_problems(&entries, &report).is_empty(),
        "anti-vacuity: the package as built is consistent"
    );
    // acme/portal has an open problem under the rule in findings.json; its cell now says clean.
    plant_clean_cell(&mut entries, "acme/portal");
    let problems = patterns_section_problems(&entries, &report);
    assert!(
        problems.iter().any(|p| p.file == "json/inspection.json"
            && p.message.contains("acme/portal")
            && p.message.contains(RULE)
            && p.message.contains("clean")),
        "{problems:?}"
    );
    let expected = format!(
        "the patterns section says {RULE} is clean in acme/portal, and the package holds 1 place \
         open for that rule in acme/portal; a clean cell beside an open problem is a false \
         all-clear"
    );
    assert!(
        problems
            .iter()
            .any(|p| p.file == "json/inspection.json" && p.message == expected),
        "{problems:?}"
    );
}

#[test]
fn the_gate_refuses_the_pdf_data_saying_clean_beside_an_open_problem() {
    let (entries, report) = entries_of(package_for(two_repo_report()));
    let mut planted = report.clone();
    let section = planted.patterns_across_services.as_mut().unwrap();
    let cell = section.rows[0]
        .cells
        .iter_mut()
        .find(|c| c.repo == "acme/portal")
        .unwrap();
    cell.state = crate::scan_ledger::CellState::Clean;
    cell.label = "Clean".to_string();
    let problems = patterns_section_problems(&entries, &planted);
    assert!(
        problems
            .iter()
            .any(|p| p.message.contains("acme/portal") && p.message.contains("clean")),
        "{problems:?}"
    );
}

#[test]
fn a_package_whose_clean_cell_has_only_a_ruled_out_problem_clears_the_gate() {
    // acme/web's one problem is ruled out, acme/portal's is open: web reads clean, and that is
    // consistent, because a ruled-out problem is not open.
    let mut report = two_repo_report();
    report
        .ledger
        .record_rule("acme/web", RULE, RuleTier::Semantic, true, None, 5, 1);
    let mut web = report.findings[0].clone();
    web.repo = "acme/web".to_string();
    web.path = "src/query.rs".to_string();
    report.findings.push(web);
    let sealed = report.seal();
    let web_key = sealed
        .findings
        .iter()
        .find(|f| f.repo == "acme/web")
        .map(crate::report_export::finding_key)
        .unwrap();
    let mut dispositions = HashMap::new();
    dispositions.insert(
        web_key,
        crate::report_export::DispositionWire {
            state: "FalsePositive".to_string(),
            reason: "the value is a constant".to_string(),
            ..Default::default()
        },
    );
    let reviewed = crate::report_export::ReviewedReport::unreviewed(&sealed, &dispositions);
    let options = client_options();
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
    let (entries, report) = entries_of(package);
    let section = report.patterns_across_services.as_ref().unwrap();
    assert_eq!(section.rows.len(), 1, "anti-vacuity: the services differ");
    assert!(
        section.rows[0]
            .cells
            .iter()
            .any(|c| c.repo == "acme/web" && c.state == crate::scan_ledger::CellState::Clean),
        "anti-vacuity: web reads clean: {:?}",
        section.rows[0].cells
    );
    assert!(patterns_section_problems(&entries, &report).is_empty());
}

const RULE_B: &str = "SEC-OTHER-RULE-1";

/// The report reviewed with `dispositions`, and with the problem the ruling split out of the
/// ruled-out row confirmed too: a split-out problem needs a decision of its own to ship.
fn decided_split_out(
    sealed: &crate::onboard::SealedReport,
    mut dispositions: HashMap<String, crate::report_export::DispositionWire>,
) -> crate::report_export::ReviewedReport {
    let first = crate::report_export::ReviewedReport::unreviewed(sealed, &dispositions);
    for f in &first.report().findings {
        if f.captures
            .contains_key(crate::report_export::unfold::UNFOLDED_FROM_CAPTURE)
        {
            dispositions.insert(
                f.site_key.clone(),
                crate::report_export::DispositionWire {
                    state: "Confirmed".to_string(),
                    reason: "a real problem".to_string(),
                    ..Default::default()
                },
            );
        }
    }
    crate::report_export::ReviewedReport::unreviewed(sealed, &dispositions)
}

/// A package where the portal's only row (rule `RULE`) absorbed a rule B site in the web and the
/// reviewer ruled that row out. Rule B's cell for the web must stay open.
fn package_with_ruled_out_host_absorbing_another_rules_site(
) -> (BTreeMap<String, Vec<u8>>, AuditReportJson) {
    let mut report = two_repo_report();
    report
        .ledger
        .record_rule("acme/portal", RULE_B, RuleTier::Semantic, true, None, 5, 0);
    report
        .ledger
        .record_rule("acme/web", RULE_B, RuleTier::Semantic, true, None, 5, 1);
    report.findings[0].also_locations = vec![crate::onboard::MergedLocation::site(
        "acme/web",
        "src/query.rs",
        7,
        RULE_B,
        "query(x)",
        false,
    )];
    let sealed = report.seal();
    let host_key = crate::report_export::finding_key(&sealed.findings[0]);
    let mut dispositions = HashMap::new();
    dispositions.insert(
        host_key,
        crate::report_export::DispositionWire {
            state: "FalsePositive".to_string(),
            reason: "the value is a constant".to_string(),
            ..Default::default()
        },
    );
    let reviewed = decided_split_out(&sealed, dispositions);
    let options = client_options();
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
    entries_of(package)
}

#[test]
fn a_clean_cell_beside_a_site_a_ruled_out_row_absorbed_for_another_rule_is_refused() {
    let (mut entries, report) = package_with_ruled_out_host_absorbing_another_rules_site();
    let findings: Value = serde_json::from_slice(&entries["json/inspection.json"]).unwrap();
    assert_eq!(
        findings["findings"].as_array().unwrap().len(),
        1,
        "anti-vacuity: the ruled-out host is not a finding, and its absorbed site is: {findings}"
    );
    assert_eq!(findings["findings"][0]["rule_id"], RULE_B);
    assert_eq!(findings["findings"][0]["repo"], "acme/web");
    assert!(
        findings.get("open_beside_ruled_out").is_none(),
        "the site has a row of its own, so nothing is open beside the ruled-out row"
    );
    let section = report.patterns_across_services.as_ref().unwrap();
    assert_eq!(section.rows.len(), 1, "anti-vacuity: rule B differs");
    assert_eq!(section.rows[0].rule_id, RULE_B);
    assert_eq!(
        section.rows[0]
            .cells
            .iter()
            .find(|c| c.repo == "acme/web")
            .unwrap()
            .state,
        crate::scan_ledger::CellState::Violated { sites: 1 },
        "anti-vacuity: the absorbed site keeps the web cell open"
    );
    assert!(patterns_section_problems(&entries, &report).is_empty());
    plant_clean_cell(&mut entries, "acme/web");
    let problems = patterns_section_problems(&entries, &report);
    assert!(
        problems.iter().any(|p| p.file == "json/inspection.json"
            && p.message.contains("acme/web")
            && p.message.contains(RULE_B)
            && p.message.contains("clean")),
        "{problems:?}"
    );
}

/// A package where the portal's only row (rule `RULE`) also matches rule B and the reviewer
/// ruled that row out. The portal's rule B place stays open, so its rule B cell must not read
/// clean. The web checked rule B and is clean, so the services differ on it.
fn package_with_ruled_out_host_also_matching_another_rule(
) -> (BTreeMap<String, Vec<u8>>, AuditReportJson) {
    let mut report = two_repo_report();
    report
        .ledger
        .record_rule("acme/portal", RULE_B, RuleTier::Semantic, true, None, 5, 0);
    report
        .ledger
        .record_rule("acme/web", RULE_B, RuleTier::Semantic, true, None, 5, 0);
    report.findings[0].also_matches = vec![RULE_B.to_string()];
    let sealed = report.seal();
    let host_key = crate::report_export::finding_key(&sealed.findings[0]);
    let mut dispositions = HashMap::new();
    dispositions.insert(
        host_key,
        crate::report_export::DispositionWire {
            state: "FalsePositive".to_string(),
            reason: "the value is a constant".to_string(),
            ..Default::default()
        },
    );
    let reviewed = decided_split_out(&sealed, dispositions);
    let options = client_options();
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
    entries_of(package)
}

#[test]
fn a_clean_cell_beside_a_rule_a_ruled_out_row_also_matches_is_refused() {
    let (mut entries, report) = package_with_ruled_out_host_also_matching_another_rule();
    let findings: Value = serde_json::from_slice(&entries["json/inspection.json"]).unwrap();
    assert_eq!(
        findings["findings"].as_array().unwrap().len(),
        1,
        "anti-vacuity: the ruled-out row is not a finding, the rule it also matched is: {findings}"
    );
    assert_eq!(findings["findings"][0]["rule_id"], RULE_B);
    assert_eq!(findings["findings"][0]["path"], "src/db.rs");
    assert_eq!(findings["findings"][0]["line"], 12);
    assert!(
        findings.get("open_beside_ruled_out").is_none(),
        "the rule it also matched has a row of its own"
    );
    let section = report.patterns_across_services.as_ref().unwrap();
    assert_eq!(section.rows.len(), 1, "anti-vacuity: rule B differs");
    assert_eq!(section.rows[0].rule_id, RULE_B);
    assert!(patterns_section_problems(&entries, &report).is_empty());
    plant_clean_cell(&mut entries, "acme/portal");
    let problems = patterns_section_problems(&entries, &report);
    assert!(
        problems.iter().any(|p| p.file == "json/inspection.json"
            && p.message.contains("acme/portal")
            && p.message.contains(RULE_B)
            && p.message.contains("clean")),
        "{problems:?}"
    );
}

/// The gate's backstop: a package whose `open_beside_ruled_out` lists a place is refused, with or
/// without a clean cell beside it, because every such place should have a row of its own.
#[test]
fn the_gate_refuses_a_package_whose_open_beside_ruled_out_is_not_empty() {
    let (mut entries, report) = entries_of(package_for(two_repo_report()));
    assert!(
        patterns_section_problems(&entries, &report).is_empty(),
        "anti-vacuity: the package as built is consistent"
    );
    let mut findings: Value = serde_json::from_slice(&entries["json/inspection.json"]).unwrap();
    findings["open_beside_ruled_out"] =
        json!([{"repo": "acme/web", "rule_id": RULE_B, "path": "src/query.rs", "line": 7}]);
    entries.insert(
        "json/inspection.json".to_string(),
        serde_json::to_vec(&findings).unwrap(),
    );
    let problems = patterns_section_problems(&entries, &report);
    assert!(
        problems.iter().any(|p| p.file == "json/inspection.json"
            && p.message.contains("src/query.rs")
            && p.message.contains(RULE_B)
            && p.message.contains("ruled-out row")),
        "{problems:?}"
    );
}

#[test]
fn findings_json_omits_open_beside_ruled_out_when_there_is_nothing_to_list() {
    let (entries, _report) = entries_of(package_for(two_repo_report()));
    let findings: Value = serde_json::from_slice(&entries["json/inspection.json"]).unwrap();
    assert!(
        findings.get("findings").is_some(),
        "anti-vacuity: findings.json parsed with its findings"
    );
    assert!(
        findings.get("open_beside_ruled_out").is_none(),
        "{findings}"
    );
}

/// The package as built, with `findings.json`'s rows replaced by `rows` and the patterns row's
/// title by `title`, and the portal's cell planted clean.
fn gate_problems_with_rows(rows: Value, title: Option<&str>) -> Vec<Problem> {
    let (mut entries, report) = entries_of(package_for(two_repo_report()));
    plant_clean_cell(&mut entries, "acme/portal");
    let mut findings: Value = serde_json::from_slice(&entries["json/inspection.json"]).unwrap();
    findings["findings"] = rows;
    if let Some(title) = title {
        findings["patterns_across_services"]["rows"][0]["title"] = json!(title);
    }
    entries.insert(
        "json/inspection.json".to_string(),
        serde_json::to_vec(&findings).unwrap(),
    );
    // The PDF side is not under test: it keeps the section as built.
    let mut no_pdf_section = report.clone();
    no_pdf_section.patterns_across_services = None;
    patterns_clean_cell_problems(&entries, &no_pdf_section)
}

fn refuses_portal(problems: &[Problem]) -> bool {
    problems.iter().any(|p| {
        p.file == "json/inspection.json"
            && p.message.contains("acme/portal")
            && p.message.contains("clean")
    })
}

#[test]
fn the_gate_counts_a_row_in_a_sub_group_of_the_rule_as_open_for_the_rule() {
    let rows = json!([{"repo": "acme/portal", "rule_id": "SEC-OTHER-RULE-1",
        "group": format!("sub:{RULE}/2"), "kind": "deviation"}]);
    assert!(refuses_portal(&gate_problems_with_rows(rows, None)));
    let unrelated = json!([{"repo": "acme/portal", "rule_id": "SEC-OTHER-RULE-1",
        "group": "sub:SEC-ELSE-1/2", "kind": "deviation"}]);
    assert!(
        !refuses_portal(&gate_problems_with_rows(unrelated, None)),
        "anti-vacuity: a sub-group of another rule does not count"
    );
}

#[test]
fn the_gate_counts_a_row_a_source_was_collapsed_into_as_open_for_the_source_rule() {
    let rows = json!([{"repo": "acme/portal", "rule_id": "SEC-OTHER-RULE-1", "kind": "deviation",
        "flagged_by": [{"rule_id": RULE, "path": "src/db.rs", "line": 12}]}]);
    assert!(refuses_portal(&gate_problems_with_rows(rows, None)));
}

#[test]
fn the_gate_counts_a_row_that_also_matches_the_rule_by_title_as_open_for_it() {
    let rows = json!([{"repo": "acme/portal", "rule_id": "SEC-OTHER-RULE-1", "kind": "deviation",
        "also_matches": "Some other title, Raw SQL building"}]);
    assert!(refuses_portal(&gate_problems_with_rows(
        rows,
        Some("Raw SQL building")
    )));
}

#[test]
fn the_gate_matches_a_rule_title_that_holds_a_comma_as_one_title() {
    let rows = json!([{"repo": "acme/portal", "rule_id": "SEC-OTHER-RULE-1", "kind": "deviation",
        "also_matches": "Some other title, Queries built by joining text, not parameters"}]);
    assert!(refuses_portal(&gate_problems_with_rows(
        rows.clone(),
        Some("Queries built by joining text, not parameters")
    )));
    assert!(
        !refuses_portal(&gate_problems_with_rows(
            rows,
            Some("Queries built by joining")
        )),
        "anti-vacuity: a title the row does not list is not found"
    );
}

#[test]
fn the_gate_counts_a_site_absorbed_in_another_service_as_open_in_its_own() {
    let rows = json!([{"repo": "acme/web", "rule_id": "SEC-OTHER-RULE-1", "kind": "deviation",
        "also_locations": [{"repo": "acme/portal", "path": "src/db.rs", "line": 12,
            "rule_id": RULE, "snippet": "x", "consequence": false}]}]);
    assert!(refuses_portal(&gate_problems_with_rows(rows, None)));
}
