//! The export panel's words against the package (work order E). The panel and its help text are
//! built from lists in `camerata_ui_core::export_panel`; this proves every name on those lists is
//! in the package, and that the text holds no word the package retired, so the panel cannot drift
//! from what the client receives.

use std::collections::HashMap;

use camerata_ui_core::export_panel::{
    blocked_line, help_body, HEADING, NAMED_FILES, NAMED_PDF_SECTIONS, NAMED_SHEETS,
};

use super::tests::scan_report;
use super::*;
use crate::report_export::{build_report_json, render_data_json};
use crate::xlsx_export::vocabulary::{banned_synonym_in, PDF_SECTIONS, SHEETS};

const STEM: &str = "acme-inspection";
const STAND_IN_PDF: &[u8] = b"%PDF-1.7 stand-in for the gate tests";

/// A released package for a seeded project, with its files by path.
fn released_files() -> std::collections::BTreeMap<String, Vec<u8>> {
    let options = ReportOptions {
        client_name: "Example Client Ltd".to_string(),
        ..ReportOptions::default()
    };
    let sealed = scan_report().seal();
    let reviewed = crate::report_export::ReviewedReport::unreviewed(&sealed, &HashMap::new());
    let json = build_report_json(&reviewed, None, &options);
    let data = render_data_json(&json).expect("data.json renders");
    let package = assemble_package(
        &reviewed,
        None,
        &options,
        STEM,
        (json, data, STAND_IN_PDF.to_vec()),
    )
    .expect("the package assembles");
    let released = release(package).expect("the package clears the gate");
    read_zip_entries(&released.bytes).expect("a zip")
}

#[test]
fn every_pdf_section_the_panel_names_is_a_section_of_the_pdf() {
    for name in NAMED_PDF_SECTIONS {
        assert!(
            PDF_SECTIONS.contains(&name),
            "the panel names a PDF section the PDF lacks: {name}"
        );
    }
}

#[test]
fn every_sheet_the_panel_names_is_a_sheet_of_the_workbook() {
    let sheets: Vec<&str> = SHEETS.iter().map(|s| s.name).collect();
    for name in NAMED_SHEETS {
        assert!(
            sheets.contains(&name),
            "the panel names a sheet the package lacks: {name}"
        );
    }
    // And the workbook a seeded project gets really has them.
    let files = released_files();
    let xlsx = files
        .get(&format!("{STEM}-findings.xlsx"))
        .expect("the package has its workbook");
    let present = xlsx_sheet_names(xlsx).expect("the workbook reads");
    for name in NAMED_SHEETS {
        assert!(
            present.iter().any(|s| s == name),
            "the workbook has no {name} sheet: {present:?}"
        );
    }
}

#[test]
fn every_file_the_panel_names_is_written_and_the_readme_names_it_too() {
    let files = released_files();
    let readme = String::from_utf8(files["README.txt"].clone()).expect("README is text");
    for path in NAMED_FILES {
        assert!(
            files.contains_key(path),
            "the panel names {path} and the package does not write it: {:?}",
            files.keys().collect::<Vec<_>>()
        );
        if path != "README.txt" {
            assert!(
                readme.contains(path),
                "the panel names {path} and the README does not: {readme}"
            );
        }
    }
}

#[test]
fn the_panel_text_holds_no_retired_word() {
    let mut texts = vec![HEADING.to_string(), help_body()];
    for (rows, in_review, groups, sites) in [(8, 0, 0, 0), (8, 3, 0, 0), (1, 1, 2, 5), (0, 0, 1, 1)]
    {
        texts.push(blocked_line(rows, in_review, groups, sites));
    }
    texts.push("Operator files (internal, not part of the client package)".to_string());
    texts.push("Download triage log".to_string());
    for text in texts {
        assert_eq!(
            banned_synonym_in(&text),
            None,
            "a retired word in the panel text: {text}"
        );
    }
}

/// The five claims the panel used to make that the package does not bear out.
#[test]
fn the_panel_no_longer_makes_the_false_claims() {
    let body = help_body();
    for old in [
        "category scorecard",
        "per-category",
        "False Positives",
        "curated findings",
        "draft mid-engagement",
    ] {
        assert!(!body.contains(old), "{old} is still in the panel text");
    }
    assert!(body.contains("Area scorecard"));
    assert!(body.contains("The export is refused while any row needs a decision."));
}
