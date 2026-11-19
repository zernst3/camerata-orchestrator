//! The machine-readable files of a real package: a header row on every CSV, and the list and
//! number fields beside the prose ones in `findings.json` and the finding datasets.

use serde_json::Value;

use super::tests::{mixed_package, package};
use super::*;
use crate::machine_files::columns_of;

fn json_rows(bytes: &[u8]) -> Vec<Value> {
    serde_json::from_slice::<Value>(bytes)
        .expect("a dataset parses")
        .as_array()
        .cloned()
        .expect("a dataset is an array")
}

fn dataset<'a>(package: &'a ProductPackage, path: &str) -> &'a [u8] {
    &package
        .datasets
        .iter()
        .find(|(p, _)| p == path)
        .unwrap_or_else(|| panic!("no {path}"))
        .1
}

fn csv_headers(bytes: &[u8]) -> Vec<String> {
    csv::Reader::from_reader(bytes)
        .headers()
        .expect("a header row")
        .iter()
        .map(String::from)
        .collect()
}

#[test]
fn every_csv_of_a_package_has_a_real_header_row_even_with_no_rows() {
    for package in [package(), mixed_package()] {
        let mut seen_empty = 0;
        for name in crate::xlsx_export::DATASETS {
            let headers = csv_headers(dataset(&package, &format!("csv/{name}.csv")));
            assert!(
                headers.len() > 1 && headers.iter().all(|h| !h.is_empty()),
                "csv/{name}.csv: {headers:?}"
            );
            let rows = json_rows(dataset(&package, &format!("json/{name}.json")));
            if rows.is_empty() {
                seen_empty += 1;
                assert_eq!(
                    headers,
                    columns_of(name)
                        .expect("a known dataset")
                        .iter()
                        .map(|c| c.to_string())
                        .collect::<Vec<_>>(),
                    "csv/{name}.csv"
                );
            }
        }
        assert!(seen_empty > 0, "the fixture must include an empty dataset");
    }
}

#[test]
fn the_tickets_file_keeps_its_header() {
    let package = package();
    let headers = csv_headers(dataset(&package, "csv/tickets.csv"));
    assert!(headers.contains(&"Title".to_string()), "{headers:?}");
}

#[test]
fn the_column_lists_agree_with_the_rows_the_builders_write() {
    // Every key of every row is one of the dataset's columns. (A row's keys may come in a
    // different order than the union the CSV header uses, because a deviation or a convention
    // writes its payload keys beside `kind`, so order is not part of the agreement.)
    let package = mixed_package();
    let mut checked = 0;
    for name in crate::xlsx_export::DATASETS {
        let rows = json_rows(dataset(&package, &format!("json/{name}.json")));
        let Some(columns) = columns_of(name) else {
            continue;
        };
        for row in &rows {
            for key in row.as_object().expect("a row").keys() {
                assert!(
                    columns.contains(&key.as_str()),
                    "{name}: column {key:?} is not in columns_of({name:?})"
                );
            }
            checked += 1;
        }
    }
    assert!(
        checked > 5,
        "the fixture must write rows in several datasets"
    );
}

#[test]
fn findings_json_and_the_finding_datasets_carry_the_list_and_number_fields() {
    let package = mixed_package();
    let doc: Value = serde_json::from_slice(&package.findings_json).expect("findings.json");
    let findings = doc["findings"].as_array().expect("rows");
    assert!(!findings.is_empty());
    for row in findings {
        assert!(row["citation_url_list"].is_array(), "{row}");
        let urls = row["citation_urls"].as_str().unwrap_or("");
        let listed: Vec<&str> = row["citation_url_list"]
            .as_array()
            .expect("a list")
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert_eq!(
            listed,
            urls.lines().filter(|u| !u.is_empty()).collect::<Vec<_>>()
        );
        // The numbers are numbers or null, and the prose is still there.
        for key in ["est_hours_min", "est_hours_max"] {
            assert!(row[key].is_null() || row[key].is_number(), "{key} in {row}");
        }
        assert!(row["est_hours"].is_string(), "{row}");
        assert_eq!(
            row["est_hours_min"].is_null(),
            row["est_hours"] == "not yet estimated",
            "{row}"
        );
    }
    // The dataset carries the same rows, and the CSV the same columns.
    assert_eq!(
        json_rows(dataset(&package, "json/all-findings.json")),
        findings.to_vec()
    );
    let headers = csv_headers(dataset(&package, "csv/all-findings.csv"));
    for key in ["citation_url_list", "est_hours_min", "est_hours_max"] {
        assert!(headers.contains(&key.to_string()), "{headers:?}");
    }
}

#[test]
fn the_gate_still_holds_the_csv_to_the_json_after_the_added_fields() {
    let released = release(mixed_package()).expect("the package clears the gate");
    assert!(released.report.is_clean());
}
