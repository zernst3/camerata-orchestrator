//! The gate's text-style check: a planted em dash, en dash or multiplication sign is refused in
//! every kind of client file, and a clean package passes. Built like the secrets tests: a real
//! package around a stand-in PDF, and the archive checks over a real zip.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use super::tests::{
    internal_package, package, plant, rewrite_xlsx_part, rezip, typst_on_path, Surface,
};
use super::*;

const EM: &str = "\u{2014}";
const EN: &str = "\u{2013}";
const TIMES: &str = "\u{00D7}";

fn planted_text(character: &str) -> String {
    format!("Messages readable by everyone {character} the select policy")
}

fn style_problems(report: &GateReport) -> Vec<&Problem> {
    report
        .checks
        .iter()
        .filter(|o| o.check == CheckId::ClientTextStyle)
        .flat_map(|o| o.problems.iter())
        .collect()
}

fn failure_of(package: ProductPackage) -> GateFailure {
    match release(package) {
        Err(ReleaseError::Blocked(failure)) => failure,
        Err(other) => panic!("expected the gate to block, got {other}"),
        Ok(_) => panic!("expected the gate to block, but the package was released"),
    }
}

#[test]
fn a_clean_package_clears_the_text_style_check() {
    let released = release(package()).expect("a clean package must clear the gate");
    assert!(released.report.is_clean());
    assert!(
        released.report.check_names().contains(&"client text style"),
        "the check must be one of the checks that ran: {:?}",
        released.report.check_names()
    );
    assert!(style_problems(&released.report).is_empty());
}

#[test]
fn each_banned_character_planted_on_each_surface_is_refused_with_its_place() {
    for character in [EM, EN, TIMES] {
        for (surface, file, location) in [
            (
                Surface::DataJson,
                "PDF data (data.json)",
                "$.a_field_added_tomorrow.note",
            ),
            (
                Surface::FindingsJson,
                "findings.json",
                "$.a_field_added_tomorrow",
            ),
            (Surface::DatasetJson, "json/deviations.json", "$[0].snippet"),
            (
                Surface::DatasetCsv,
                "csv/conventions.csv",
                "row 1, column \"snippet\"",
            ),
            (
                Surface::XlsxSharedString,
                "acme-inspection-findings.xlsx",
                "shared string #",
            ),
            (Surface::Readme, "README.txt", "line "),
        ] {
            let mut pkg = package();
            plant(&mut pkg, surface, &planted_text(character));
            let failure = failure_of(pkg);
            let problems = style_problems(&failure.report);
            let hit = problems
                .iter()
                .find(|p| p.file == file && p.location.starts_with(location))
                .unwrap_or_else(|| {
                    panic!(
                        "{surface:?} {character:?}: no problem at {file} {location}: {problems:?}"
                    )
                });
            assert!(
                hit.message.contains("U+"),
                "the message names the character: {}",
                hit.message
            );
            assert_eq!(
                hit.evidence.as_deref().map(|e| e.contains(character)),
                Some(true),
                "{surface:?}: the evidence shows the place: {hit:?}"
            );
        }
    }
}

/// An edit to one member of a released package.
type Edit = Box<dyn Fn(&mut BTreeMap<String, Vec<u8>>)>;

/// The released clean package's members, edited in one place and zipped again.
fn clean_entries() -> BTreeMap<String, Vec<u8>> {
    let released = release(package()).expect("clean");
    read_zip_entries(&released.bytes).expect("zip")
}

#[test]
fn an_audit_of_a_zip_refuses_a_banned_character_in_every_member_kind() {
    let cases: Vec<(&str, Edit)> = vec![
        (
            "README.txt",
            Box::new(|e| {
                let readme = e.get_mut("README.txt").expect("README");
                readme.extend_from_slice(format!("\nExtra: {}\n", planted_text(EM)).as_bytes());
            }),
        ),
        (
            "manifest.json",
            Box::new(|e| {
                let mut manifest: Value =
                    serde_json::from_slice(&e["manifest.json"]).expect("manifest");
                manifest["note"] = json!(planted_text(EM));
                e.insert(
                    "manifest.json".to_string(),
                    serde_json::to_vec_pretty(&manifest).expect("serializes"),
                );
            }),
        ),
        (
            "decisions.json",
            Box::new(|e| {
                e.insert(
                    "decisions.json".to_string(),
                    serde_json::to_vec(&json!({ "note": planted_text(EM) })).expect("serializes"),
                );
            }),
        ),
        (
            "csv/tickets.csv",
            Box::new(|e| {
                let csv = e.get_mut("csv/tickets.csv").expect("tickets");
                csv.extend_from_slice(format!("\"{}\"\n", planted_text(EM)).as_bytes());
            }),
        ),
        (
            "json/coverage.json",
            Box::new(|e| {
                e.insert(
                    "json/coverage.json".to_string(),
                    serde_json::to_vec(&json!([{ "title": planted_text(EM) }])).expect("json"),
                );
            }),
        ),
        (
            "acme-inspection-findings.xlsx",
            Box::new(|e| {
                let name = "acme-inspection-findings.xlsx";
                let escaped = planted_text(EM);
                let edited = rewrite_xlsx_part(&e[name], "xl/sharedStrings.xml", |xml| {
                    xml.replacen("</sst>", &format!("<si><t>{escaped}</t></si></sst>"), 1)
                });
                e.insert(name.to_string(), edited);
            }),
        ),
    ];
    for (file, edit) in cases {
        let mut entries = clean_entries();
        edit(&mut entries);
        let report = audit_zip(&rezip(&entries), None);
        let problems = style_problems(&report);
        assert!(
            problems.iter().any(|p| p.file == file),
            "{file}: no text-style problem: {problems:?}"
        );
    }
}

#[test]
fn an_audit_of_a_zip_refuses_a_banned_character_in_the_pdf_text() {
    let entries = clean_entries();
    let bytes = rezip(&entries);
    let clean = audit_zip(&bytes, Some("A clean page of text, with a: colon."));
    assert!(style_problems(&clean).is_empty());
    let report = audit_zip(&bytes, Some(&format!("Page one\n{}\n", planted_text(EN))));
    let problems = style_problems(&report);
    assert!(
        problems.iter().any(|p| p.file.ends_with(".pdf")),
        "no problem on the PDF: {problems:?}"
    );
}

#[test]
fn a_member_with_many_hits_reports_the_first_few_and_the_count() {
    let mut entries = clean_entries();
    let many = (0..40)
        .map(|i| format!("entry {i} {EM} text"))
        .collect::<Vec<_>>()
        .join("\n");
    entries.insert("README.txt".to_string(), many.into_bytes());
    let report = audit_zip(&rezip(&entries), None);
    let problems: Vec<_> = style_problems(&report)
        .into_iter()
        .filter(|p| p.file == "README.txt")
        .collect();
    assert_eq!(problems.len(), 6, "five places and the count: {problems:?}");
    assert!(problems
        .iter()
        .any(|p| p.message.contains("35 more") && p.message.contains("40 in all")));
}

#[test]
fn the_pdf_only_route_refuses_a_banned_character_in_its_data() {
    let data = serde_json::to_string(&json!({ "headline": planted_text(EM) })).expect("json");
    let failure = check_pdf_data("acme", &data).expect_err("a banned character blocks the route");
    assert!(!style_problems(&failure.report).is_empty());
    assert!(check_pdf_data("acme", "{\"headline\": \"clean\"}").is_ok());
}

#[tokio::test]
async fn a_compiled_pdf_with_a_banned_character_in_its_text_is_found() {
    if !typst_on_path() {
        eprintln!("skipping a_compiled_pdf_with_a_banned_character_in_its_text_is_found: no typst");
        return;
    }
    let clean = package();
    let clean_pdf = crate::report_export::compile_pdf_from_data(&clean.data_json)
        .await
        .expect("a clean package compiles");
    assert!(
        text_style::pdf_problems("report.pdf", &clean_pdf).is_empty(),
        "the shipped template must carry none of the characters in what it renders"
    );
    // The confidentiality line is printed at the foot of every page, so a character in it is in
    // the rendered text.
    let mut data: Value = serde_json::from_str(&clean.data_json).expect("data parses");
    let line = data["confidentiality"]["line"]
        .as_str()
        .unwrap_or("")
        .to_string();
    data["confidentiality"]["line"] = json!(format!("{line} {}", planted_text(EM)));
    let dirty_pdf = crate::report_export::compile_pdf_from_data(&data.to_string())
        .await
        .expect("compiles");
    let problems = text_style::pdf_problems("report.pdf", &dirty_pdf);
    assert!(
        problems.iter().any(|p| p.location.starts_with("page ")),
        "the extracted text carries the planted character: {problems:?}"
    );
}

// ---- the engine talking to itself in a headline -------------------------------------------------

#[test]
fn a_client_headline_that_opens_with_the_engines_own_note_is_refused() {
    for headline in [
        "This rule's own evaluation evidence reads \"x\", which appears to negate the rule.",
        "Borderline: the policy on messages is open.",
        "Needs review (uncited, no grounded citation found): Member documents are public.",
        "needs review: lower case counts too",
    ] {
        let mut pkg = package();
        let mut doc: Value = serde_json::from_slice(&pkg.findings_json).expect("parses");
        doc["findings"] = json!([{ "rule_id": "SEC-X-1", "headline": headline }]);
        pkg.findings_json = serde_json::to_vec_pretty(&doc).expect("serializes");
        let failure = failure_of(pkg);
        let problems = style_problems(&failure.report);
        assert!(
            problems
                .iter()
                .any(|p| p.file == "findings.json" && p.location == "$.findings[0].headline"),
            "{headline}: {problems:?}"
        );
    }
}

#[test]
fn a_rule_id_standing_as_its_own_headline_is_refused() {
    let mut pkg = package();
    let mut data: Value = serde_json::from_str(&pkg.data_json).expect("data parses");
    data["a_group_added_tomorrow"] = json!({
        "rule_id": "AI-PUBLIC-STORAGE-BUCKET",
        "headline": "AI-PUBLIC-STORAGE-BUCKET: AI-PUBLIC-STORAGE-BUCKET (1 site)",
    });
    pkg.data_json = data.to_string();
    let failure = failure_of(pkg);
    let problems = style_problems(&failure.report);
    assert!(
        problems
            .iter()
            .any(|p| p.location.ends_with(".headline") && p.message.contains("rule id")),
        "{problems:?}"
    );
}

#[test]
fn a_plain_headline_passes() {
    let mut pkg = package();
    let mut doc: Value = serde_json::from_slice(&pkg.findings_json).expect("parses");
    doc["findings"] = json!([{ "rule_id": "SEC-X-1", "headline": "The messages table is readable by everyone." }]);
    pkg.findings_json = serde_json::to_vec_pretty(&doc).expect("serializes");
    let failure = failure_of(pkg);
    assert!(
        style_problems(&failure.report)
            .iter()
            .all(|p| !p.location.ends_with(".headline")),
        "a plain headline is not a text-style problem"
    );
}

// ---- "whole file", never "line 0" ---------------------------------------------------------------

#[test]
fn a_whole_file_problem_printed_as_line_zero_is_refused_in_every_kind_of_file() {
    for text in [
        "supabase-portal-fixture / .env.production, line 0",
        "`.env.production:0` is marked as an outlier",
        "SEC-NO-SECRET-FILE-1 @ .env.production:0",
    ] {
        for surface in [
            Surface::DataJson,
            Surface::FindingsJson,
            Surface::DatasetJson,
            Surface::DatasetCsv,
            Surface::XlsxSharedString,
            Surface::Readme,
        ] {
            let mut pkg = package();
            plant(&mut pkg, surface, text);
            let failure = failure_of(pkg);
            let problems = style_problems(&failure.report);
            assert!(
                problems.iter().any(|p| p.message.contains("whole file")),
                "{surface:?} {text:?}: {problems:?}"
            );
        }
    }
}

#[test]
fn a_real_line_number_and_a_time_are_not_a_whole_file_problem() {
    for text in [
        "src/main.rs:10 and line 10 and line 01 and 10:05 and a 1:0 ratio",
        "baseline 0 and outline 0 are not places",
    ] {
        let mut pkg = package();
        plant(&mut pkg, Surface::Readme, text);
        let released = release(pkg);
        let report = match &released {
            Ok(z) => &z.report,
            Err(ReleaseError::Blocked(f)) => &f.report,
            Err(other) => panic!("{other}"),
        };
        let readme_problems: Vec<_> = style_problems(report)
            .into_iter()
            .filter(|p| p.file == "README.txt")
            .collect();
        assert!(readme_problems.is_empty(), "{text}: {readme_problems:?}");
    }
}

#[test]
fn an_internal_package_is_not_held_to_the_client_text_style() {
    // The operator's own review package carries engine notes and discarded rows verbatim; it is
    // not a client file. The same planted text refuses a client package.
    let mut internal = internal_package();
    plant(&mut internal, Surface::Readme, &planted_text(EM));
    let released = release_internal(internal).expect("an internal package is not a client file");
    assert!(style_problems(&released.report).is_empty());
    let mut client = package();
    plant(&mut client, Surface::Readme, &planted_text(EM));
    assert!(!style_problems(&failure_of(client).report).is_empty());
}
