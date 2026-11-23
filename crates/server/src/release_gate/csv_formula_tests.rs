//! V1 item 33 finding 39: a cell a spreadsheet would run as a formula never ships. Every CSV the
//! package writes carries the OWASP apostrophe in front of such a cell, the workbook keeps the
//! same text as a string, and the gate refuses a CSV that still has one, naming file and row.

use super::published_example_tests::exported;
use super::tests::{package, plant, Surface};
use super::*;
use crate::onboard::Finding;

const FORMULA: &str = "=cmd|' /C calc'!A0";

fn finding_with_formulas() -> Finding {
    Finding {
        repo: "acme/portal".to_string(),
        path: "src/deploy.yml".to_string(),
        line: 4,
        rule_id: "SEC-NO-HARDCODED-SECRETS-1".to_string(),
        severity: "low".to_string(),
        snippet: FORMULA.to_string(),
        detail: "@SUM(1+1) in a workflow".to_string(),
        ..Finding::default()
    }
}

#[test]
fn every_csv_the_package_writes_neutralizes_a_formula_cell() {
    let pkg = exported(vec![finding_with_formulas()]);
    let mut csv_files = 0;
    let mut neutralized_snippet = false;
    for (path, bytes) in pkg.datasets.iter().filter(|(p, _)| p.ends_with(".csv")) {
        csv_files += 1;
        let mut reader = csv::ReaderBuilder::new()
            .flexible(true)
            .has_headers(false)
            .from_reader(bytes.as_slice());
        for (row, record) in reader.records().enumerate() {
            let record = record.expect("a CSV row");
            for cell in record.iter() {
                assert!(
                    !crate::csv_cell::starts_a_formula(cell),
                    "{path} row {row} has an unneutralized formula cell"
                );
                neutralized_snippet |= cell == format!("'{FORMULA}");
            }
        }
    }
    assert!(csv_files >= 8, "only {csv_files} CSV files");
    assert!(neutralized_snippet, "the planted snippet is in no CSV");
    // The tickets file is one of them.
    assert!(pkg.datasets.iter().any(|(p, _)| p == "csv/tickets.csv"));
    // The package, neutralized, clears the gate.
    release(pkg).expect("the neutralized package must clear the gate");
}

#[test]
fn the_workbook_keeps_a_formula_text_as_a_string_never_a_formula() {
    let pkg = exported(vec![finding_with_formulas()]);
    let names = archive::xlsx_sheet_names(&pkg.xlsx).expect("sheets");
    for i in 1..=names.len() {
        let xml = rewrite_read(&pkg.xlsx, &format!("xl/worksheets/sheet{i}.xml"));
        assert!(
            !xml.contains("<f>") && !xml.contains("<f "),
            "sheet{i} has a formula"
        );
    }
    let cells = archive::xlsx_cells(&pkg.xlsx).expect("cells");
    assert!(
        cells.iter().any(|c| c.text.contains("cmd|")),
        "the snippet is not in the workbook as text"
    );
}

fn rewrite_read(xlsx: &[u8], part: &str) -> String {
    use std::io::Read;
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(xlsx)).expect("zip");
    let mut text = String::new();
    archive
        .by_name(part)
        .expect("part")
        .read_to_string(&mut text)
        .expect("utf8");
    text
}

#[test]
fn the_gate_refuses_a_csv_with_an_unneutralized_formula_cell_naming_file_and_row() {
    let mut pkg = package();
    plant(&mut pkg, Surface::DatasetCsv, FORMULA);
    let failure = match release(pkg) {
        Err(ReleaseError::Blocked(failure)) => failure,
        Err(other) => panic!("expected the gate to block, got {other}"),
        Ok(_) => panic!("the package was released"),
    };
    let problem = failure
        .report
        .checks
        .iter()
        .flat_map(|o| o.problems.iter())
        .find(|p| p.message.contains("runs as a formula"))
        .expect("a formula problem");
    assert_eq!(problem.file, "csv/conventions.csv");
    assert_eq!(problem.location, "row 1, column 1");
    let shown = format!("{failure}");
    assert!(!shown.contains("cmd|"), "the cell text was printed");
}

#[test]
fn the_triage_log_and_the_tech_debt_csv_neutralize_too() {
    let row = crate::xlsx_export::triage_log::TriageLogRow {
        rule_id: "R-1".to_string(),
        rule: "-rule".to_string(),
        proposed: 1,
        kept: 1,
        held_for_review: 0,
        information_only: 0,
        thrown_out: 0,
        why: FORMULA.to_string(),
    };
    let csv = crate::xlsx_export::triage_log::triage_log_csv(&[row]).expect("csv");
    let text = String::from_utf8_lossy(&csv);
    assert!(
        text.contains("'-rule") && text.contains(&format!("'{FORMULA}")),
        "{text}"
    );
    let debt = crate::onboard::tech_debt_csv(&[finding_with_formulas()]);
    assert!(debt.contains("'@SUM(1+1)"), "{debt}");
}
