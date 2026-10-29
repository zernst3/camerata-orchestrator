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
            class: crate::ai_audit::FailureClass::Content,
            files: Vec::new(),
            attempts: Vec::new(),
        },
        // Not a pass over files: disclosed in the methodology, not as a Coverage file gap.
        FailedPass {
            repo: "owner/repo".to_string(),
            pass: "rule-alternative recommendations".to_string(),
            reason: "timed out".to_string(),
            files_not_reviewed: 0,
            class: crate::ai_audit::FailureClass::Content,
            files: Vec::new(),
            attempts: Vec::new(),
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

// ── The skipped-file list is bounded; the rest is counted and every renderer says so ──────────

/// A synthetic large scan: 2,500 oversize and 1,200 past-the-cap files, fed through the same
/// collector the scan uses. No file exists on disk.
fn report_with_a_huge_skip_list() -> crate::onboard::ScanReport {
    let mut report = report_with(
        vec![finding(
            "SEC-NO-HARDCODED-SECRETS-1",
            "src/a.rs",
            10,
            "high",
        )],
        vec!["SEC-NO-HARDCODED-SECRETS-1"],
    );
    let mut list = crate::scan_ledger::SkippedFileList::default();
    for i in 0..2_500 {
        list.push(
            "owner/repo",
            format!("big/{i:05}.json"),
            SkipReason::Oversize,
        );
    }
    for i in 0..1_200 {
        list.push(
            "owner/repo",
            format!("deep/{i:05}.rs"),
            SkipReason::BeyondFileCap,
        );
    }
    let (skipped_files, skipped_beyond_cap_total) = list.finish();
    report.ledger.record_input(ScanInputFacts {
        skipped_files,
        skipped_beyond_cap_total,
        ..ScanInputFacts::default()
    });
    report
}

#[test]
fn the_collector_names_the_first_thousand_paths_per_reason_and_counts_the_rest() {
    let report = report_with_a_huge_skip_list();
    let input = report.ledger.input();
    let oversize = input
        .skipped_files
        .iter()
        .filter(|s| s.reason == SkipReason::Oversize)
        .count();
    let capped = input
        .skipped_files
        .iter()
        .filter(|s| s.reason == SkipReason::BeyondFileCap)
        .count();
    assert_eq!((oversize, capped), (1_000, 1_000));
    assert_eq!(input.skipped_beyond_cap_total, 1_500 + 200);
    // The FIRST paths in walk order are the ones named.
    assert_eq!(input.skipped_files[0].path, "big/00000.json");
    assert!(input
        .skipped_files
        .iter()
        .any(|s| s.path == "big/00999.json"));
    assert!(!input
        .skipped_files
        .iter()
        .any(|s| s.path == "big/01000.json"));
    // Nothing is lost silently: named plus counted is the full total.
    assert_eq!(
        input.skipped_files.len() + input.skipped_beyond_cap_total,
        3_700
    );
}

#[test]
fn a_complete_list_has_no_more_line() {
    let gaps = coverage_gaps(&report_with_gaps());
    assert_eq!(gaps.skipped_beyond_cap_total, 0);
    assert!(gaps.skipped_beyond_cap_note.is_none());
}

#[test]
fn findings_json_and_the_pdf_data_carry_the_and_k_more_count() {
    let report = report_with_a_huge_skip_list().seal();
    let json = build_report_json(&report, &HashMap::new(), None, &empty_opts());
    assert_eq!(json.coverage_gaps.skipped_files.len(), 2_000);
    assert_eq!(json.coverage_gaps.skipped_beyond_cap_total, 1_700);
    let export = crate::xlsx_export::build_findings_export(
        &report,
        &HashMap::new(),
        None,
        &json,
        &HashMap::new(),
    );
    let value = serde_json::to_value(&export).unwrap();
    assert_eq!(value["coverage"]["skipped_beyond_cap_total"], 1_700);
    assert!(value["coverage"]["skipped_beyond_cap_note"]
        .as_str()
        .unwrap()
        .starts_with("and 1700 more skipped files, not listed"));
    let data = render_data_json(&json).expect("renders");
    assert!(data.contains("and 1700 more skipped files, not listed"));
}

#[test]
fn the_workbook_coverage_sheet_says_and_k_more() {
    let report = report_with_a_huge_skip_list().seal();
    let bytes =
        crate::xlsx_export::build_workbook(&report, &HashMap::new(), None, &empty_opts()).unwrap();
    let strings = crate::release_gate::xlsx_strings(&bytes).expect("a workbook");
    let all = strings.join("\n");
    assert!(all.contains("big/00999.json"));
    assert!(!all.contains("big/01000.json"), "the list is bounded");
    assert!(all.contains("and 1700 more skipped files, not listed"));
}

#[tokio::test]
async fn the_pdf_says_and_k_more() {
    if which_typst().is_none() {
        eprintln!("skipping the_pdf_says_and_k_more: typst not on PATH");
        return;
    }
    let report = report_with_a_huge_skip_list();
    let json = build_report_json(&report.clone().seal(), &HashMap::new(), None, &empty_opts());
    let pdf = compile_pdf(&json).await.expect("the PDF compiles");
    let text = pdf_extract::extract_text_from_mem(&pdf).unwrap_or_default();
    // The PDF names the first 100, then points to the sheet for the rest it holds.
    assert!(text.contains("big/00000.json"), "{text}");
    assert!(!text.contains("big/00100.json"), "the PDF list is bounded");
    assert!(
        text.contains("more skipped files, listed in full"),
        "{text}"
    );
    assert!(text.contains("and 1700 more skipped files"), "{text}");
}
