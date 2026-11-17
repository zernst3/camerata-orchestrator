//! The Possible duplicate hint in the packages: an internal package carries it (a column in the
//! workbook's technical band, a key in `findings.json`, a count on Start here); a client package
//! carries none of it; and the hint changes no other field of any row. Synthetic data.

use std::collections::BTreeMap;

use camerata_api_types::decisions::DecisionState;

use super::support::*;
use crate::release_gate::{read_zip_entries, release, release_internal, xlsx_strings};
use crate::report_export::{apply_overlay, ReportOptions};
use crate::xlsx_export::vocabulary as v;

/// A SQL row and a layering row two lines apart in one file (two causes at one place), and a SQL
/// row alone in another file.
fn sealed_with_a_pair() -> crate::onboard::SealedReport {
    let mut report = crate::release_gate::test_scan_report();
    report.findings = vec![
        sql_row("src/orders.rs", 10),
        layering_row("src/orders.rs", 12),
        sql_row("src/alone.rs", 40),
    ];
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
    let doc: serde_json::Value =
        serde_json::from_slice(&entries["findings.json"]).expect("findings.json parses");
    doc["findings"].as_array().expect("rows").clone()
}

fn start_here_line(xlsx: &[u8]) -> Option<String> {
    crate::release_gate::xlsx_sheet_grid(xlsx, v::START_HERE)
        .expect("Start here")
        .into_iter()
        .find(|r| r.first().map(String::as_str) == Some(v::P_POSSIBLE_DUPLICATES))
        .map(|r| r[1].clone())
}

fn fix_id_of_rule(sealed: &crate::onboard::SealedReport, rule: &str, path: &str) -> String {
    let f = sealed
        .findings
        .iter()
        .find(|f| f.rule_id == rule && f.path == path)
        .expect("a row");
    sealed
        .tree
        .as_ref()
        .and_then(|t| t.fix_of(&f.group_path[2]))
        .map(|fx| fx.id.clone())
        .expect("a Fix")
}

#[test]
fn a_client_package_carries_no_hint_no_term_and_no_count() {
    let sealed = sealed_with_a_pair();
    let reviewed = apply_overlay(&sealed, &[], &[], &[]);
    let package = package_of(&reviewed, &client_options());
    let mut text = package.data_json.clone();
    let entries = read_zip_entries(
        &release(package)
            .expect("a hinted row does not hold a client export")
            .bytes,
    )
    .expect("zip");
    for (name, bytes) in &entries {
        if name.ends_with(".xlsx") {
            text.push_str(&xlsx_strings(bytes).expect("strings").join("\n"));
        } else if !name.ends_with(".pdf") {
            text.push_str(&String::from_utf8_lossy(bytes));
        }
    }
    let lower = text.to_lowercase();
    for needle in [
        "possible_duplicate_of",
        "possible duplicate",
        "may duplicate another",
    ] {
        assert!(!lower.contains(needle), "a client file carries {needle:?}");
    }
    assert_eq!(start_here_line(&workbook(&entries)), None);
    // The client's own Glossary defines no such term.
    for e in v::GLOSSARY_ENTRIES {
        assert_ne!(e.term, v::POSSIBLE_DUPLICATE);
    }
}

#[test]
fn an_internal_package_carries_the_hint_in_the_workbook_findings_json_and_start_here() {
    let sealed = sealed_with_a_pair();
    let layering_fix = fix_id_of_rule(&sealed, "ARCH-LAYERING-1", "src/orders.rs");
    let orders_sql_fix = fix_id_of_rule(&sealed, "SEC-NO-RAW-SQL-CONCAT-1", "src/orders.rs");
    assert_ne!(layering_fix, orders_sql_fix, "two causes");
    let reviewed = apply_overlay(&sealed, &[], &[], &[]);
    let entries = internal_entries(&reviewed);
    // findings.json: the key on the two paired rows only, naming the other cause's Fix id.
    let rows = findings_rows(&entries);
    let hinted: Vec<&serde_json::Value> = rows
        .iter()
        .filter(|r| r.get("possible_duplicate_of").is_some())
        .collect();
    assert_eq!(hinted.len(), 2, "{rows:#?}");
    for r in &hinted {
        let ids = r["possible_duplicate_of"].as_array().unwrap();
        assert_eq!(ids.len(), 1);
        if r["rule_id"] == "ARCH-LAYERING-1" {
            assert_eq!(ids[0], orders_sql_fix.as_str());
        } else {
            assert_eq!(ids[0], layering_fix.as_str());
        }
    }
    assert!(rows
        .iter()
        .filter(|r| r["path"] == "src/alone.rs")
        .all(|r| r.get("possible_duplicate_of").is_none()));
    // The workbook: a column in the technical band.
    let xlsx = workbook(&entries);
    let grid = sheet_table(&xlsx, v::ALL_PROBLEMS);
    let col = grid[0]
        .iter()
        .position(|h| h == v::POSSIBLE_DUPLICATE_OF)
        .expect("the internal column");
    let file = grid[0].iter().position(|h| h == v::H_FILE).expect("File");
    let hinted_cells: Vec<&str> = grid[1..]
        .iter()
        .filter(|r| r[file] == "src/orders.rs")
        .map(|r| r[col].as_str())
        .collect();
    assert!(hinted_cells.iter().all(|c| !c.is_empty()), "{hinted_cells:?}");
    assert!(grid[1..]
        .iter()
        .filter(|r| r[file] == "src/alone.rs")
        .all(|r| r[col].is_empty()));
    // The internal Glossary defines the term.
    let glossary = crate::release_gate::xlsx_sheet_grid(&xlsx, v::GLOSSARY).expect("Glossary");
    assert!(glossary.iter().any(|r| r[0] == v::POSSIBLE_DUPLICATE));
    // Start here: how many fixes may duplicate another and how many are decided.
    assert_eq!(
        start_here_line(&xlsx).as_deref(),
        Some("2 fixes may duplicate another, 0 decided")
    );
}

#[test]
fn the_hint_changes_no_severity_kind_escalation_row_count_or_decision() {
    let sealed = sealed_with_a_pair();
    let records = vec![decision(
        "dec-000001",
        &key_at(&sealed, "src/alone.rs"),
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
                // The keys only an internal package carries.
                obj.remove("possible_duplicate_of");
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
        "apart from the internal keys every row is the same, in the same order"
    );
    let order = |rows: Vec<serde_json::Value>| -> Vec<String> {
        rows.iter().map(|r| r["site_id"].as_str().unwrap().to_string()).collect()
    };
    assert_eq!(order(findings_rows(&client)), order(findings_rows(&internal)));
    let rows_of = |entries: &BTreeMap<String, Vec<u8>>| {
        let grid = sheet_table(&workbook(entries), v::ALL_PROBLEMS);
        let sev = grid[0].iter().position(|h| h == v::H_SEVERITY).unwrap();
        let file = grid[0].iter().position(|h| h == v::H_FILE).unwrap();
        grid[1..]
            .iter()
            .map(|r| (r[file].clone(), r[sev].clone()))
            .collect::<Vec<_>>()
    };
    assert_eq!(rows_of(&client), rows_of(&internal));
}

#[test]
fn the_start_here_count_counts_a_ruled_out_hinted_fix_as_decided() {
    let sealed = sealed_with_a_pair();
    let layering = sealed
        .findings
        .iter()
        .find(|f| f.rule_id == "ARCH-LAYERING-1")
        .unwrap()
        .site_key
        .clone();
    let records = vec![decision(
        "dec-000001",
        &layering,
        DecisionState::FalsePositive,
        "the same finding as the SQL row",
    )];
    let reviewed = apply_overlay(&sealed, &records, &[], &[]);
    let entries = internal_entries(&reviewed);
    assert_eq!(
        start_here_line(&workbook(&entries)).as_deref(),
        Some("2 fixes may duplicate another, 1 decided")
    );
}
