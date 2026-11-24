//! H4 item 1: every failed AI pass and every skipped file reaches Coverage in the PDF data, the
//! workbook and `json/inspection.json`, from one computation.

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
        format!("over the {} file size limit", max_file_label())
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

use crate::onboard::files::{
    max_file_label, read_local_repo_files, read_local_repo_files_capped, MAX_FILE_BYTES,
};
use crate::scan_ledger::{RepoSkips, SkippedFileList};

/// A report whose ledger holds exactly what the real walk of `root` skipped, recorded the way the
/// scan records it (`SkippedFileList::record_repo`).
fn report_from_walk(extracted: crate::onboard::files::ExtractedRepo) -> crate::onboard::ScanReport {
    report_from_walk_with(extracted, Vec::new())
}

/// [`report_from_walk`] with Excluded by agreement patterns that match only files never read.
fn report_from_walk_with(
    extracted: crate::onboard::files::ExtractedRepo,
    never_read: Vec<String>,
) -> crate::onboard::ScanReport {
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
            excluded_by_agreement: extracted.excluded_by_agreement,
            long_line: extracted.skipped_long_line,
            generated: extracted.generated,
            generated_unsearched: Vec::new(),
            not_inspected_type: Vec::new(),
            excluded_never_read: never_read,
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

/// V1 item 32: a file that declares itself generated is named in Coverage with the one phrase
/// every surface uses, counted in the summary, and (with the floor run) counted as searched for
/// secrets. The ledger is recorded from the real walk.
#[test]
fn a_generated_file_is_named_in_coverage_as_scanned_for_secrets_only_and_not_billed() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("Migrations")).unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/a.rs"), "fn a() {}\n").unwrap();
    std::fs::write(
        dir.path().join("Migrations/Init.Designer.cs"),
        "// <auto-generated />\nclass A {}\n",
    )
    .unwrap();
    let extracted =
        read_local_repo_files(dir.path(), &crate::onboard::scope::RepoScope::none()).unwrap();
    assert_eq!(extracted.generated, ["Migrations/Init.Designer.cs"]);

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
            oversize: Vec::new(),
            beyond_cap: Vec::new(),
            not_text: Vec::new(),
            excluded_by_agreement: Vec::new(),
            long_line: Vec::new(),
            generated: extracted.generated,
            generated_unsearched: Vec::new(),
            not_inspected_type: Vec::new(),
            excluded_never_read: Vec::new(),
            sizes: &extracted.skipped_sizes,
            file_cap: extracted.file_cap,
        },
    );
    // The floor searched the one generated file, as the scan records it.
    list.record_secret_search(1, 0, 0, 0);
    let totals = list.totals();
    let (skipped_files, skipped_beyond_cap_total) = list.finish();
    report.ledger.record_input(ScanInputFacts {
        skipped_files,
        skipped_beyond_cap_total,
        skipped_totals: totals,
        ..ScanInputFacts::default()
    });

    let gaps = coverage_gaps(&report);
    assert_eq!(gaps.skipped_files.len(), 1);
    assert_eq!(gaps.skipped_files[0].reason_code, SkipReason::Generated);
    assert_eq!(
        gaps.skipped_files[0].reason,
        "generated: searched for secrets only, not billed"
    );
    assert_eq!(
        gaps.skipped_summary.as_deref(),
        Some(
            "1 file was not read for the inspection: 1 generated (searched for secrets only, \
             not billed). 1 of these files was still searched for secrets."
        )
    );
}

/// Round 1 fix 3: Coverage never says a generated file was searched for secrets when it was not.
/// The label follows what ran: searched, or (floor off, over the search ceiling, not text) not
/// searched. The summary line carries one colon, not two.
#[test]
fn a_generated_file_that_was_not_searched_is_worded_as_not_searched() {
    let sizes = HashMap::new();
    let mut list = SkippedFileList::default();
    list.record_repo(
        "owner/repo",
        RepoSkips {
            oversize: Vec::new(),
            beyond_cap: Vec::new(),
            not_text: Vec::new(),
            excluded_by_agreement: Vec::new(),
            long_line: Vec::new(),
            generated: vec!["a/Searched.Designer.cs".to_string()],
            generated_unsearched: vec!["b/Not.Designer.cs".to_string()],
            not_inspected_type: Vec::new(),
            excluded_never_read: Vec::new(),
            sizes: &sizes,
            file_cap: 20_000,
        },
    );
    let totals = list.totals();
    assert_eq!((totals.generated, totals.generated_unsearched), (1, 1));
    assert_eq!(totals.total(), 2);
    assert_eq!(
        SkipReason::Generated.label(),
        "generated: searched for secrets only, not billed"
    );
    assert_eq!(
        SkipReason::GeneratedUnsearched.label(),
        "generated: not read, not billed, not searched for secrets"
    );
    // The floor did not run: no sentence claims a search.
    let sentence = super::coverage_gaps::skipped_summary_sentence(&totals, &[]).unwrap();
    assert_eq!(
        sentence,
        "2 files were not read for the inspection: 1 generated (searched for secrets only, not \
         billed), 1 generated (not read, not billed, not searched for secrets)."
    );
    assert_eq!(sentence.matches(':').count(), 1, "{sentence}");
    let (listed, _) = list.finish();
    let reason_of = |path: &str| {
        listed
            .iter()
            .find(|f| f.path == path)
            .map(|f| f.reason)
            .unwrap()
    };
    assert_eq!(reason_of("a/Searched.Designer.cs"), SkipReason::Generated);
    assert_eq!(
        reason_of("b/Not.Designer.cs"),
        SkipReason::GeneratedUnsearched
    );
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
    let extracted =
        read_local_repo_files(dir.path(), &crate::onboard::scope::RepoScope::none()).unwrap();
    assert_eq!(extracted.skipped_oversize, ["src/huge.rs"]);

    let gaps = coverage_gaps(&report_from_walk(extracted));
    assert_eq!(gaps.skipped_files.len(), 1);
    let row = &gaps.skipped_files[0];
    assert_eq!(row.path, "src/huge.rs");
    assert_eq!(row.reason_code, SkipReason::Oversize);
    assert_eq!(
        row.reason,
        format!("over the {} file size limit", max_file_label())
    );
    assert_eq!(row.size_bytes, Some((MAX_FILE_BYTES + 1_000) as u64));
    assert_eq!(row.size, "1.1 MB");
    // The count is in the Coverage summary.
    assert_eq!(
        gaps.skipped_summary.as_deref(),
        Some(
            format!(
                "1 file was not read for the inspection: 1 over the {} file size limit.",
                max_file_label()
            )
            .as_str()
        )
    );
    assert!(gaps.file_limit_notes.is_empty());
}

#[test]
fn a_repo_over_a_lowered_file_cap_discloses_the_unscanned_count() {
    let dir = tempfile::tempdir().unwrap();
    let files: Vec<(String, usize)> = (0..7).map(|i| (format!("src/f{i}.rs"), 10)).collect();
    let refs: Vec<(&str, usize)> = files.iter().map(|(p, n)| (p.as_str(), *n)).collect();
    write_tree(dir.path(), &refs);
    let extracted =
        read_local_repo_files_capped(dir.path(), 3, &crate::onboard::scope::RepoScope::none())
            .unwrap();
    assert!(extracted.truncated);
    assert_eq!(extracted.files.len(), 3);

    let gaps = coverage_gaps(&report_from_walk(extracted));
    // The limit is stated for the run, with the number of files not read for the inspection. It is the limit
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
         4 files were not read for the inspection."
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
        Some("4 files were not read for the inspection: 4 beyond the 3-file limit.")
    );
}

#[test]
fn the_totals_count_every_skipped_file_including_those_past_the_named_list() {
    let gaps = coverage_gaps(&report_with_a_huge_skip_list());
    // 2,500 oversize and 1,200 past the cap: only 1,000 of each are named.
    assert_eq!(
        gaps.skipped_summary.as_deref(),
        Some(
            format!(
                "3700 files were not read for the inspection: 2500 over the {} file size limit, 1200 beyond the \
                 20,000-file limit.",
                max_file_label()
            )
            .as_str()
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
    let extracted =
        read_local_repo_files_capped(dir.path(), 2, &crate::onboard::scope::RepoScope::none())
            .unwrap();
    let report = report_from_walk(extracted);
    let sealed = report.seal();
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&sealed, &HashMap::new()),
        None,
        &empty_opts(),
    );

    // The PDF's data.
    let data = render_data_json(&json).expect("renders");
    assert!(data.contains("1.1 MB"), "{data}");
    assert!(
        data.contains("files were not read for the inspection"),
        "{data}"
    );
    assert!(data.contains("reached the 2-file limit"), "{data}");

    // json/inspection.json.
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
    assert!(all.contains("1.1 MB"), "{all}");
    assert!(
        all.contains("files were not read for the inspection"),
        "{all}"
    );
    assert!(all.contains("reached the 2-file limit"), "{all}");
}

/// Round 1 item 13: "excluded by agreement" reaches the PDF's data, the workbook's Coverage
/// sheet and (when typst is installed) the rendered PDF text, not only `coverage_gaps` and JSON.
#[tokio::test]
async fn every_renderer_says_excluded_by_agreement() {
    let dir = tempfile::tempdir().unwrap();
    write_tree(dir.path(), &[("a.rs", 10), ("seeds/data.json", 40)]);
    let scope = crate::onboard::scope::ScanScope::from_patterns(&["app/seeds/**".to_string()])
        .unwrap()
        .for_repo("acme/app");
    let extracted = crate::onboard::files::read_local_repo_files(dir.path(), &scope).unwrap();
    let report = report_from_walk(extracted);
    let sealed = report.seal();
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&sealed, &HashMap::new()),
        None,
        &empty_opts(),
    );
    let data = render_data_json(&json).expect("renders");
    assert!(data.contains("excluded by agreement"), "{data}");
    assert!(data.contains("seeds/data.json"), "{data}");

    let bytes = crate::xlsx_export::build_workbook(
        &crate::report_export::ReviewedReport::unreviewed(&sealed, &HashMap::new()),
        None,
        &empty_opts(),
    )
    .unwrap();
    let all = crate::release_gate::xlsx_strings(&bytes)
        .expect("a workbook")
        .join("\n");
    assert!(all.contains("excluded by agreement"), "{all}");
    assert!(all.contains("seeds/data.json"), "{all}");

    if which_typst().is_none() {
        eprintln!("skipping the PDF half: typst not on PATH");
        return;
    }
    let pdf = compile_pdf(&json).await.expect("the PDF compiles");
    let text = pdf_extract::extract_text_from_mem(&pdf).unwrap_or_default();
    assert!(text.contains("excluded by agreement"), "{text}");
}

/// Zach's ruling (round 2): a pattern that matches only files never read is listed in the package,
/// marked as having removed nothing read or billed, in the JSON, the PDF data, the workbook and
/// (typst installed) the PDF text; the marking passes the client text gates.
#[tokio::test]
async fn a_never_read_pattern_is_listed_with_its_marking_in_every_renderer() {
    let dir = tempfile::tempdir().unwrap();
    write_tree(dir.path(), &[("a.rs", 10)]);
    let extracted =
        read_local_repo_files(dir.path(), &crate::onboard::scope::RepoScope::none()).unwrap();
    let report = report_from_walk_with(extracted, vec!["app/node_modules/**".to_string()]);
    let marking = "excluded by agreement; nothing here is read by default";
    let gaps = coverage_gaps(&report);
    let row = gaps
        .skipped_files
        .iter()
        .find(|r| r.path == "app/node_modules/**")
        .expect("listed");
    assert_eq!(row.reason, marking);
    assert_eq!(
        gaps.skipped_summary.as_deref(),
        Some("1 pattern is excluded by agreement; nothing here is read by default.")
    );
    assert!(
        !marking.contains('\u{2014}') && !marking.contains('\u{2013}'),
        "no dashes"
    );
    assert!(!marking.contains(", and "), "no serial comma");
    let sealed = report.seal();
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&sealed, &HashMap::new()),
        None,
        &empty_opts(),
    );
    let data = render_data_json(&json).expect("renders");
    assert!(
        data.contains(marking) && data.contains("app/node_modules/**"),
        "{data}"
    );
    let bytes = crate::xlsx_export::build_workbook(
        &crate::report_export::ReviewedReport::unreviewed(&sealed, &HashMap::new()),
        None,
        &empty_opts(),
    )
    .unwrap();
    let all = crate::release_gate::xlsx_strings(&bytes)
        .expect("a workbook")
        .join("\n");
    assert!(
        all.contains(marking) && all.contains("app/node_modules/**"),
        "{all}"
    );
    if which_typst().is_none() {
        eprintln!("skipping the PDF half: typst not on PATH");
        return;
    }
    let pdf = compile_pdf(&json).await.expect("the PDF compiles");
    let text = pdf_extract::extract_text_from_mem(&pdf).unwrap_or_default();
    assert!(text.contains("nothing here is read by default"), "{text}");
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
    let report = report_from_walk(
        read_local_repo_files_capped(dir.path(), 1, &crate::onboard::scope::RepoScope::none())
            .unwrap(),
    );
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );
    let pdf = compile_pdf(&json).await.expect("the PDF compiles");
    let text = pdf_extract::extract_text_from_mem(&pdf).unwrap_or_default();
    assert!(text.contains("1.1 MB"), "{text}");
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
    // The PDF data, json/inspection.json and the workbook carry it.
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

/// A pattern that removed nothing is not a file: a project-wide wildcard pattern recorded by
/// several repos is one pattern in Coverage and one row, and no file count includes it.
#[test]
fn a_never_read_pattern_is_counted_once_and_is_not_a_file() {
    let mut list = SkippedFileList::default();
    let sizes = HashMap::new();
    for repo in ["owner/a", "owner/b", "owner/c"] {
        list.record_repo(
            repo,
            RepoSkips {
                oversize: Vec::new(),
                beyond_cap: Vec::new(),
                not_text: Vec::new(),
                excluded_by_agreement: Vec::new(),
                long_line: Vec::new(),
                generated: Vec::new(),
                generated_unsearched: Vec::new(),
                not_inspected_type: Vec::new(),
                excluded_never_read: vec!["*/node_modules/**".to_string()],
                sizes: &sizes,
                file_cap: 20_000,
            },
        );
    }
    let totals = list.totals();
    assert_eq!(
        totals.excluded_never_read, 1,
        "one pattern, not one per repo"
    );
    assert_eq!(totals.total(), 0, "a pattern is not a skipped file");
    let (rows, _) = list.finish();
    assert_eq!(rows.len(), 1);
    let sentence = super::coverage_gaps::skipped_summary_sentence(&totals, &[]).unwrap();
    assert_eq!(
        sentence,
        "1 pattern is excluded by agreement; nothing here is read by default."
    );
}

/// A file excluded by agreement is never opened, so the secrets sentence names it as not searched.
#[test]
fn the_secrets_sentence_counts_files_excluded_by_agreement_as_not_searched() {
    let totals = crate::scan_ledger::SkippedTotals {
        excluded_by_agreement: 3,
        not_text: 2,
        secret_search_ran: true,
        ..Default::default()
    };
    assert_eq!(
        super::coverage_gaps::skipped_summary_sentence(&totals, &[]).unwrap(),
        "5 files were not read for the inspection: 2 not UTF-8 text, 3 excluded by agreement. \
         5 files were not searched for secrets: 2 not UTF-8 text, 3 excluded by agreement."
    );
}

/// Over-limit files the run's secret search budget left unsearched are said to be "not searched
/// for secrets", with the budget named, in the same sentence as every other unsearched file.
#[test]
fn the_secrets_sentence_names_files_the_search_budget_left_unsearched() {
    let mut list = crate::scan_ledger::SkippedFileList::default();
    list.record_secret_search(10, 0, 0, 2);
    let totals = crate::scan_ledger::SkippedTotals {
        oversize: 12,
        ..list.totals()
    };
    let sentence = super::coverage_gaps::skipped_summary_sentence(&totals, &[]).unwrap();
    assert!(
        sentence.contains(
            "2 files were not searched for secrets: 2 past the 200 MB search budget for one run."
        ),
        "{sentence}"
    );
    assert!(
        sentence.contains("10 of these files were still searched"),
        "{sentence}"
    );
}
