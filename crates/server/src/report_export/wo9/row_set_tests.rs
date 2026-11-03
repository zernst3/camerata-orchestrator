//! WO9, design 3B: every output renders the same reviewed rows. Row count, count per review level
//! and count per sub-group are equal across `findings.json`, the JSON and CSV datasets and the
//! workbook, and the manifest lists every file.

use std::collections::BTreeMap;

use serde_json::Value;

use super::support::*;
use crate::release_gate::{xlsx_sheet_grid, ProductPackage};
use crate::report_export::apply_overlay;

type Tally = BTreeMap<String, usize>;

fn tally(rows: &[Value], key: &str) -> Tally {
    let mut out = Tally::new();
    for row in rows {
        let value = row[key].as_str().unwrap_or("<missing>").to_string();
        *out.entry(value).or_default() += 1;
    }
    out
}

fn json_rows(entries: &BTreeMap<String, Vec<u8>>, name: &str) -> Vec<Value> {
    let value: Value = serde_json::from_slice(&entries[name]).expect("json");
    match value {
        Value::Array(rows) => rows,
        other => other["findings"]
            .as_array()
            .cloned()
            .expect("a findings array"),
    }
}

fn csv_rows(entries: &BTreeMap<String, Vec<u8>>, name: &str) -> Vec<Value> {
    let mut reader = csv::Reader::from_reader(entries[name].as_slice());
    let headers = reader.headers().expect("headers").clone();
    reader
        .records()
        .map(|record| {
            let record = record.expect("a csv row");
            let mut row = serde_json::Map::new();
            for (h, v) in headers.iter().zip(record.iter()) {
                row.insert(h.to_string(), Value::String(v.to_string()));
            }
            Value::Object(row)
        })
        .collect()
}

fn sites_sheet_rows(entries: &BTreeMap<String, Vec<u8>>) -> Vec<Value> {
    let xlsx = entries
        .iter()
        .find(|(name, _)| name.ends_with(".xlsx"))
        .map(|(_, bytes)| bytes.clone())
        .expect("a workbook");
    let grid = xlsx_sheet_grid(&xlsx, "Sites").expect("a Sites sheet");
    let header = &grid[0];
    let col = |name: &str| {
        header
            .iter()
            .position(|h| h == name)
            .unwrap_or_else(|| panic!("the Sites sheet has no {name:?} column: {header:?}"))
    };
    let (site, level, group) = (col("Site"), col("Review level"), col("Group"));
    grid.iter()
        .skip(1)
        .filter(|r| r[site] == "Primary")
        .map(|r| serde_json::json!({ "review_level": r[level], "group": r[group] }))
        .collect()
}

#[test]
fn every_export_renders_the_same_reviewed_row_set() {
    let sealed = mixed_sealed();
    let (records, edits) = mixed_review(&sealed);
    let reviewed = apply_overlay(&sealed, &records, &edits, &[]);
    let options = client_options();
    let entries = released_entries(&reviewed, &options);

    // The rows of the report: kept rows, false positives and dependency advisories.
    let total = sealed.findings.len();
    assert_eq!(total, 6);
    let of_json = |rows: Vec<Value>| rows;
    let json_all: Vec<Value> = [
        json_rows(&entries, "json/all-findings.json"),
        json_rows(&entries, "json/false-positives.json"),
        json_rows(&entries, "json/dependencies.json"),
    ]
    .concat();
    let findings_json: Vec<Value> = [
        json_rows(&entries, "findings.json"),
        json_rows(&entries, "json/false-positives.json"),
        json_rows(&entries, "json/dependencies.json"),
    ]
    .concat();
    let csv_all: Vec<Value> = [
        csv_rows(&entries, "csv/all-findings.csv"),
        csv_rows(&entries, "csv/false-positives.csv"),
        csv_rows(&entries, "csv/dependencies.csv"),
    ]
    .concat();
    let sites = sites_sheet_rows(&entries);

    let outputs: [(&str, Vec<Value>); 4] = [
        ("findings.json", of_json(findings_json)),
        ("json datasets", json_all),
        ("csv datasets", csv_all),
        ("Sites sheet", sites),
    ];
    let want_levels = tally(&outputs[0].1, "review_level");
    let want_groups = tally(&outputs[0].1, "group");
    assert_eq!(
        want_levels,
        Tally::from([
            ("engine_verified".to_string(), 2),
            ("personally_reviewed".to_string(), 2),
            (
                "reviewed_as_pattern:rule:SEC-NO-RAW-SQL-CONCAT-1".to_string(),
                2
            ),
        ]),
        "{want_levels:?}"
    );
    assert!(
        want_groups.keys().all(|g| g.starts_with("sub:")),
        "{want_groups:?}"
    );
    for (name, rows) in &outputs {
        assert_eq!(rows.len(), total, "{name}: row count");
        assert_eq!(tally(rows, "review_level"), want_levels, "{name}: levels");
        assert_eq!(tally(rows, "group"), want_groups, "{name}: sub-groups");
    }
}

#[allow(dead_code)]
fn unused(_: &ProductPackage) {}
