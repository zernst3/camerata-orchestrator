//! The machine-readable files of a package, finished for the people who script against them.
//!
//! Two jobs, both additive and neither removing a field:
//!
//! * **Numbers and lists beside prose.** A row of `findings.json` and of `json/all-findings.json`
//!   (and the same columns of `csv/all-findings.csv`) carries `citation_urls` as one text with a
//!   line break between the URLs and `est_hours` as a sentence ("1 to 2 days (about 8 to 16
//!   hours)"). Beside them it now carries `citation_url_list`, the URLs as an array, and
//!   `est_hours_min` and `est_hours_max`, the bounds as numbers (null when the effort was never
//!   estimated). The prose fields are unchanged.
//! * **A header row on every CSV.** An empty dataset used to write a header of one empty name
//!   (`""`), which a spreadsheet opens as one blank column. It now writes the dataset's real
//!   column names, so an importer sees the same columns whether or not the inspection found
//!   anything. A dataset with rows keeps the header it has always had.
//!
//! [`finish`] works on the bytes the workbook builders produced, in the assembly
//! (`release_gate::assemble_package`), so `findings.json`, `json/` and `csv/` are changed the same
//! way and the gate's parity checks still hold them equal.

use anyhow::Context;
use serde_json::{json, Map, Value};

/// The rows a FindingRow-shaped dataset holds, and so the datasets that get the added fields.
const FINDING_DATASETS: [&str; 2] = ["all-findings", "false-positives"];

/// The columns of `all-findings` and `false-positives` (one row shape), in the order the rows
/// write them. A row of a deviation, a convention, a missing convention or a rule that could not be
/// assessed adds its own payload columns at the end.
const FINDING_COLUMNS: [&str; 54] = [
    "severity",
    "headline",
    "repo",
    "path",
    "line",
    "rule_id",
    "category",
    "bucket",
    "disposition_kind",
    "disposition_label",
    "effort",
    "est_hours",
    "est_hours_min",
    "est_hours_max",
    "confidence",
    "needs_review",
    "in_test",
    "provenance",
    "citation_label",
    "citation_urls",
    "citation_url_list",
    "also_matches",
    "status",
    "snippet",
    "detail",
    "fix_specific",
    "fix",
    "fix_type",
    "is_fp",
    "fp_reason",
    "also_locations",
    "flagged_by",
    "site_count",
    "kind",
    "severity_adjustments",
    "carried_from",
    "reviewer_edited",
    "edit_ids",
    "site_id",
    "review_level",
    "group",
    "fix_id",
    "sub_group",
    "sub_group_cause",
    "escalated",
    "merged_into",
    "carried_by",
    "deviating_sites",
    "assessed_sites",
    "convention_option_id",
    "convention_label",
    "observed_options",
    "reason",
    "reason_detail",
];

/// The columns each dataset's CSV carries when it has no rows: the keys its rows write, in the
/// same order. A test holds each list to the rows the builders really write.
pub(crate) fn columns_of(dataset: &str) -> Option<Vec<&'static str>> {
    let columns: &[&str] = match dataset {
        "all-findings" | "false-positives" => return Some(FINDING_COLUMNS.to_vec()),
        "deviations" => &[
            "category",
            "rule_title",
            "rule_id",
            "convention_label",
            "deviating_sites",
            "assessed_sites",
            "worst_severity",
            "locations",
            "recommended_fix",
            "fix_type",
        ],
        "conventions" => &[
            "category",
            "rule_title",
            "rule_id",
            "status",
            "convention_label",
            "patterns_in_use",
            "headline",
            "detail",
            "sites",
            "also_found_at",
        ],
        "not-assessed" => &["rule_title", "rule_id", "why", "headline", "detail"],
        "dependencies" => &[
            "package",
            "path",
            "site_id",
            "review_level",
            "group",
            "fix_id",
            "sub_group",
            "sub_group_cause",
            "escalated",
            "merged_into",
            "carried_by",
            "reviewer_edited",
            "edit_ids",
            "advisory",
            "severity",
            "repo",
            "fixed_version",
            "ownership",
            "action",
        ],
        "coverage" => &[
            "rule_id",
            "title",
            "category",
            "citation_kind",
            "citation_label",
            "findings_count",
        ],
        "rules-applied" => &["rule_id", "chosen", "evidence", "applicable"],
        _ => return None,
    };
    Some(columns.to_vec())
}

/// `citation_urls` (one text, a line break between URLs) as a list.
fn url_list(urls: &str) -> Vec<String> {
    urls.lines()
        .map(str::trim)
        .filter(|u| !u.is_empty())
        .map(str::to_string)
        .collect()
}

/// The numbers beside a row's prose: `(citation_url_list, est_hours_min, est_hours_max)`.
fn added_values(urls: &str, effort: &str) -> (Value, Value, Value) {
    let bounds =
        crate::report_export::effort_hours_bounds(Some(effort).filter(|e| !e.is_empty())).0;
    (
        json!(url_list(urls)),
        bounds.map_or(Value::Null, |(low, _)| json!(low)),
        bounds.map_or(Value::Null, |(_, high)| json!(high)),
    )
}

/// One row with the added fields put beside the fields they describe. A row that already has
/// them is returned as it is.
fn with_added_fields(row: &Map<String, Value>) -> Map<String, Value> {
    if row.contains_key("citation_url_list") {
        return row.clone();
    }
    let text = |key: &str| row.get(key).and_then(Value::as_str).unwrap_or("");
    let (list, low, high) = added_values(text("citation_urls"), text("effort"));
    let mut out = Map::new();
    for (key, value) in row {
        out.insert(key.clone(), value.clone());
        match key.as_str() {
            "est_hours" => {
                out.insert("est_hours_min".to_string(), low.clone());
                out.insert("est_hours_max".to_string(), high.clone());
            }
            "citation_urls" => {
                out.insert("citation_url_list".to_string(), list.clone());
            }
            _ => {}
        }
    }
    out
}

fn with_added_fields_in_rows(rows: &[Value]) -> Vec<Value> {
    rows.iter()
        .map(|row| match row {
            Value::Object(map) => Value::Object(with_added_fields(map)),
            other => other.clone(),
        })
        .collect()
}

/// `findings.json` with the added fields on every row of its `findings` array.
fn finish_findings_json(bytes: &[u8]) -> anyhow::Result<Vec<u8>> {
    let mut doc: Value = serde_json::from_slice(bytes).context("findings.json does not parse")?;
    if let Some(rows) = doc.get("findings").and_then(Value::as_array) {
        let rows = with_added_fields_in_rows(rows);
        doc["findings"] = Value::Array(rows);
    }
    Ok(serde_json::to_vec_pretty(&doc)?)
}

/// A dataset's JSON with the added fields on every row.
fn finish_json(bytes: &[u8]) -> anyhow::Result<Vec<u8>> {
    let value: Value = serde_json::from_slice(bytes).context("a dataset does not parse")?;
    let rows = value.as_array().cloned().unwrap_or_default();
    Ok(serde_json::to_vec_pretty(&with_added_fields_in_rows(
        &rows,
    ))?)
}

/// The CSV cell of a JSON value, as the dataset writer renders it: text as it is, null as empty, a
/// nested list or object as compact JSON.
fn csv_cell(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

/// A dataset's CSV with the added columns beside the ones they describe.
fn finish_csv_rows(bytes: &[u8]) -> anyhow::Result<Vec<u8>> {
    let mut reader = csv::ReaderBuilder::new().flexible(true).from_reader(bytes);
    let headers = reader
        .headers()
        .context("a dataset CSV has no header")?
        .clone();
    if headers.iter().any(|h| h == "citation_url_list") {
        return Ok(bytes.to_vec());
    }
    let index_of = |name: &str| headers.iter().position(|h| h == name);
    let (urls_at, effort_at) = (index_of("citation_urls"), index_of("effort"));
    let mut writer = csv::WriterBuilder::new().from_writer(Vec::new());
    let mut new_headers: Vec<String> = Vec::new();
    for h in headers.iter() {
        new_headers.push(h.to_string());
        match h {
            "est_hours" => {
                new_headers.push("est_hours_min".to_string());
                new_headers.push("est_hours_max".to_string());
            }
            "citation_urls" => new_headers.push("citation_url_list".to_string()),
            _ => {}
        }
    }
    writer.write_record(&new_headers)?;
    for record in reader.records() {
        let record = record.context("a dataset CSV row is unreadable")?;
        let cell = |at: Option<usize>| at.and_then(|i| record.get(i)).unwrap_or("");
        let (list, low, high) = added_values(cell(urls_at), cell(effort_at));
        let mut out: Vec<String> = Vec::new();
        for (i, value) in record.iter().enumerate() {
            out.push(value.to_string());
            match headers.get(i) {
                Some("est_hours") => {
                    out.push(csv_cell(&low));
                    out.push(csv_cell(&high));
                }
                Some("citation_urls") => out.push(csv_cell(&list)),
                _ => {}
            }
        }
        writer.write_record(&out)?;
    }
    Ok(writer.into_inner()?)
}

/// Whether `bytes` is the CSV of a dataset with no rows: no record and a header of one empty name.
fn is_headerless_empty_csv(bytes: &[u8]) -> bool {
    let mut reader = csv::ReaderBuilder::new().flexible(true).from_reader(bytes);
    let Ok(headers) = reader.headers() else {
        return false;
    };
    headers.len() == 1
        && headers.get(0).is_some_and(str::is_empty)
        && reader.records().next().is_none()
}

/// The CSV of an empty dataset with its columns as the header row.
fn header_only_csv(columns: &[&str]) -> anyhow::Result<Vec<u8>> {
    let mut writer = csv::WriterBuilder::new().from_writer(Vec::new());
    writer.write_record(columns)?;
    Ok(writer.into_inner()?)
}

/// The dataset a `csv/` or `json/` path names (`csv/all-findings.csv` is `all-findings`).
fn dataset_of(path: &str) -> Option<&str> {
    path.strip_prefix("csv/")
        .and_then(|p| p.strip_suffix(".csv"))
        .or_else(|| {
            path.strip_prefix("json/")
                .and_then(|p| p.strip_suffix(".json"))
        })
}

/// Finish the machine-readable files of a package: the added fields on `findings.json` and on the
/// finding datasets (JSON and CSV alike), and a header row on every empty CSV.
pub fn finish(
    findings_json: &mut Vec<u8>,
    datasets: &mut [(String, Vec<u8>)],
) -> anyhow::Result<()> {
    *findings_json = finish_findings_json(findings_json)?;
    for (path, bytes) in datasets.iter_mut() {
        let Some(dataset) = dataset_of(path) else {
            continue;
        };
        let is_finding_rows = FINDING_DATASETS.contains(&dataset);
        if path.starts_with("json/") && is_finding_rows {
            *bytes = finish_json(bytes)?;
        } else if path.starts_with("csv/") {
            if is_headerless_empty_csv(bytes) {
                if let Some(columns) = columns_of(dataset) {
                    *bytes = header_only_csv(&columns)?;
                }
            } else if is_finding_rows {
                *bytes = finish_csv_rows(bytes)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
