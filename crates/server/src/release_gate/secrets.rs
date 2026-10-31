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

use super::archive::{xlsx_cells, xlsx_header_footers, xlsx_strings};
use super::report::Problem;

/// One problem per secret in `text`, located by `location` (and by line when `with_line`).
/// Scans the text as given AND with zero-width characters stripped, so one inserted inside a key
/// cannot hide it from the detector while a reader still sees the key.
fn scan_text(file: &str, location: &str, text: &str, with_line: bool) -> Vec<Problem> {
    scan_text_except(file, location, text, with_line, &|_, _| false)
}

/// [`scan_text`] with a `skip` test: a match for which `skip(match, matched text)` is true is
/// left out. Only [`csv_problems`] uses it, for the one shape a JSON cell scanned as one string
/// invents (see there); everything else keeps every match.
fn scan_text_except(
    file: &str,
    location: &str,
    text: &str,
    with_line: bool,
    skip: &dyn Fn(&camerata_secrets::SecretMatch, &str) -> bool,
) -> Vec<Problem> {
    let stripped = crate::report_export::strip_zero_width(text);
    let mut variants: Vec<&str> = vec![text];
    if stripped.as_ref() != text {
        variants.push(stripped.as_ref());
    }
    let mut seen: HashSet<(String, String)> = HashSet::new();
    let mut problems = Vec::new();
    for variant in variants {
        for m in camerata_secrets::detect_secrets(variant) {
            if skip(&m, variant) {
                continue;
            }
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
            let at = format!("row {}, column {column:?}", i + 1);
            match nested_json_cell(cell) {
                // A nested list or object (the `also_locations` column) is JSON text. It is
                // scanned BOTH ways, and the union is reported, so the check is never weaker
                // than scanning the cell as one string was:
                // 1. walked like `findings.json`, every string and key on its own (the same
                //    detector, the same strictness), which also sees an unquoted `.env` line
                //    inside a snippet that the one-string scan cannot (the line is not at a line
                //    start there);
                // 2. as one string, which also sees a value whose NAME sits in a neighbouring
                //    field (a rule id naming a key beside a bare value in the snippet).
                // The one-string scan invents exactly one false shape: a named secret literal
                // whose "value" is itself a NAME, not a value, beside a rule id naming a key:
                // an environment variable name (the efficiency run's refused export) or an HTTP
                // header name such as `Access-Control-Allow-Credentials` (D6, the faktura and
                // lumen exports). Only that shape is skipped, and only on the one-string pass.
                Some(value) => {
                    let mut seen: HashSet<(String, Option<String>)> = HashSet::new();
                    for p in json_problems(file, &value) {
                        seen.insert((p.message.clone(), p.evidence.clone()));
                        problems.push(Problem::new(
                            file,
                            format!("{at}, {}", p.location),
                            p.message,
                            p.evidence,
                        ));
                    }
                    for p in scan_text_except(file, &at, cell, false, &|m, text| {
                        m.shape == camerata_secrets::SecretShape::NamedSecretLiteral && {
                            let value = &text[m.value.clone()];
                            is_environment_variable_name(value) || is_http_header_name(value)
                        }
                    }) {
                        if seen.insert((p.message.clone(), p.evidence.clone())) {
                            problems.push(p);
                        }
                    }
                }
                None => problems.extend(scan_text(file, &at, cell, false)),
            }
        }
    }
    problems
}

/// Whether a matched "value" is an environment variable NAME rather than a value: upper case
/// letters, digits and underscores only, at least one underscore, a letter first, and fewer
/// than four digits (`NEXT_PUBLIC_SUPABASE_SERVICE_ROLE_KEY`, `S3_SECRET_ACCESS_KEY`), with the
/// `=` the line may end in. A generated secret of that length has lower case letters or more
/// digits in it; one with neither is still caught when its name is in the same string.
fn is_environment_variable_name(value: &str) -> bool {
    let name = value.trim_end_matches('=');
    name.starts_with(|c: char| c.is_ascii_uppercase())
        && name
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        && name.contains('_')
        && name.chars().filter(char::is_ascii_digit).count() < 4
}

/// Words an HTTP header name is built from (`Access-Control-Allow-Credentials`,
/// `Strict-Transport-Security`, `Cross-Origin-Opener-Policy`), lower case. A value is a header
/// name only when EVERY part is one of these, so a passphrase of ordinary words
/// (`Correct-Horse-Battery-Staple`) or a hyphenated random token is still a credential, and a
/// header outside the list stays refused (the safe direction: the gate fails closed).
const HEADER_WORDS: &[&str] = &[
    "access",
    "control",
    "allow",
    "credentials",
    "origin",
    "headers",
    "methods",
    "expose",
    "max",
    "age",
    "content",
    "type",
    "security",
    "policy",
    "strict",
    "transport",
    "request",
    "forwarded",
    "for",
    "proto",
    "host",
    "cache",
    "frame",
    "options",
    "referrer",
    "permitted",
    "cross",
    "domain",
    "policies",
    "embedder",
    "opener",
    "resource",
    "real",
    "ip",
    "requested",
    "with",
    "accept",
    "language",
    "encoding",
    "disposition",
    "length",
    "vary",
    "etag",
    "location",
    "user",
    "agent",
    "csrf",
    "xsrf",
    "fetch",
    "site",
    "dest",
    "mode",
    "timing",
    "report",
    "only",
    "private",
    "network",
    "local",
];

/// Whether a matched "value" is an HTTP header NAME: two or more alphabetic words joined by `-`,
/// all Title-Case or all lower case, every one a header word (see [`HEADER_WORDS`]). No digit,
/// no mixed case inside a word: the shape of `Access-Control-Allow-Origin`, not of a key.
fn is_http_header_name(value: &str) -> bool {
    let parts: Vec<&str> = value.split('-').collect();
    let well_cased = |title: bool| {
        parts.iter().all(|part| {
            let mut chars = part.chars();
            let first_ok = chars.next().is_some_and(|c| {
                if title {
                    c.is_ascii_uppercase()
                } else {
                    c.is_ascii_lowercase()
                }
            });
            first_ok && chars.all(|c| c.is_ascii_lowercase())
        })
    };
    parts.len() >= 2
        && (well_cased(true) || well_cased(false))
        && parts
            .iter()
            .all(|part| HEADER_WORDS.contains(&part.to_ascii_lowercase().as_str()))
}

/// The parsed value of a CSV cell holding a nested JSON list or object, as `json_array_to_csv`
/// writes one. `None` for any other cell (including a scalar that happens to parse as JSON).
fn nested_json_cell(cell: &str) -> Option<Value> {
    let trimmed = cell.trim();
    if !(trimmed.starts_with('[') || trimmed.starts_with('{')) {
        return None;
    }
    serde_json::from_str::<Value>(trimmed)
        .ok()
        .filter(|v| v.is_array() || v.is_object())
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
    // Sheet headers and footers (the confidentiality line) live in each sheet's XML, not in a
    // cell or the shared-strings table, so they are read separately.
    match xlsx_header_footers(bytes) {
        Ok(parts) => {
            for hf in parts {
                for text in hf.sections() {
                    problems.extend(scan_text(
                        file,
                        &format!("sheet {:?} {}", hf.sheet, hf.part),
                        text,
                        false,
                    ));
                }
            }
        }
        Err(e) => problems.push(Problem::new(
            file,
            "workbook",
            format!(
                "the workbook's headers and footers could not be read for the secrets check: {e}"
            ),
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
