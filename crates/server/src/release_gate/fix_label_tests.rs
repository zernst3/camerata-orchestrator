//! Defect D8: every export labels each fix "Tailored fix" (written for this code, passed the
//! checks) or "Standard fix" (the rule's own guidance), in neutral client wording, and the
//! release gate demands the label. The reasons the checks turned a model fix down are for the
//! operator and never reach a client file.

use std::collections::{BTreeMap, HashMap};

use serde_json::Value;

use super::tests::{client_options, scan_report};
use super::*;
use crate::onboard::{Finding, ScanReport};
use crate::report_export::{
    build_report_json, mark_standard_fix, render_data_json, STANDARD_FIX_LABEL, TAILORED_FIX_LABEL,
};

const STEM: &str = "acme-inspection";
const REASON: &str = "you named `zzUniqueRejectedName`, which appears neither in this finding's \
                      evidence nor anywhere in the repository";

fn row(line: usize, detail: &str, fix: Option<&str>, standard: bool) -> Finding {
    let mut f = scan_report().findings.remove(0);
    f.line = line;
    f.detail = detail.to_string();
    f.fix_specific = fix.map(str::to_string);
    if standard {
        mark_standard_fix(&mut f, REASON);
    }
    f
}

fn report(detail: &str) -> ScanReport {
    let mut report = scan_report();
    report.findings = vec![
        row(
            10,
            detail,
            Some("Bind the value as a parameter in `db.rs` instead of formatting it in."),
            false,
        ),
        row(
            20,
            detail,
            Some("Use a parameterized query for the SQL built at this place."),
            true,
        ),
        row(30, detail, None, false),
    ];
    report
}

fn package_for(report: ScanReport) -> ProductPackage {
    let sealed = report.seal();
    let options = client_options();
    let json = build_report_json(&sealed, &HashMap::new(), None, &options);
    let data = render_data_json(&json).expect("data.json renders");
    assemble_package(
        &sealed,
        &HashMap::new(),
        None,
        &options,
        STEM,
        (json, data, b"%PDF-1.7 stand-in for the gate tests".to_vec()),
    )
    .expect("the package assembles")
}

fn label_of(rows: &[Value], line: u64) -> String {
    rows.iter()
        .find(|r| r["line"].as_u64() == Some(line))
        .unwrap_or_else(|| panic!("no row at line {line}"))["fix_type"]
        .as_str()
        .expect("a fix_type field")
        .to_string()
}

fn expect_labels(rows: &[Value], what: &str) {
    assert_eq!(label_of(rows, 10), TAILORED_FIX_LABEL, "{what}");
    assert_eq!(label_of(rows, 20), STANDARD_FIX_LABEL, "{what}");
    assert_eq!(label_of(rows, 30), "", "{what}: no fix text, no label");
}

/// The label is in findings.json and in both formats of the all-findings dataset, whether the
/// finding's explanation is one sentence (the `terse_output` shape) or several paragraphs, and
/// the gate clears the package.
#[test]
fn every_fix_is_labelled_in_findings_json_and_the_datasets_with_terse_output_on_and_off() {
    let long = "The query is built by string formatting. A caller controls the value.\n\nThe \
                value reaches the database unescaped, so it can change the statement.";
    for detail in ["Raw SQL built by string formatting.", long] {
        let package = package_for(report(detail));
        let doc: Value = serde_json::from_slice(&package.findings_json).expect("parses");
        expect_labels(
            doc["findings"].as_array().expect("findings"),
            "findings.json",
        );

        let datasets: BTreeMap<String, Vec<u8>> = package.datasets.iter().cloned().collect();
        let all: Vec<Value> =
            serde_json::from_slice(&datasets["json/all-findings.json"]).expect("parses");
        expect_labels(&all, "json/all-findings.json");

        let mut reader = csv::Reader::from_reader(datasets["csv/all-findings.csv"].as_slice());
        let headers: Vec<String> = reader.headers().unwrap().iter().map(String::from).collect();
        let (line_col, label_col) = (
            headers.iter().position(|h| h == "line").unwrap(),
            headers.iter().position(|h| h == "fix_type").unwrap(),
        );
        let labels: BTreeMap<String, String> = reader
            .records()
            .map(|r| {
                let r = r.unwrap();
                (r[line_col].to_string(), r[label_col].to_string())
            })
            .collect();
        assert_eq!(labels["10"], TAILORED_FIX_LABEL);
        assert_eq!(labels["20"], STANDARD_FIX_LABEL);
        assert_eq!(labels["30"], "");

        let released = release(package).expect("a labelled package clears the gate");
        assert!(released.report.is_clean());
    }
}

/// The wording is neutral: no client file carries a word that reads as a failure, and none
/// carries the reason the checks turned the model's fix down.
#[test]
fn no_client_file_carries_the_rejection_reason_or_a_failure_word_about_the_fix() {
    let package = package_for(report("Raw SQL built by string formatting."));
    let released = release(package).expect("clears the gate");
    let entries = read_zip_entries(&released.bytes).expect("zip");
    for (name, bytes) in &entries {
        if name.ends_with(".xlsx") || name.ends_with(".pdf") {
            continue;
        }
        let text = String::from_utf8_lossy(bytes);
        assert!(
            !text.contains("zzUniqueRejectedName"),
            "the rejection reason reached {name}"
        );
        assert!(
            !text.contains("fix_rejection") && !text.contains("fix_source"),
            "an operator-only key reached {name}"
        );
    }
    for (name, text) in [
        (
            "findings.json",
            String::from_utf8_lossy(&entries["findings.json"]).into_owned(),
        ),
        (
            "json/all-findings.json",
            String::from_utf8_lossy(&entries["json/all-findings.json"]).into_owned(),
        ),
    ] {
        for label in text.lines().filter(|l| l.contains("\"fix_type\"")) {
            let lower = label.to_ascii_lowercase();
            for word in ["fail", "fallback", "reject", "incomplete"] {
                assert!(!lower.contains(word), "{name}: {label}");
            }
        }
    }
}

/// The gate demands the label: a row whose fix is unlabelled, or labelled with anything but the
/// two client labels, is refused.
#[test]
fn the_gate_refuses_an_unlabelled_or_misnamed_fix() {
    let released = release(package_for(report("Raw SQL built by string formatting.")))
        .expect("a labelled package clears the gate");
    let entries = read_zip_entries(&released.bytes).expect("zip");
    assert!(
        dataset_parity_problems(&entries).is_empty(),
        "control: {:?}",
        dataset_parity_problems(&entries)
    );

    for tamper in [Tamper::Remove, Tamper::Rename] {
        let mut tampered = entries.clone();
        // The same edit in findings.json and all-findings.json, so the parity check passes and
        // only the label check can notice.
        let mut doc: Value = serde_json::from_slice(&tampered["findings.json"]).expect("parses");
        let mut all: Vec<Value> =
            serde_json::from_slice(&tampered["json/all-findings.json"]).expect("parses");
        for rows in [doc["findings"].as_array_mut().unwrap(), &mut all] {
            for r in rows.iter_mut() {
                if r["line"].as_u64() == Some(10) {
                    match tamper {
                        Tamper::Remove => {
                            r.as_object_mut().unwrap().remove("fix_type");
                        }
                        Tamper::Rename => r["fix_type"] = Value::from("Fallback fix"),
                    }
                }
            }
        }
        tampered.insert(
            "findings.json".to_string(),
            serde_json::to_vec_pretty(&doc).unwrap(),
        );
        tampered.insert(
            "json/all-findings.json".to_string(),
            serde_json::to_vec_pretty(&all).unwrap(),
        );
        let problems = dataset_parity_problems(&tampered);
        assert!(
            problems
                .iter()
                .any(|q| q.file == "findings.json" && q.message.contains("fix")),
            "{problems:?}"
        );
    }
}

#[derive(Clone, Copy)]
enum Tamper {
    Remove,
    Rename,
}

/// The workbook's All Findings sheet carries the same label beside each fix.
#[test]
fn the_workbook_labels_each_fix_beside_the_fix() {
    let package = package_for(report("Raw SQL built by string formatting."));
    let grid = xlsx_sheet_grid(&package.xlsx, "All Findings").expect("the sheet");
    let header = &grid[0];
    let col = |name: &str| {
        header
            .iter()
            .position(|h| h == name)
            .unwrap_or_else(|| panic!("no column {name:?} in {header:?}"))
    };
    let (fix_col, type_col) = (col("Fix (specific to this finding)"), col("Fix type"));
    let mut seen = BTreeMap::new();
    for row in &grid[1..] {
        seen.insert(row[fix_col].clone(), row[type_col].clone());
    }
    assert_eq!(
        seen["Bind the value as a parameter in db.rs instead of formatting it in."],
        TAILORED_FIX_LABEL
    );
    assert_eq!(
        seen["Use a parameterized query for the SQL built at this place."],
        STANDARD_FIX_LABEL
    );
    assert_eq!(seen[""], "");
}

/// The PDF prints the same label in front of each fix. The curated sites of the PDF's data carry
/// it in `fix_type`, and the compiled PDF (when Typst is installed) shows it.
#[tokio::test]
async fn the_pdf_labels_each_fix_in_its_data_and_in_the_rendered_page() {
    let sealed = report("Raw SQL built by string formatting.").seal();
    let json = build_report_json(&sealed, &HashMap::new(), None, &client_options());
    let labels: BTreeMap<usize, String> = json
        .curated_findings
        .iter()
        .chain(&json.held_for_review_findings)
        .flat_map(|group| group.sites.iter())
        .map(|site| (site.line, site.fix_type.clone()))
        .collect();
    assert_eq!(labels[&10], TAILORED_FIX_LABEL, "{labels:?}");
    assert_eq!(labels[&20], STANDARD_FIX_LABEL, "{labels:?}");
    assert_eq!(labels[&30], "", "{labels:?}");

    if std::process::Command::new("typst")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("skipping the rendered-page leg: typst is not on PATH");
        return;
    }
    let pdf = crate::report_export::compile_pdf(&json)
        .await
        .expect("the PDF compiles");
    let text = pdf_extract::extract_text_from_mem(&pdf).expect("the PDF text");
    assert!(text.contains("Tailored fix:"), "{text:?}");
    assert!(text.contains("Standard fix:"), "{text:?}");
}
