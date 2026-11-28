//! The status word (single counts, work order D). A row found by an outside tool in preview
//! never prints a baseline status; a real baseline record prints `carried-baseline`; an inline
//! waiver prints `waived-inline`. Synthetic rows only.

use std::collections::HashMap;

use super::tests::{empty_opts, finding, report_with};
use super::*;

/// A row as an outside tool in preview leaves it.
fn tool_row(legacy_status: &str) -> crate::onboard::Finding {
    let mut f = finding("SYN-TOOL-1", "src/t.rs", 3, "medium");
    f.preview = true;
    f.preview_tool = Some("semgrep".to_string());
    f.status = legacy_status.to_string();
    f
}

/// The report as an older version sealed it: the same JSON, read back through the report's own
/// reader, so the read shim is in play.
fn read_back(findings: Vec<crate::onboard::Finding>) -> crate::onboard::ScanReport {
    let report = report_with(findings, vec!["SYN-TOOL-1", "SYN-B-1"]);
    let mut value = serde_json::to_value(&report).expect("serialises");
    // Put the old words back on the wire exactly as the old producer wrote them.
    for row in value["findings"].as_array_mut().expect("rows") {
        if row["preview"] == true {
            row["status"] = serde_json::json!("suppressed-baseline");
        }
    }
    serde_json::from_value(value).expect("reads back")
}

#[test]
fn a_preview_row_never_prints_a_baseline_status() {
    let sealed = read_back(vec![
        tool_row("suppressed-baseline"),
        finding("SYN-B-1", "src/b.rs", 9, "high"),
    ])
    .seal();
    let reviewed = ReviewedReport::unreviewed(&sealed, &HashMap::new());
    let opts = empty_opts();

    // json/inspection.json
    let json = build_report_json(&reviewed, None, &opts);
    let fe = crate::xlsx_export::build_findings_export(&reviewed, None, &json, &HashMap::new());
    let findings_json = serde_json::to_string(&fe.findings).expect("json");
    assert!(
        findings_json.contains("SYN-TOOL-1"),
        "anti-vacuity: the tool row is in json/inspection.json"
    );
    assert!(
        !findings_json.contains("baseline"),
        "json/inspection.json prints no baseline word: {findings_json}"
    );
    assert!(findings_json.contains("\"status\":\"active\""));

    // the CSV datasets
    let csvs = crate::xlsx_export::build_csv_exports(&reviewed, None, &opts).expect("csv");
    let all = csvs
        .iter()
        .find(|(n, _)| n == "csv/all-findings.csv")
        .expect("all-findings.csv");
    let text = String::from_utf8(all.1.clone()).expect("utf8");
    assert!(
        text.contains("SYN-TOOL-1"),
        "anti-vacuity: the row is in the CSV"
    );
    assert!(
        !text.contains("baseline"),
        "the CSV prints no baseline word: {text}"
    );

    // the workbook Status column
    let xlsx = crate::xlsx_export::build_workbook(&reviewed, None, &opts).expect("workbook");
    let grid = crate::release_gate::xlsx_sheet_grid(&xlsx, "All problems").expect("sheet");
    let header = grid
        .iter()
        .find(|r| r.iter().any(|c| c == "Status"))
        .expect("a header row with Status");
    let col = header.iter().position(|c| c == "Status").expect("column");
    let statuses: Vec<&str> = grid
        .iter()
        .skip_while(|r| *r != header)
        .skip(1)
        .filter_map(|r| r.get(col).map(String::as_str))
        .filter(|s| !s.is_empty())
        .collect();
    assert!(
        !statuses.is_empty(),
        "anti-vacuity: the Status column has rows"
    );
    assert!(
        statuses.iter().all(|s| !s.contains("baseline")),
        "the Status column prints no baseline word: {statuses:?}"
    );
}

#[test]
fn a_tool_in_preview_writes_status_active_and_says_it_is_a_preview_row() {
    let note = crate::scan_tools::note_finding("owner/repo", "ruff", "could not run");
    assert_eq!(note.status, "active");
    assert!(
        note.preview,
        "the separate field says it came from a tool in preview"
    );
}

#[test]
fn a_real_baseline_record_and_a_waiver_print_their_own_words() {
    let mut carried = finding("SYN-B-1", "src/b.rs", 9, "high");
    carried.status = "suppressed-baseline".to_string();
    let mut waived = finding("SYN-TOOL-1", "src/t.rs", 3, "medium");
    waived.status = "suppressed-inline".to_string();
    let report = report_with(vec![carried, waived], vec!["SYN-TOOL-1", "SYN-B-1"]);
    let value = serde_json::to_value(&report).expect("serialises");
    let back: crate::onboard::ScanReport = serde_json::from_value(value).expect("reads back");
    let words: Vec<&str> = back.findings.iter().map(|f| f.status.as_str()).collect();
    assert_eq!(words, ["carried-baseline", "waived-inline"]);
}

#[test]
fn every_folded_row_names_its_head() {
    let mut a = tool_row("active");
    a.rule_id = "SYN-TOOL-1".to_string();
    let mut b = tool_row("active");
    b.rule_id = "SYN-TOOL-2".to_string();
    let sealed = report_with(vec![a, b], vec!["SYN-TOOL-1", "SYN-TOOL-2"]).seal();
    let reviewed = ReviewedReport::unreviewed(&sealed, &HashMap::new());
    let json = build_report_json(&reviewed, None, &empty_opts());
    let fe = crate::xlsx_export::build_findings_export(&reviewed, None, &json, &HashMap::new());
    let rows = serde_json::to_value(&fe.findings).expect("json");
    let rows = rows.as_array().expect("rows");
    assert_eq!(rows.len(), 2, "anti-vacuity: two rows at one place");
    let carried: Vec<&str> = rows
        .iter()
        .map(|r| r["carried_by"].as_str().unwrap_or_default())
        .filter(|c| !c.is_empty())
        .collect();
    assert_eq!(carried.len(), 1, "one head and one folded row: {rows:?}");
}

#[test]
fn the_package_says_how_many_client_waivers_and_baseline_entries_it_did_not_honour() {
    let mut report = report_with(
        vec![finding("SYN-B-1", "src/b.rs", 9, "high")],
        vec!["SYN-B-1"],
    );
    report.ledger.add_client_suppressions(3);
    let sealed = report.seal();
    let reviewed = ReviewedReport::unreviewed(&sealed, &HashMap::new());
    let json = build_report_json(&reviewed, None, &empty_opts());
    assert_eq!(json.counts.other.client_suppressions_found, 3);
    let line = serde_json::to_value(&json.methodology).expect("json")["reconciliation"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        line.contains("3 waivers and baseline entries") && line.contains("did not honour them"),
        "{line}"
    );
}
