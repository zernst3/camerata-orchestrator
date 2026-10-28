//! H4 item 1: every failed AI pass and every skipped file reaches Coverage in the PDF data, the
//! workbook and `findings.json`, from one computation.

use std::collections::HashMap;

use super::coverage_gaps::coverage_gaps;
use super::tests::{empty_opts, finding, report_with, which_typst};
use super::*;
use crate::ai_audit::FailedPass;
use crate::scan_ledger::{ScanInputFacts, SkipReason, SkippedFile};

fn report_with_gaps() -> crate::onboard::ScanReport {
    let mut report = report_with(
        vec![finding(
            "SEC-NO-HARDCODED-SECRETS-1",
            "src/a.rs",
            10,
            "high",
        )],
        vec!["SEC-NO-HARDCODED-SECRETS-1"],
    );
    report.failed_passes = vec![
        FailedPass {
            repo: "owner/repo".to_string(),
            pass: "audit pass[all] 2/3 · rules 1/1".to_string(),
            reason: "the model's reply was not valid JSON after 3 attempts".to_string(),
            files_not_reviewed: 12,
        },
        // Not a pass over files: disclosed in the methodology, not as a Coverage file gap.
        FailedPass {
            repo: "owner/repo".to_string(),
            pass: "rule-alternative recommendations".to_string(),
            reason: "timed out".to_string(),
            files_not_reviewed: 0,
        },
    ];
    report.ledger.record_input(ScanInputFacts {
        skipped_files: vec![
            SkippedFile {
                repo: "owner/repo".to_string(),
                path: "data/huge.json".to_string(),
                reason: SkipReason::Oversize,
            },
            SkippedFile {
                repo: "owner/repo".to_string(),
                path: "src/z/last.rs".to_string(),
                reason: SkipReason::BeyondFileCap,
            },
        ],
        ..ScanInputFacts::default()
    });
    report
}

#[test]
fn coverage_gaps_list_failed_ai_passes_and_skipped_files_by_path() {
    let gaps = coverage_gaps(&report_with_gaps());
    assert_eq!(
        gaps.failed_ai_passes,
        ["owner/repo, audit pass[all] 2/3 · rules 1/1: pass failed, 12 files not reviewed by the AI pass"]
    );
    let listed: Vec<(&str, SkipReason)> = gaps
        .skipped_files
        .iter()
        .map(|s| (s.path.as_str(), s.reason_code))
        .collect();
    assert_eq!(
        listed,
        [
            ("data/huge.json", SkipReason::Oversize),
            ("src/z/last.rs", SkipReason::BeyondFileCap)
        ]
    );
    assert_eq!(
        gaps.skipped_files[0].reason,
        "over the 400 KB file size limit"
    );
    assert_eq!(gaps.skipped_files[1].reason, "beyond the 20,000-file limit");
}

#[test]
fn a_scan_that_covered_everything_has_no_gaps() {
    let report = report_with(Vec::new(), Vec::new());
    assert!(coverage_gaps(&report).is_empty());
}

#[test]
fn the_pdf_data_carries_the_coverage_gaps() {
    let report = report_with_gaps();
    let json = build_report_json(&report.clone().seal(), &HashMap::new(), None, &empty_opts());
    assert_eq!(json.coverage_gaps, coverage_gaps(&report));
    let data = render_data_json(&json).expect("renders");
    assert!(data.contains("data/huge.json"), "{data}");
    assert!(data.contains("12 files not reviewed by the AI pass"));
}

#[test]
fn findings_json_carries_the_coverage_gaps() {
    let report = report_with_gaps().seal();
    let json = build_report_json(&report, &HashMap::new(), None, &empty_opts());
    let export = crate::xlsx_export::build_findings_export(
        &report,
        &HashMap::new(),
        None,
        &json,
        &HashMap::new(),
    );
    let value = serde_json::to_value(&export).unwrap();
    let coverage = &value["coverage"];
    assert_eq!(
        coverage["failed_ai_passes"][0],
        "owner/repo, audit pass[all] 2/3 · rules 1/1: pass failed, 12 files not reviewed by the AI pass"
    );
    assert_eq!(coverage["skipped_files"][0]["path"], "data/huge.json");
    assert_eq!(coverage["skipped_files"][0]["reason_code"], "oversize");
    assert_eq!(coverage["skipped_files"][1]["path"], "src/z/last.rs");
    assert_eq!(
        coverage["skipped_files"][1]["reason_code"],
        "beyond_file_cap"
    );
}

#[test]
fn the_workbook_coverage_sheet_lists_the_gaps() {
    let report = report_with_gaps().seal();
    let bytes =
        crate::xlsx_export::build_workbook(&report, &HashMap::new(), None, &empty_opts()).unwrap();
    let strings = crate::release_gate::xlsx_strings(&bytes).expect("a workbook");
    let all = strings.join("\n");
    assert!(all.contains("Not reviewed this scan"), "{all}");
    assert!(all.contains("data/huge.json"));
    assert!(all.contains("src/z/last.rs"));
    assert!(all.contains("beyond the 20,000-file limit"));
    assert!(all.contains("12 files not reviewed by the AI pass"));
}

#[tokio::test]
async fn the_pdf_renders_the_coverage_gaps() {
    if which_typst().is_none() {
        eprintln!("skipping the_pdf_renders_the_coverage_gaps: typst not on PATH");
        return;
    }
    let report = report_with_gaps();
    let json = build_report_json(&report.clone().seal(), &HashMap::new(), None, &empty_opts());
    let pdf = compile_pdf(&json)
        .await
        .expect("the PDF compiles with coverage gaps");
    let text = pdf_extract::extract_text_from_mem(&pdf).unwrap_or_default();
    assert!(text.contains("data/huge.json"), "{text}");
    assert!(text.contains("files not reviewed by the AI pass"), "{text}");
}
