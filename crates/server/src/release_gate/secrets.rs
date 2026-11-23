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

use super::archive::{xlsx_cells, xlsx_extra_texts, xlsx_header_footers, xlsx_strings};
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
    // A key split by a space in the text still reads as one key to a person copying it. The text
    // with those spaces closed up ([`close_split_key_spaces`]) is scanned too, for the shapes
    // with a fixed vendor prefix only: the shapes read from their context would invent matches
    // across prose.
    let closed = close_split_key_spaces(&stripped);
    let closed_variant = (closed != stripped.as_ref()).then_some(closed.as_str());
    let mut seen: HashSet<(String, String)> = HashSet::new();
    let mut problems = Vec::new();
    for (variant, is_closed) in variants
        .into_iter()
        .map(|v| (v, false))
        .chain(closed_variant.map(|v| (v, true)))
    {
        for m in camerata_secrets::detect_client_secrets(variant) {
            if skip(&m, variant) || (is_closed && !m.shape.is_vendor_shaped()) {
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
                present_message(m.shape, m.twin.as_ref()),
                Some(evidence),
            ));
        }
    }
    problems
}

/// The problem's message: the shape named, and for a published documentation key that it is one
/// and is unmasked (the export masks it; this proves it did). Never the value.
fn present_message(
    shape: camerata_secrets::SecretShape,
    twin: Option<&camerata_secrets::Twin>,
) -> String {
    if matches!(twin, Some(camerata_secrets::Twin::PublishedExampleKey)) {
        format!(
            "{} (a published example key) present unmasked in a client file",
            shape.display_name()
        )
    } else {
        format!("{} present in a client file", shape.display_name())
    }
}

/// A plain-text file (the README, the manifest, a non-JSON member).
pub fn text_problems(file: &str, text: &str) -> Vec<Problem> {
    scan_text(file, "", text, true)
}

fn map_json_strings(value: &mut Value, f: &dyn Fn(&str) -> String) {
    match value {
        Value::String(s) => *s = f(s),
        Value::Array(items) => items.iter_mut().for_each(|item| map_json_strings(item, f)),
        Value::Object(map) => {
            let entries = std::mem::take(map);
            for (key, mut item) in entries {
                map_json_strings(&mut item, f);
                map.insert(f(&key), item);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

/// The characters a key is made of.
fn is_key_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '+' | '/' | '=')
}

/// `text` with the single spaces closed up where they split what reads as a key: the run after
/// the space opens with six or more key characters and one of the first six is a digit or a
/// capital (random text, not a word), and the run before it ends in a key character. A word
/// after a prefix in prose (`sb_secret_ keys in client code`) is not random, so prose is left as
/// it is; a key split by a space (`sk_live_a1B2c3D4 e5F6g7H8i9J0`) is closed.
fn close_split_key_spaces(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for (i, fragment) in text.split(' ').enumerate() {
        if i > 0 {
            let lead: Vec<char> = fragment.chars().take_while(|c| is_key_char(*c)).collect();
            let random = lead.len() >= 6
                && lead
                    .iter()
                    .take(6)
                    .any(|c| c.is_ascii_digit() || c.is_ascii_uppercase());
            let joins = out.chars().next_back().is_some_and(is_key_char);
            if !(random && joins) {
                out.push(' ');
            }
        }
        out.push_str(fragment);
    }
    out
}

/// A parsed JSON document: every string and every object key, located by JSON path. Read as
/// given, with zero-width characters stripped, and with the spaces that split a key closed up (the last
/// for the shapes with a fixed vendor prefix only, as in [`scan_text_except`]).
pub fn json_problems(file: &str, value: &Value) -> Vec<Problem> {
    let mut stripped = value.clone();
    map_json_strings(&mut stripped, &|s| {
        crate::report_export::strip_zero_width(s).into_owned()
    });
    let mut closed = stripped.clone();
    map_json_strings(&mut closed, &close_split_key_spaces);
    let mut seen: HashSet<(String, String, String)> = HashSet::new();
    let mut problems = Vec::new();
    for (doc, is_closed) in [(value, false), (&stripped, false), (&closed, true)] {
        for hit in camerata_secrets::find_secret_hits_in_json(doc) {
            if is_closed && !hit.shape.is_vendor_shaped() {
                continue;
            }
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
                present_message(hit.shape, hit.twin.as_ref()),
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
        Err(_) => unparseable_json_problems(file, &String::from_utf8_lossy(bytes)),
    }
}

/// A `.json` file that does not parse: scanned as text, and again with its string escapes decoded
/// (`\u0073k_live_...` is the key to any reader that opens it), so an unparseable export is never
/// a way around the check.
fn unparseable_json_problems(file: &str, text: &str) -> Vec<Problem> {
    let mut problems = text_problems(file, text);
    let decoded = decode_json_escapes(text);
    if decoded != text {
        let known: HashSet<(String, Option<String>)> = problems
            .iter()
            .map(|p| (p.message.clone(), p.evidence.clone()))
            .collect();
        for p in text_problems(file, &decoded) {
            if !known.contains(&(p.message.clone(), p.evidence.clone())) {
                problems.push(p);
            }
        }
    }
    problems
}

/// `text` with its JSON string escapes decoded leniently: `\uXXXX` (a surrogate pair too),
/// `\n`, `\t`, `\r`, `\b`, `\f`, `\"`, `\\` and `\/`. An escape that is not valid stays as written.
fn decode_json_escapes(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let hex4 = |at: usize| -> Option<u32> {
        let digits: String = chars.get(at..at + 4)?.iter().collect();
        u32::from_str_radix(&digits, 16).ok()
    };
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '\\' || i + 1 >= chars.len() {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        match chars[i + 1] {
            'u' => {
                let Some(unit) = hex4(i + 2) else {
                    out.push('\\');
                    i += 1;
                    continue;
                };
                let mut consumed = 6;
                let code = if (0xD800..0xDC00).contains(&unit)
                    && chars.get(i + 6) == Some(&'\\')
                    && chars.get(i + 7) == Some(&'u')
                {
                    match hex4(i + 8).filter(|low| (0xDC00..0xE000).contains(low)) {
                        Some(low) => {
                            consumed = 12;
                            0x10000 + ((unit - 0xD800) << 10) + (low - 0xDC00)
                        }
                        None => unit,
                    }
                } else {
                    unit
                };
                if let Some(c) = char::from_u32(code) {
                    out.push(c);
                    i += consumed;
                } else {
                    out.push('\\');
                    i += 1;
                }
            }
            simple @ ('n' | 't' | 'r' | 'b' | 'f' | '"' | '\\' | '/') => {
                out.push(match simple {
                    'n' => '\n',
                    't' => '\t',
                    'r' => '\r',
                    'b' => '\u{8}',
                    'f' => '\u{c}',
                    other => other,
                });
                i += 2;
            }
            _ => {
                out.push('\\');
                i += 1;
            }
        }
    }
    out
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
                // 1. walked like `json/inspection.json`, every string and key on its own (the same
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
    "x",
    "upgrade",
    "insecure",
    "requests",
    "public",
    "pins",
    "permissions",
    // `Public-Key-Pins-Report-Only`: the word is a header word only in company with the others,
    // since every part of a value must be one of these.
    "key",
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
    // Parts a reader can open that are neither a cell nor a header: cell comments, the document
    // properties (title, author, keywords), drawings and charts. Their text is scanned both as
    // runs joined and as runs apart, so a key split across two runs is still one key.
    match xlsx_extra_texts(bytes) {
        Ok(parts) => {
            for (part, joined, apart) in parts {
                let mut seen: HashSet<(String, Option<String>)> = HashSet::new();
                for text in [&joined, &apart] {
                    for p in scan_text(file, &format!("workbook part {part}"), text, false) {
                        if seen.insert((p.message.clone(), p.evidence.clone())) {
                            problems.push(p);
                        }
                    }
                }
            }
        }
        Err(e) => problems.push(Problem::new(
            file,
            "workbook",
            format!(
                "a workbook part could not be read, so it could not be checked for secrets: {e}"
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
