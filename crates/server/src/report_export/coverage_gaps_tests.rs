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
            scope_note: false,
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
            scope_note: false,
        },
    ];
    report.ledger.record_input(ScanInputFacts {
        skipped_files: vec![
            SkippedFile {
                repo: "owner/repo".to_string(),
                path: "data/huge.json".to_string(),
                reason: SkipReason::Oversize,
                bytes: Some(612_000),
            },
            SkippedFile {
                repo: "owner/repo".to_string(),
                path: "src/z/last.rs".to_string(),
                reason: SkipReason::BeyondFileCap,
                bytes: None,
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
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );
    assert_eq!(json.coverage_gaps, coverage_gaps(&report));
    let data = render_data_json(&json).expect("renders");
    assert!(data.contains("data/huge.json"), "{data}");
    assert!(data.contains("12 files not reviewed by the AI pass"));
}

#[test]
fn findings_json_carries_the_coverage_gaps() {
    let report = report_with_gaps().seal();
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report, &HashMap::new()),
        None,
        &empty_opts(),
    );
    let export = crate::xlsx_export::build_findings_export(
        &crate::report_export::ReviewedReport::unreviewed(&report, &HashMap::new()),
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
    let bytes = crate::xlsx_export::build_workbook(
        &crate::report_export::ReviewedReport::unreviewed(&report, &HashMap::new()),
        None,
        &empty_opts(),
    )
    .unwrap();
    let strings = crate::release_gate::xlsx_strings(&bytes).expect("a workbook");
    let all = strings.join("\n");
    assert!(all.contains("File not read"), "{all}");
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
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );
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
    let skipped_totals = list.totals();
    let (skipped_files, skipped_beyond_cap_total) = list.finish();
    report.ledger.record_input(ScanInputFacts {
        skipped_files,
        skipped_beyond_cap_total,
        skipped_totals,
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
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report, &HashMap::new()),
        None,
        &empty_opts(),
    );
    assert_eq!(json.coverage_gaps.skipped_files.len(), 2_000);
    assert_eq!(json.coverage_gaps.skipped_beyond_cap_total, 1_700);
    let export = crate::xlsx_export::build_findings_export(
        &crate::report_export::ReviewedReport::unreviewed(&report, &HashMap::new()),
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
    let bytes = crate::xlsx_export::build_workbook(
        &crate::report_export::ReviewedReport::unreviewed(&report, &HashMap::new()),
        None,
        &empty_opts(),
    )
    .unwrap();
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
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );
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

// ── M5 item 3: no file is skipped silently: its name, size and reason, and the totals ─────────

use crate::onboard::files::{read_local_repo_files, read_local_repo_files_capped, MAX_FILE_BYTES};
use crate::scan_ledger::{RepoSkips, SkippedFileList};

/// A report whose ledger holds exactly what the real walk of `root` skipped, recorded the way the
/// scan records it (`SkippedFileList::record_repo`).
fn report_from_walk(extracted: crate::onboard::files::ExtractedRepo) -> crate::onboard::ScanReport {
    let mut report = report_with(
        vec![finding(
            "SEC-NO-HARDCODED-SECRETS-1",
            "src/a.rs",
            10,
            "high",
        )],
        vec!["SEC-NO-HARDCODED-SECRETS-1"],
    );
    let mut list = SkippedFileList::default();
    list.record_repo(
        "owner/repo",
        RepoSkips {
            oversize: extracted.skipped_oversize,
            beyond_cap: extracted.skipped_beyond_cap,
            not_text: extracted.skipped_unreadable,
            sizes: &extracted.skipped_sizes,
            file_cap: extracted.file_cap,
        },
    );
    let totals = list.totals();
    let file_cap_hits = list.file_cap_hits();
    let (skipped_files, skipped_beyond_cap_total) = list.finish();
    report.ledger.record_input(ScanInputFacts {
        skipped_files,
        skipped_beyond_cap_total,
        skipped_totals: totals,
        file_cap_hits,
        ..ScanInputFacts::default()
    });
    report
}

fn write_tree(root: &std::path::Path, files: &[(&str, usize)]) {
    for (rel, size) in files {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "x".repeat(*size)).unwrap();
    }
}

#[test]
fn a_file_over_max_file_bytes_appears_in_coverage_with_its_size_and_reason() {
    let dir = tempfile::tempdir().unwrap();
    write_tree(
        dir.path(),
        &[
            ("src/small.rs", 20),
            ("src/huge.rs", MAX_FILE_BYTES + 1_000),
        ],
    );
    let extracted = read_local_repo_files(dir.path()).unwrap();
    assert_eq!(extracted.skipped_oversize, ["src/huge.rs"]);

    let gaps = coverage_gaps(&report_from_walk(extracted));
    assert_eq!(gaps.skipped_files.len(), 1);
    let row = &gaps.skipped_files[0];
    assert_eq!(row.path, "src/huge.rs");
    assert_eq!(row.reason_code, SkipReason::Oversize);
    assert_eq!(row.reason, "over the 400 KB file size limit");
    assert_eq!(row.size_bytes, Some((MAX_FILE_BYTES + 1_000) as u64));
    assert_eq!(row.size, "401 KB");
    // The count is in the Coverage summary.
    assert_eq!(
        gaps.skipped_summary.as_deref(),
        Some("1 file was not scanned: 1 over the 400 KB file size limit.")
    );
    assert!(gaps.file_limit_notes.is_empty());
}

#[test]
fn a_repo_over_a_lowered_file_cap_discloses_the_unscanned_count() {
    let dir = tempfile::tempdir().unwrap();
    let files: Vec<(String, usize)> = (0..7).map(|i| (format!("src/f{i}.rs"), 10)).collect();
    let refs: Vec<(&str, usize)> = files.iter().map(|(p, n)| (p.as_str(), *n)).collect();
    write_tree(dir.path(), &refs);
    let extracted = read_local_repo_files_capped(dir.path(), 3).unwrap();
    assert!(extracted.truncated);
    assert_eq!(extracted.files.len(), 3);

    let gaps = coverage_gaps(&report_from_walk(extracted));
    // The limit is stated for the run, with the number of files not scanned. It is the limit
    // that was applied (3 here), not the production one.
    assert_eq!(
        gaps.file_limit_notes.len(),
        1,
        "{:?}",
        gaps.file_limit_notes
    );
    assert_eq!(
        gaps.file_limit_notes[0],
        "owner/repo reached the 3-file limit, so the scan read the first 3 files in path order. \
         4 files were not scanned."
    );
    // And each of them is named, with its size and the same limit as its reason.
    assert_eq!(gaps.skipped_files.len(), 4);
    assert!(gaps.skipped_files.iter().all(|s| {
        s.reason_code == SkipReason::BeyondFileCap
            && s.size == "10 B"
            && s.reason == "beyond the 3-file limit"
    }));
    assert_eq!(
        gaps.skipped_summary.as_deref(),
        Some("4 files were not scanned: 4 beyond the 3-file limit.")
    );
}

#[test]
fn the_totals_count_every_skipped_file_including_those_past_the_named_list() {
    let gaps = coverage_gaps(&report_with_a_huge_skip_list());
    // 2,500 oversize and 1,200 past the cap: only 1,000 of each are named.
    assert_eq!(
        gaps.skipped_summary.as_deref(),
        Some(
            "3700 files were not scanned: 2500 over the 400 KB file size limit, 1200 beyond the \
             20,000-file limit."
        )
    );
}

#[test]
fn each_renderer_carries_the_size_the_summary_and_the_file_limit() {
    let dir = tempfile::tempdir().unwrap();
    write_tree(
        dir.path(),
        &[
            ("a.rs", 10),
            ("b.rs", 10),
            ("c.rs", 10),
            ("big.json", MAX_FILE_BYTES + 2_000),
        ],
    );
    let extracted = read_local_repo_files_capped(dir.path(), 2).unwrap();
    let report = report_from_walk(extracted);
    let sealed = report.seal();
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&sealed, &HashMap::new()),
        None,
        &empty_opts(),
    );

    // The PDF's data.
    let data = render_data_json(&json).expect("renders");
    assert!(data.contains("402 KB"), "{data}");
    assert!(data.contains("files were not scanned"), "{data}");
    assert!(data.contains("reached the 2-file limit"), "{data}");

    // findings.json.
    let export = crate::xlsx_export::build_findings_export(
        &crate::report_export::ReviewedReport::unreviewed(&sealed, &HashMap::new()),
        None,
        &json,
        &HashMap::new(),
    );
    let value = serde_json::to_value(&export).unwrap();
    assert_eq!(
        value["coverage"]["skipped_files"][0]["size_bytes"],
        (MAX_FILE_BYTES + 2_000) as u64
    );
    assert!(value["coverage"]["skipped_summary"].is_string());
    assert_eq!(
        value["coverage"]["file_limit_notes"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    // The workbook's Coverage sheet.
    let bytes = crate::xlsx_export::build_workbook(
        &crate::report_export::ReviewedReport::unreviewed(&sealed, &HashMap::new()),
        None,
        &empty_opts(),
    )
    .unwrap();
    let all = crate::release_gate::xlsx_strings(&bytes)
        .expect("a workbook")
        .join("\n");
    assert!(all.contains("402 KB"), "{all}");
    assert!(all.contains("files were not scanned"), "{all}");
    assert!(all.contains("reached the 2-file limit"), "{all}");
}

#[tokio::test]
async fn the_pdf_prints_the_size_and_the_file_limit() {
    if which_typst().is_none() {
        eprintln!("skipping the_pdf_prints_the_size_and_the_file_limit: typst not on PATH");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    write_tree(
        dir.path(),
        &[
            ("a.rs", 10),
            ("b.rs", 10),
            ("big.json", MAX_FILE_BYTES + 2_000),
        ],
    );
    let report = report_from_walk(read_local_repo_files_capped(dir.path(), 1).unwrap());
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );
    let pdf = compile_pdf(&json).await.expect("the PDF compiles");
    let text = pdf_extract::extract_text_from_mem(&pdf).unwrap_or_default();
    assert!(text.contains("402 KB"), "{text}");
    assert!(text.contains("reached the 1-file limit"), "{text}");
}

#[test]
fn coverage_says_how_many_absorbed_sites_were_not_checked() {
    let mut report = report_with_gaps();
    report
        .ledger
        .record_evidence_check(crate::scan_ledger::EvidenceCheck {
            site_checked: 4,
            site_verified: 4,
            site_exempt: 3,
            ..Default::default()
        });
    let gaps = coverage_gaps(&report);
    assert_eq!(
        gaps.evidence_note.as_deref(),
        Some(
            "3 absorbed sites were not checked against their files: they describe an impact \
             rather than quote code, belong to a summary row, sit in another repository or cite \
             no line."
        )
    );
    // The PDF data, findings.json and the workbook carry it.
    let sealed = report.seal();
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&sealed, &HashMap::new()),
        None,
        &empty_opts(),
    );
    assert!(render_data_json(&json)
        .unwrap()
        .contains("3 absorbed sites were not checked"));
    let export = crate::xlsx_export::build_findings_export(
        &crate::report_export::ReviewedReport::unreviewed(&sealed, &HashMap::new()),
        None,
        &json,
        &HashMap::new(),
    );
    let value = serde_json::to_value(&export).unwrap();
    assert!(value["coverage"]["evidence_note"].is_string());
    let bytes = crate::xlsx_export::build_workbook(
        &crate::report_export::ReviewedReport::unreviewed(&sealed, &HashMap::new()),
        None,
        &empty_opts(),
    )
    .unwrap();
    let all = crate::release_gate::xlsx_strings(&bytes)
        .unwrap()
        .join("\n");
    assert!(all.contains("3 absorbed sites were not checked"), "{all}");
}

#[test]
fn coverage_is_silent_when_every_absorbed_site_was_checked() {
    let mut report = report_with_gaps();
    report
        .ledger
        .record_evidence_check(crate::scan_ledger::EvidenceCheck {
            site_checked: 4,
            site_verified: 4,
            ..Default::default()
        });
    assert!(coverage_gaps(&report).evidence_note.is_none());
}
