//! False-negative audit, Batch C, H11: an incremental run says on the cover what it re-audited
//! and what it carried forward, and a carried row says where it came from.

use std::collections::HashMap;

use super::tests::{empty_opts, finding, report_with, which_typst};
use super::*;
use crate::scan_ledger::IncrementalCarry;

fn carry() -> IncrementalCarry {
    IncrementalCarry {
        changed: 12,
        carried_files: 340,
        carried_findings: 3,
        prior_sha: Some("0123456789abcdef".to_string()),
    }
}

fn incremental_report() -> crate::onboard::ScanReport {
    let mut carried = finding("SEC-NO-HARDCODED-SECRETS-1", "src/old.rs", 4, "high");
    carried.captures.insert(
        CARRIED_FROM_CAPTURE.to_string(),
        "0123456789abcdef".to_string(),
    );
    let fresh = finding("SEC-NO-HARDCODED-SECRETS-1", "src/new.rs", 9, "high");
    let mut report = report_with(vec![carried, fresh], vec!["SEC-NO-HARDCODED-SECRETS-1"]);
    report.ledger.record_incremental(carry());
    report
}

#[test]
fn the_cover_sentence_states_both_counts_and_the_source_commit() {
    assert_eq!(
        incremental_cover_note(&carry()),
        "12 files re-audited this run. 340 files not re-audited; 3 findings carried from the \
         inspection at 0123456."
    );
    let one = IncrementalCarry {
        changed: 1,
        carried_files: 1,
        carried_findings: 1,
        prior_sha: None,
    };
    assert_eq!(
        incremental_cover_note(&one),
        "1 file re-audited this run. 1 file not re-audited; 1 finding carried from an earlier \
         inspection."
    );
}

#[test]
fn a_full_scan_has_no_incremental_note() {
    let report = report_with(
        vec![finding("SEC-NO-HARDCODED-SECRETS-1", "src/a.rs", 1, "high")],
        vec!["SEC-NO-HARDCODED-SECRETS-1"],
    );
    let json = build_report_json(&report.seal(), &HashMap::new(), None, &empty_opts());
    assert!(json.cover.incremental_note.is_none());
}

#[test]
fn the_cover_data_carries_the_note_for_an_incremental_run() {
    let json = build_report_json(
        &incremental_report().seal(),
        &HashMap::new(),
        None,
        &empty_opts(),
    );
    assert_eq!(
        json.cover.incremental_note.as_deref(),
        Some(incremental_cover_note(&carry()).as_str())
    );
}

#[test]
fn findings_json_and_the_csv_name_the_inspection_a_row_was_carried_from() {
    let report = incremental_report().seal();
    let json = build_report_json(&report, &HashMap::new(), None, &empty_opts());
    let export = crate::xlsx_export::build_findings_export(
        &report,
        &HashMap::new(),
        None,
        &json,
        &HashMap::new(),
    );
    let value = serde_json::to_value(&export).unwrap();
    let rows = value["findings"].as_array().unwrap();
    let carried = rows.iter().find(|r| r["path"] == "src/old.rs").unwrap();
    assert_eq!(carried["carried_from"], "0123456789abcdef");
    let fresh = rows.iter().find(|r| r["path"] == "src/new.rs").unwrap();
    assert!(fresh.get("carried_from").is_none(), "{fresh}");

    let csvs = crate::xlsx_export::build_csv_exports(&report, &HashMap::new(), None, &empty_opts())
        .unwrap();
    let (_, all) = csvs
        .iter()
        .find(|(n, _)| n == "csv/all-findings.csv")
        .unwrap();
    let text = String::from_utf8_lossy(all);
    assert!(text.contains("carried_from"), "{text}");
    assert!(text.contains("0123456789abcdef"), "{text}");
}

#[test]
fn the_workbook_has_a_carried_from_column() {
    let report = incremental_report().seal();
    let bytes =
        crate::xlsx_export::build_workbook(&report, &HashMap::new(), None, &empty_opts()).unwrap();
    let all = crate::release_gate::xlsx_strings(&bytes)
        .expect("a workbook")
        .join("\n");
    assert!(all.contains("Carried from"), "{all}");
    assert!(all.contains("0123456789abcdef"), "{all}");
}

#[tokio::test]
async fn the_pdf_cover_says_what_was_carried_forward() {
    if which_typst().is_none() {
        eprintln!("skipping the_pdf_cover_says_what_was_carried_forward: typst not on PATH");
        return;
    }
    let json = build_report_json(
        &incremental_report().seal(),
        &HashMap::new(),
        None,
        &empty_opts(),
    );
    let pdf = compile_pdf(&json).await.expect("the PDF compiles");
    let text = pdf_extract::extract_text_from_mem(&pdf).unwrap_or_default();
    let joined: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    assert!(joined.contains("12filesre-auditedthisrun."), "{text}");
    assert!(
        joined.contains("3findingscarriedfromtheinspectionat0123456."),
        "{text}"
    );
}
