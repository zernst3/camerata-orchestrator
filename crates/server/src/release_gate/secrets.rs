//! The gate's secrets check: no secret value in ANY client-visible string.
//!
//! The check walks every string of every file, never a list of named fields: the sanitizer gap
//! this gate exists to close was a field list that did not name `fix_specific`, `also_locations`
//! or `calibration_rationale`. A field added tomorrow is covered the day it exists.
//!
//! Every problem carries the REDACTED form as evidence (`sb_secret_…WvXy (51 chars)`), never the
//! value, and [`Problem::new`] redacts its own text as a second line of defense.

use std::collections::HashSet;

use serde_json::Value;

use super::archive::{xlsx_cells, xlsx_strings};
use super::report::Problem;

/// One problem per secret in `text`, located by `location` (and by line when `with_line`).
/// Scans the text as given AND with zero-width characters stripped, so one inserted inside a key
/// cannot hide it from the detector while a reader still sees the key.
fn scan_text(file: &str, location: &str, text: &str, with_line: bool) -> Vec<Problem> {
    let stripped = crate::report_export::strip_zero_width(text);
    let mut variants: Vec<&str> = vec![text];
    if stripped.as_ref() != text {
        variants.push(stripped.as_ref());
    }
    let mut seen: HashSet<(String, String)> = HashSet::new();
    let mut problems = Vec::new();
    for variant in variants {
        for m in camerata_secrets::detect_secrets(variant) {
            let form = m.redacted_form(variant);
            // A multi-line form (a PEM block) reads as one line in a message.
            let evidence = form.split_whitespace().collect::<Vec<_>>().join(" ");
            if !seen.insert((m.shape.label().to_string(), evidence.clone())) {
                continue;
            }
            let at = if with_line {
                let line = variant[..m.start].matches('\n').count() + 1;
                if location.is_empty() {
                    format!("line {line}")
                } else {
                    format!("{location}, line {line}")
                }
            } else {
                location.to_string()
            };
            problems.push(Problem::new(
                file,
                at,
                format!("{} present in a client file", m.shape.display_name()),
                Some(evidence),
            ));
        }
    }
    problems
}

/// A plain-text file (the README, the manifest, a non-JSON member).
pub fn text_problems(file: &str, text: &str) -> Vec<Problem> {
    scan_text(file, "", text, true)
}

fn strip_json_zero_width(value: &mut Value) {
    match value {
        Value::String(s) => *s = crate::report_export::strip_zero_width(s).into_owned(),
        Value::Array(items) => items.iter_mut().for_each(strip_json_zero_width),
        Value::Object(map) => {
            let entries = std::mem::take(map);
            for (key, mut item) in entries {
                strip_json_zero_width(&mut item);
                map.insert(
                    crate::report_export::strip_zero_width(&key).into_owned(),
                    item,
                );
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

/// A parsed JSON document: every string and every object key, located by JSON path.
pub fn json_problems(file: &str, value: &Value) -> Vec<Problem> {
    let mut stripped = value.clone();
    strip_json_zero_width(&mut stripped);
    let mut seen: HashSet<(String, String, String)> = HashSet::new();
    let mut problems = Vec::new();
    for doc in [value, &stripped] {
        for hit in camerata_secrets::find_secret_hits_in_json(doc) {
            let evidence = hit.form.split_whitespace().collect::<Vec<_>>().join(" ");
            if !seen.insert((
                hit.path.clone(),
                hit.shape.label().to_string(),
                evidence.clone(),
            )) {
                continue;
            }
            problems.push(Problem::new(
                file,
                hit.path,
                format!("{} present in a client file", hit.shape.display_name()),
                Some(evidence),
            ));
        }
    }
    problems
}

/// A JSON file's bytes. A file that does not parse is scanned as text instead, so an unparseable
/// export is never a way around the check.
pub fn json_bytes_problems(file: &str, bytes: &[u8]) -> Vec<Problem> {
    match serde_json::from_slice::<Value>(bytes) {
        Ok(value) => json_problems(file, &value),
        Err(_) => text_problems(file, &String::from_utf8_lossy(bytes)),
    }
}

/// A CSV file's bytes: every header and every cell, located by row and column name.
pub fn csv_problems(file: &str, bytes: &[u8]) -> Vec<Problem> {
    let mut reader = csv::ReaderBuilder::new().flexible(true).from_reader(bytes);
    let Ok(headers) = reader.headers().cloned() else {
        return text_problems(file, &String::from_utf8_lossy(bytes));
    };
    let mut problems = Vec::new();
    for (c, header) in headers.iter().enumerate() {
        problems.extend(scan_text(
            file,
            &format!("header, column {}", c + 1),
            header,
            false,
        ));
    }
    for (i, record) in reader.records().enumerate() {
        let Ok(record) = record else {
            return text_problems(file, &String::from_utf8_lossy(bytes));
        };
        for (c, cell) in record.iter().enumerate() {
            let column = headers.get(c).unwrap_or("?");
            problems.extend(scan_text(
                file,
                &format!("row {}, column {column:?}", i + 1),
                cell,
                false,
            ));
        }
    }
    problems
}

/// A workbook: every cell (located by sheet and cell reference) and every shared string (so a
/// string no cell currently points at is still seen).
pub fn xlsx_problems(file: &str, bytes: &[u8]) -> Vec<Problem> {
    let mut problems = Vec::new();
    match xlsx_cells(bytes) {
        Ok(cells) => {
            for cell in cells {
                problems.extend(scan_text(
                    file,
                    &format!("sheet {:?} cell {}", cell.sheet, cell.reference),
                    &cell.text,
                    false,
                ));
            }
        }
        Err(e) => problems.push(Problem::new(
            file,
            "workbook",
            format!("the workbook could not be read for the secrets check: {e}"),
            None,
        )),
    }
    if let Ok(strings) = xlsx_strings(bytes) {
        let already: HashSet<(String, Option<String>)> = problems
            .iter()
            .map(|p| (p.message.clone(), p.evidence.clone()))
            .collect();
        for (i, text) in strings.iter().enumerate() {
            for p in scan_text(file, &format!("shared string #{i}"), text, false) {
                if !already.contains(&(p.message.clone(), p.evidence.clone())) {
                    problems.push(p);
                }
            }
        }
    }
    problems
}

/// Text extracted from the compiled PDF (the CLI's `release-gate` extracts it; the in-process
/// gate checks the PDF's `data.json` instead, which is the load-bearing check). Typst wraps long
/// tokens across lines, so the text is also scanned with line breaks removed.
pub fn pdf_text_problems(file: &str, text: &str) -> Vec<Problem> {
    let mut problems = scan_text(file, "rendered text", text, false);
    let joined: String = text.chars().filter(|c| *c != '\n' && *c != '\r').collect();
    let known: HashSet<(String, Option<String>)> = problems
        .iter()
        .map(|p| (p.message.clone(), p.evidence.clone()))
        .collect();
    for p in scan_text(file, "rendered text", &joined, false) {
        if !known.contains(&(p.message.clone(), p.evidence.clone())) {
            problems.push(p);
        }
    }
    problems
}
