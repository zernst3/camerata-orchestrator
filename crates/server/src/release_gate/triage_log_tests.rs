//! The triage log is the operator's file: the release gate refuses a package that carries it, and
//! a package built with every option on never does. Built through the production assembly around
//! a stand-in PDF, like the gate's own tests.

use std::collections::HashMap;

use super::tests::scan_report;
use super::*;
use crate::report_export::{build_report_json, render_data_json};
use crate::xlsx_export::triage_log::{TRIAGE_LOG_CSV, TRIAGE_LOG_SHEET};

const STEM: &str = "acme-inspection";
const STAND_IN_PDF: &[u8] = b"%PDF-1.7 stand-in for the gate tests";

/// The reviewer's decisions on every row of the seeded report. A package is only built from
/// reviewed rows (criticals and rows the engine is unsure of are held until decided), so the
/// fixture carries a decision for each.
fn confirmed(
    report: &crate::onboard::ScanReport,
) -> HashMap<String, crate::report_export::DispositionWire> {
    report
        .findings
        .iter()
        .map(|f| {
            (
                crate::report_export::finding_key(f),
                crate::report_export::DispositionWire {
                    state: "Confirmed".to_string(),
                    ..Default::default()
                },
            )
        })
        .collect()
}

fn package_with(options: &ReportOptions) -> ProductPackage {
    let report = scan_report();
    let decisions = confirmed(&report);
    let sealed = report.seal();
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&sealed, &decisions),
        None,
        options,
    );
    let data = render_data_json(&json).expect("data.json renders");
    assemble_package(
        &crate::report_export::ReviewedReport::unreviewed(&sealed, &decisions),
        None,
        options,
        STEM,
        (json, data, STAND_IN_PDF.to_vec()),
    )
    .expect("the package assembles")
}

/// A client engagement: the confidentiality marking names the client, so the gate's marking
/// check (every PDF page and workbook sheet) has a real line to find.
fn base_options() -> ReportOptions {
    ReportOptions {
        client_name: "Example Client Ltd".to_string(),
        ..ReportOptions::default()
    }
}

/// Every option a client export can be given, the old `triage_log` key a stale client may still
/// send included.
fn every_option_on() -> ReportOptions {
    let mut options: ReportOptions = serde_json::from_str(
        r#"{"triage_log": true, "project_title": "Acme", "prepared_by": "A. Person",
            "executive_summary_override": "A summary.", "brand": "Acme Co"}"#,
    )
    .expect("a stale client's body still parses");
    options.client_name = "Example Client Ltd".to_string();
    options
}

#[test]
fn a_package_built_with_every_option_on_has_no_triage_log_and_clears_the_gate() {
    let package = package_with(&every_option_on());
    assert!(
        package.datasets.iter().all(|(p, _)| p != TRIAGE_LOG_CSV),
        "no csv/triage-log.csv in the package"
    );
    let released = release(package).expect("the package clears the gate");
    let entries = read_zip_entries(&released.bytes).expect("a zip");
    assert!(!entries.contains_key(TRIAGE_LOG_CSV));
    let manifest: serde_json::Value =
        serde_json::from_slice(&entries["manifest.json"]).expect("manifest parses");
    let listed = manifest["files"]
        .as_array()
        .expect("files")
        .iter()
        .any(|f| f["path"] == TRIAGE_LOG_CSV);
    assert!(
        !listed,
        "the manifest does not list the triage log: {manifest}"
    );
    let readme = String::from_utf8_lossy(&entries["README.txt"]).to_string();
    assert!(
        !readme.to_lowercase().contains("triage log"),
        "the README does not name it: {readme}"
    );
}

#[test]
fn the_gate_refuses_a_package_that_carries_the_triage_log_csv() {
    let mut package = package_with(&base_options());
    package.datasets.push((
        TRIAGE_LOG_CSV.to_string(),
        b"Rule ID,Rule\nAI-1,x\n".to_vec(),
    ));
    let failure = match release(package) {
        Err(ReleaseError::Blocked(failure)) => failure,
        Err(other) => panic!("expected the gate to block, got {other}"),
        Ok(_) => panic!("a package with the triage log must be refused"),
    };
    let hit = failure
        .report
        .problems()
        .any(|(check, problem)| check == CheckId::DatasetSet && problem.file == TRIAGE_LOG_CSV);
    assert!(hit, "{failure}");
}

#[test]
fn the_gate_refuses_a_workbook_with_a_triage_log_sheet() {
    let mut wb = rust_xlsxwriter::Workbook::new();
    wb.add_worksheet()
        .set_name(TRIAGE_LOG_SHEET)
        .expect("sheet name");
    let xlsx = wb.save_to_buffer().expect("a workbook");
    let mut entries: std::collections::BTreeMap<String, Vec<u8>> =
        std::collections::BTreeMap::new();
    entries.insert(format!("{STEM}.pdf"), STAND_IN_PDF.to_vec());
    entries.insert(format!("{STEM}-findings.xlsx"), xlsx);
    let problems = consistency::dataset_set_problems(&entries);
    assert!(
        problems
            .iter()
            .any(|p| p.file == format!("{STEM}-findings.xlsx")
                && p.message.contains("Triage Log sheet")),
        "{problems:?}"
    );
}

#[test]
fn any_other_unexpected_csv_is_still_refused() {
    let mut package = package_with(&ReportOptions::default());
    package
        .datasets
        .push(("csv/surprise.csv".to_string(), b"a\n1\n".to_vec()));
    let failure = match release(package) {
        Err(ReleaseError::Blocked(failure)) => failure,
        Err(other) => panic!("expected the gate to block, got {other}"),
        Ok(_) => panic!("an unexpected member must block the package"),
    };
    assert!(
        failure
            .report
            .problems()
            .any(|(check, problem)| check == CheckId::DatasetSet
                && problem.file == "csv/surprise.csv")
    );
}
