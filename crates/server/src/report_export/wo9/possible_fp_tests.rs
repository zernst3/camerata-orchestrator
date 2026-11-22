//! The Possible false positive flag in the packages: an internal package carries it (a column in
//! the workbook's technical band, a key in `json/inspection.json`, a count on Start here); a client
//! package carries none of it; and the flag changes no other field of any row. Synthetic data.

use std::collections::BTreeMap;

use camerata_api_types::decisions::DecisionState;
use camerata_api_types::review::FpReason;

use super::support::*;
use crate::release_gate::{read_zip_entries, release, release_internal, xlsx_strings};
use crate::report_export::{apply_overlay, ReportOptions};
use crate::xlsx_export::vocabulary as v;

/// Two SQL rows in production code (a security rule nobody doubts) and one in a test file.
fn sealed_with_a_test_row() -> crate::onboard::SealedReport {
    let mut report = crate::release_gate::test_scan_report();
    let mut in_test = sql_row("tests/orders_test.rs", 9);
    in_test.in_test = true;
    report.findings = vec![sql_row("src/a.rs", 10), sql_row("src/b.rs", 20), in_test];
    report.seal()
}

fn internal_options() -> ReportOptions {
    ReportOptions {
        internal: true,
        ..client_options()
    }
}

fn internal_entries(reviewed: &crate::report_export::ReviewedReport) -> BTreeMap<String, Vec<u8>> {
    let zip = release_internal(package_of(reviewed, &internal_options()))
        .expect("an internal package clears")
        .bytes;
    read_zip_entries(&zip).expect("zip")
}

fn workbook(entries: &BTreeMap<String, Vec<u8>>) -> Vec<u8> {
    entries
        .iter()
        .find(|(n, _)| n.ends_with(".xlsx"))
        .map(|(_, b)| b.clone())
        .expect("a workbook")
}

fn findings_rows(entries: &BTreeMap<String, Vec<u8>>) -> Vec<serde_json::Value> {
    let doc: serde_json::Value = serde_json::from_slice(&entries["json/inspection.json"])
        .expect("json/inspection.json parses");
    doc["findings"].as_array().expect("rows").clone()
}

fn start_here_line(xlsx: &[u8]) -> Option<String> {
    crate::release_gate::xlsx_sheet_grid(xlsx, v::START_HERE)
        .expect("Start here")
        .into_iter()
        .find(|r| r.first().map(String::as_str) == Some(v::P_POSSIBLE_FALSE_POSITIVES))
        .map(|r| r[1].clone())
}

#[test]
fn a_client_package_carries_no_flag_no_reason_and_no_count() {
    let sealed = sealed_with_a_test_row();
    let reviewed = apply_overlay(&sealed, &[], &[], &[]);
    let package = package_of(&reviewed, &client_options());
    let data = package.data_json.clone();
    let entries = read_zip_entries(
        &release(package)
            .expect("a flagged row does not hold a client export")
            .bytes,
    )
    .expect("zip");
    let mut text = data;
    for (name, bytes) in &entries {
        if name.ends_with(".xlsx") {
            text.push_str(&xlsx_strings(bytes).expect("strings").join("\n"));
        } else if !name.ends_with(".pdf") {
            // A .pdf is skipped: only `review_view` and `xlsx_export` reference this index, and
            // neither draws into the PDF, which builds from the report JSON.
            text.push_str(&String::from_utf8_lossy(bytes));
        }
    }
    let lower = text.to_lowercase();
    for needle in ["possible_false_positive", "possible false positive"] {
        assert!(!lower.contains(needle), "a client file carries {needle:?}");
    }
    for reason in FpReason::ALL {
        assert!(
            !text.contains(reason.as_str()),
            "a client file carries {reason:?}"
        );
        assert!(
            !text.contains(reason.label()),
            "a client file carries {reason:?}"
        );
    }
    assert_eq!(start_here_line(&workbook(&entries)), None);
}

#[test]
fn an_internal_package_carries_the_flag_in_the_workbook_findings_json_and_start_here() {
    let sealed = sealed_with_a_test_row();
    let reviewed = apply_overlay(&sealed, &[], &[], &[]);
    let entries = internal_entries(&reviewed);
    // json/inspection.json: the key on the flagged row only, with its reasons in the closed vocabulary.
    let rows = findings_rows(&entries);
    let flagged: Vec<&serde_json::Value> = rows
        .iter()
        .filter(|r| r.get("possible_false_positive").is_some())
        .collect();
    assert_eq!(flagged.len(), 1, "{rows:#?}");
    assert_eq!(flagged[0]["path"], "tests/orders_test.rs");
    assert_eq!(
        flagged[0]["possible_false_positive"],
        serde_json::json!({"reasons": ["test_code_only"]})
    );
    // The workbook: a column in the technical band, the reasons by their short names.
    let xlsx = workbook(&entries);
    let grid = sheet_table(&xlsx, v::ALL_PROBLEMS);
    let col = grid[0]
        .iter()
        .position(|h| h == v::POSSIBLE_FALSE_POSITIVE)
        .expect("the internal column");
    let path_col = grid[0].iter().position(|h| h == v::H_FILE).expect("File");
    for row in &grid[1..] {
        let want = if row[path_col] == "tests/orders_test.rs" {
            FpReason::TestCodeOnly.label()
        } else {
            ""
        };
        assert_eq!(row[col], want, "{row:?}");
    }
    // The internal Glossary defines the flag and every reason in the words the column uses.
    let glossary = crate::release_gate::xlsx_sheet_grid(&xlsx, v::GLOSSARY).expect("Glossary");
    let terms: Vec<&str> = glossary.iter().map(|r| r[0].as_str()).collect();
    assert!(terms.contains(&v::POSSIBLE_FALSE_POSITIVE), "{terms:?}");
    for reason in FpReason::ALL {
        assert!(
            terms.contains(&reason.label()),
            "{reason:?} is not in {terms:?}"
        );
    }
    // Start here: how many are flagged and how many are decided.
    assert_eq!(
        start_here_line(&xlsx).as_deref(),
        Some("1 row flagged as a possible false positive, 0 decided")
    );
}

#[test]
fn the_start_here_count_counts_a_ruled_out_flagged_row_as_decided() {
    let sealed = sealed_with_a_test_row();
    let key = key_at(&sealed, "tests/orders_test.rs");
    let records = vec![decision(
        "dec-000001",
        &key,
        DecisionState::FalsePositive,
        "test helper, not production code",
    )];
    let reviewed = apply_overlay(&sealed, &records, &[], &[]);
    let entries = internal_entries(&reviewed);
    assert_eq!(
        start_here_line(&workbook(&entries)).as_deref(),
        Some("1 row flagged as a possible false positive, 1 decided")
    );
}

#[test]
fn the_flag_changes_no_severity_kind_escalation_row_count_or_decision() {
    let sealed = sealed_with_a_test_row();
    let records = vec![decision(
        "dec-000001",
        &key_at(&sealed, "src/a.rs"),
        DecisionState::Confirmed,
        "real",
    )];
    let reviewed = apply_overlay(&sealed, &records, &[], &[]);
    let client = released_entries(&reviewed, &client_options());
    let internal = internal_entries(&reviewed);
    let by_place = |rows: Vec<serde_json::Value>| -> BTreeMap<String, serde_json::Value> {
        rows.into_iter()
            .map(|mut r| {
                let obj = r.as_object_mut().expect("a row object");
                // The two keys only an internal package carries.
                obj.remove("possible_false_positive");
                obj.remove("escalation_reason");
                (obj["site_id"].as_str().expect("site_id").to_string(), r)
            })
            .collect()
    };
    let client_rows = by_place(findings_rows(&client));
    let internal_rows = by_place(findings_rows(&internal));
    assert_eq!(client_rows.len(), 3, "every row ships");
    assert_eq!(
        client_rows, internal_rows,
        "apart from the internal keys every row is the same: severity, kind, escalation, decision"
    );
    // The same rows on All problems, with the same severities.
    let rows_of = |entries: &BTreeMap<String, Vec<u8>>| {
        let grid = sheet_table(&workbook(entries), v::ALL_PROBLEMS);
        let sev = grid[0].iter().position(|h| h == v::H_SEVERITY).unwrap();
        let file = grid[0].iter().position(|h| h == v::H_FILE).unwrap();
        let mut out: Vec<(String, String)> = grid[1..]
            .iter()
            .map(|r| (r[file].clone(), r[sev].clone()))
            .collect();
        out.sort();
        out
    };
    assert_eq!(rows_of(&client), rows_of(&internal));
}

const UNSURE_MARKER: &str = "UNSURE-WHY-MARKER";
const DOUBT_MARKER: &str = "DOUBT-WHY-MARKER";

/// One finder row that is unsure with a sentence and one the group reviewer doubts with a why.
fn sealed_with_row_sentences() -> crate::onboard::SealedReport {
    use crate::ai_audit::agent_finder::reply::{
        FINDER_PIECE_CAPTURE, FINDER_PROBLEM_CAPTURE, FINDER_SURE_CAPTURE,
        FINDER_UNSURE_WHY_CAPTURE,
    };
    use camerata_api_types::review::{DisagreementField, Stage2Disagreement};
    let mut report = crate::release_gate::test_scan_report();
    let mut unsure = sql_row("src/a.rs", 10);
    for (k, v) in [
        (FINDER_PROBLEM_CAPTURE, "1"),
        (FINDER_PIECE_CAPTURE, "p"),
        (FINDER_SURE_CAPTURE, "unsure"),
        (FINDER_UNSURE_WHY_CAPTURE, UNSURE_MARKER),
    ] {
        unsure.captures.insert(k.to_string(), v.to_string());
    }
    let mut doubted = sql_row("src/b.rs", 20);
    doubted.review.stage2_disagreement.push(Stage2Disagreement {
        field: DisagreementField::Real,
        stage1: "yes".to_string(),
        mine: "no".to_string(),
        why: DOUBT_MARKER.to_string(),
    });
    report.findings = vec![unsure, doubted];
    report.seal()
}

#[test]
fn a_client_package_carries_neither_row_sentence() {
    let reviewed = apply_overlay(&sealed_with_row_sentences(), &[], &[], &[]);
    let package = package_of(&reviewed, &client_options());
    let mut text = package.data_json.clone();
    let entries = read_zip_entries(&release(package).expect("releases").bytes).expect("zip");
    for (name, bytes) in &entries {
        if name.ends_with(".xlsx") {
            text.push_str(&xlsx_strings(bytes).expect("strings").join("\n"));
        } else if !name.ends_with(".pdf") {
            text.push_str(&String::from_utf8_lossy(bytes));
        }
    }
    assert!(!text.contains(UNSURE_MARKER) && !text.contains(DOUBT_MARKER));
}

#[test]
fn the_internal_workbook_cell_names_the_reason_then_the_row_sentence() {
    let reviewed = apply_overlay(&sealed_with_row_sentences(), &[], &[], &[]);
    let entries = internal_entries(&reviewed);
    let xlsx = workbook(&entries);
    let grid = sheet_table(&xlsx, v::ALL_PROBLEMS);
    let col = grid[0]
        .iter()
        .position(|h| h == v::POSSIBLE_FALSE_POSITIVE)
        .expect("column");
    let file = grid[0].iter().position(|h| h == v::H_FILE).expect("File");
    let cell_of = |path: &str| grid[1..].iter().find(|r| r[file] == path).unwrap()[col].clone();
    assert_eq!(
        cell_of("src/a.rs"),
        format!("{}: {UNSURE_MARKER}", FpReason::FinderUnsure.label())
    );
    assert_eq!(
        cell_of("src/b.rs"),
        format!("{}: {DOUBT_MARKER}", FpReason::GroupReviewerDoubt.label())
    );
}

/// The second look at rows ruled out while flagged is the Engine Check's: internal, in the
/// review lane only. Neither package carries its stratum, its labels or its card question, even
/// with a flagged row ruled out.
#[test]
fn neither_package_carries_the_second_look_at_ruled_out_flagged_rows() {
    let sealed = sealed_with_a_test_row();
    let key = key_at(&sealed, "tests/orders_test.rs");
    let records = vec![decision(
        "dec-000001",
        &key,
        DecisionState::FalsePositive,
        "test helper, not production code",
    )];
    let reviewed = apply_overlay(&sealed, &records, &[], &[]);
    for (internal, entries) in [
        (false, released_entries(&reviewed, &client_options())),
        (true, internal_entries(&reviewed)),
    ] {
        let mut text = String::new();
        for (name, bytes) in &entries {
            if name.ends_with(".xlsx") {
                text.push_str(&xlsx_strings(bytes).expect("strings").join("\n"));
            } else if !name.ends_with(".pdf") {
                text.push_str(&String::from_utf8_lossy(bytes));
            }
        }
        let lower = text.to_lowercase();
        for needle in [
            "ruled_out_flagged",
            "ruled out while flagged",
            "second look",
            "was it right to rule this out",
        ] {
            assert!(
                !lower.contains(needle),
                "internal={internal}: a package file carries {needle:?}"
            );
        }
    }
}
