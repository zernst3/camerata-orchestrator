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
    let findings: Value = serde_json::from_slice(&entries["findings.json"]).unwrap();
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
    let findings: Value = serde_json::from_slice(&entries["findings.json"]).unwrap();
    assert!(findings.get("patterns_across_services").is_none());
}

#[test]
fn the_gate_flags_a_pattern_row_counted_among_the_findings() {
    let (mut entries, report) = entries_of(package_for(two_repo_report()));
    let mut findings: Value = serde_json::from_slice(&entries["findings.json"]).unwrap();
    findings["findings"]
        .as_array_mut()
        .unwrap()
        .push(json!({"rule_id": RULE, "statement": "x", "cells": []}));
    entries.insert(
        "findings.json".to_string(),
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
    let original: Value = serde_json::from_slice(&entries["findings.json"]).unwrap();

    // A severity on a pattern row.
    let mut with_severity = original.clone();
    with_severity["patterns_across_services"]["rows"][0]["severity"] = json!("high");
    entries.insert(
        "findings.json".to_string(),
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
        "findings.json".to_string(),
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
        "findings.json".to_string(),
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
    let mut findings: Value = serde_json::from_slice(&entries["findings.json"]).unwrap();
    let cells = findings["patterns_across_services"]["rows"][0]["cells"]
        .as_array_mut()
        .unwrap();
    let cell = cells
        .iter_mut()
        .find(|c| c["repo"] == repo)
        .expect("the row has a cell for the repo");
    *cell = json!({"repo": repo, "state": "clean", "label": "Clean"});
    entries.insert(
        "findings.json".to_string(),
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
        problems.iter().any(|p| p.file == "findings.json"
            && p.message.contains("acme/portal")
            && p.message.contains(RULE)
            && p.message.contains("clean")),
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
