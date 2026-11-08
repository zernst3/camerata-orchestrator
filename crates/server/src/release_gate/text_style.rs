//! The gate's text-style check: no em dash, no en dash and no multiplication sign in any file a
//! client receives ([`crate::client_text::BANNED`]).
//!
//! Like the secrets check it walks every string of every member rather than a list of named
//! fields, so a field added tomorrow is covered the day it exists: the PDF's text, every workbook
//! string (cells, shared strings, sheet names, headers and footers), every CSV cell, every JSON
//! string and key, and any other member (the README, the manifest) line by line. The producers
//! normalise their own text; this check is what stops one that does not.

use std::collections::BTreeMap;

use serde_json::Value;

use super::archive::{xlsx_cells, xlsx_header_footers, xlsx_sheet_names, xlsx_strings};
use super::report::Problem;
use crate::client_text::{banned_name, is_banned};

/// The most problems one member reports per kind of character. A member with hundreds of hits
/// reports the first few and the count, so the refusal stays readable.
const PER_FILE_CAP: usize = 5;

/// What a hit is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    /// One of the [`crate::client_text::BANNED`] characters.
    Character(char),
    /// A problem in a whole file printed as a line number: `line 0`, or `path:0`.
    LineZero,
}

/// One hit before it becomes a [`Problem`].
struct Hit {
    location: String,
    kind: Kind,
    context: String,
}

/// About forty characters either side of `at`, on one line, so the reader can find the place.
fn context_around(text: &str, at: usize) -> String {
    let start = text[..at]
        .char_indices()
        .rev()
        .nth(39)
        .map_or(0, |(i, _)| i);
    let end = text[at..]
        .char_indices()
        .nth(41)
        .map_or(text.len(), |(i, _)| at + i);
    text[start..end]
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Where in `text` a whole-file problem is printed as a line number: `line 0` (and no more digits
/// after it) or a path ending in `:0` ([`crate::client_text::whole_file_places`]).
fn line_zero_offsets(text: &str) -> Vec<usize> {
    let bytes = text.as_bytes();
    let mut at = Vec::new();
    let mut from = 0;
    while let Some(found) = text[from..].find("line 0") {
        let i = from + found;
        let after = bytes.get(i + 6);
        let before = i.checked_sub(1).and_then(|k| bytes.get(k));
        if !after.is_some_and(|c| c.is_ascii_alphanumeric())
            && !before.is_some_and(|c| c.is_ascii_alphanumeric())
        {
            at.push(i);
        }
        from = i + 6;
    }
    for (i, byte) in bytes.iter().enumerate() {
        if *byte == b':'
            && i > 0
            && bytes[i - 1].is_ascii_alphabetic()
            && bytes.get(i + 1) == Some(&b'0')
            && !bytes
                .get(i + 2)
                .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b':')
        {
            at.push(i - 1);
        }
    }
    at
}

/// The hits of `text`, located by `location`; `with_line` adds the line number.
fn hits_in(location: &str, text: &str, with_line: bool, out: &mut Vec<Hit>) {
    let placed = |at: usize| {
        let line = text[..at].matches('\n').count() + 1;
        if with_line {
            if location.is_empty() {
                format!("line {line}")
            } else {
                format!("{location}, line {line}")
            }
        } else {
            location.to_string()
        }
    };
    for (at, character) in crate::client_text::banned_in(text) {
        out.push(Hit {
            location: placed(at),
            kind: Kind::Character(character),
            context: context_around(text, at),
        });
    }
    for at in line_zero_offsets(text) {
        out.push(Hit {
            location: placed(at),
            kind: Kind::LineZero,
            context: context_around(text, at),
        });
    }
}

/// The problems for one member's hits: the first [`PER_FILE_CAP`] of each kind, then one line
/// saying how many more there are.
fn problems_of(file: &str, hits: Vec<Hit>) -> Vec<Problem> {
    let mut problems = Vec::new();
    let mut seen: BTreeMap<Kind, usize> = BTreeMap::new();
    for hit in &hits {
        let n = seen.entry(hit.kind).or_insert(0);
        *n += 1;
        if *n > PER_FILE_CAP {
            continue;
        }
        let message = match hit.kind {
            Kind::Character(c) => format!(
                "an {} (U+{:04X}) in a client file; write a comma, a colon or a full stop (or \
                 \"by\" for a multiplication sign)",
                banned_name(c).unwrap_or("character"),
                u32::from(c)
            ),
            Kind::LineZero => "a problem in a whole file is printed as a line number; a client \
                 file says \"whole file\", never \"line 0\" or \":0\""
                .to_string(),
        };
        problems.push(Problem::new(
            file,
            &hit.location,
            message,
            Some(hit.context.clone()),
        ));
    }
    for (kind, total) in seen {
        if total > PER_FILE_CAP {
            let what = match kind {
                Kind::Character(c) => format!(
                    "of the {} (U+{:04X})",
                    banned_name(c).unwrap_or("character"),
                    u32::from(c)
                ),
                Kind::LineZero => "places printed as line 0".to_string(),
            };
            problems.push(Problem::new(
                file,
                "(the rest of the file)",
                format!(
                    "{} more {what} in the same file, {total} in all",
                    total - PER_FILE_CAP
                ),
                None,
            ));
        }
    }
    problems
}

/// A plain-text member (the README, the manifest when it does not parse, any other member).
pub fn text_problems(file: &str, text: &str) -> Vec<Problem> {
    let mut hits = Vec::new();
    hits_in("", text, true, &mut hits);
    problems_of(file, hits)
}

fn json_hits(path: &str, value: &Value, hits: &mut Vec<Hit>) {
    match value {
        Value::String(s) => hits_in(path, s, false, hits),
        Value::Array(items) => {
            for (i, item) in items.iter().enumerate() {
                json_hits(&format!("{path}[{i}]"), item, hits);
            }
        }
        Value::Object(map) => {
            for (key, item) in map {
                hits_in(&format!("{path}.{key} (key)"), key, false, hits);
                json_hits(&format!("{path}.{key}"), item, hits);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

/// A parsed JSON document: every string and every key, located by JSON path. Every `headline` in
/// it is also held to the rule against the engine talking to itself ([`headline_problems`]).
pub fn json_problems(file: &str, value: &Value) -> Vec<Problem> {
    let mut hits = Vec::new();
    json_hits("$", value, &mut hits);
    let mut problems = problems_of(file, hits);
    problems.extend(headline_problems(file, "$", value));
    problems
}

/// A `headline` a client reads is a plain description of the problem. It never opens with the
/// engine's own note about the rule ([`crate::client_text::BANNED_HEADLINE_PREFIXES`]: a
/// diagnostic about the rule's evidence, a hedge label, a held-for-review marker), and it never
/// opens with the rule id of its own row.
pub fn headline_problems(file: &str, path: &str, value: &Value) -> Vec<Problem> {
    let mut problems = Vec::new();
    match value {
        Value::Object(map) => {
            let text = |key: &str| map.get(key).and_then(Value::as_str).map(str::trim);
            let rule_id = text("rule_id").filter(|id| !id.is_empty());
            if let Some(headline) = text("headline") {
                if let Some(prefix) = crate::client_text::banned_headline_prefix(headline) {
                    problems.push(Problem::new(
                        file,
                        format!("{path}.headline"),
                        format!(
                            "a client headline must not open with the engine's own note \
                             ({prefix:?}); give the row a plain headline and put the reason in \
                             the reviewer-only field"
                        ),
                        Some(headline.chars().take(80).collect()),
                    ));
                }
                if let Some(id) = rule_id.filter(|id| headline.starts_with(id)) {
                    problems.push(Problem::new(
                        file,
                        format!("{path}.headline"),
                        format!("a client headline must not open with the rule id {id:?}"),
                        Some(headline.chars().take(80).collect()),
                    ));
                }
            }
            for (key, item) in map {
                problems.extend(headline_problems(file, &format!("{path}.{key}"), item));
            }
        }
        Value::Array(items) => {
            for (i, item) in items.iter().enumerate() {
                problems.extend(headline_problems(file, &format!("{path}[{i}]"), item));
            }
        }
        _ => {}
    }
    problems
}

/// A JSON member's bytes. A member that does not parse is scanned as text instead, so an
/// unparseable file is never a way around the check.
pub fn json_bytes_problems(file: &str, bytes: &[u8]) -> Vec<Problem> {
    match serde_json::from_slice::<Value>(bytes) {
        Ok(value) => json_problems(file, &value),
        Err(_) => text_problems(file, &String::from_utf8_lossy(bytes)),
    }
}

/// A CSV member's bytes: every header and every cell, located by row and column name.
pub fn csv_problems(file: &str, bytes: &[u8]) -> Vec<Problem> {
    let mut reader = csv::ReaderBuilder::new().flexible(true).from_reader(bytes);
    let Ok(headers) = reader.headers().cloned() else {
        return text_problems(file, &String::from_utf8_lossy(bytes));
    };
    let mut hits = Vec::new();
    for (c, header) in headers.iter().enumerate() {
        hits_in(
            &format!("header, column {}", c + 1),
            header,
            false,
            &mut hits,
        );
    }
    for (i, record) in reader.records().enumerate() {
        let Ok(record) = record else {
            return text_problems(file, &String::from_utf8_lossy(bytes));
        };
        for (c, cell) in record.iter().enumerate() {
            let column = headers.get(c).unwrap_or("?");
            hits_in(
                &format!("row {}, column {column:?}", i + 1),
                cell,
                false,
                &mut hits,
            );
        }
    }
    problems_of(file, hits)
}

/// A workbook: every cell (located by sheet and cell reference), every shared string, every
/// sheet name, header and footer.
pub fn xlsx_problems(file: &str, bytes: &[u8]) -> Vec<Problem> {
    let mut hits = Vec::new();
    let mut unreadable = Vec::new();
    match xlsx_cells(bytes) {
        Ok(cells) => {
            for cell in cells {
                hits_in(
                    &format!("sheet {:?} cell {}", cell.sheet, cell.reference),
                    &cell.text,
                    false,
                    &mut hits,
                );
            }
        }
        Err(e) => unreadable.push(e),
    }
    match xlsx_sheet_names(bytes) {
        Ok(names) => {
            for name in names {
                hits_in(&format!("sheet name {name:?}"), &name, false, &mut hits);
            }
        }
        Err(e) => unreadable.push(e),
    }
    match xlsx_header_footers(bytes) {
        Ok(parts) => {
            for hf in parts {
                for text in hf.sections() {
                    hits_in(
                        &format!("sheet {:?} {}", hf.sheet, hf.part),
                        text,
                        false,
                        &mut hits,
                    );
                }
            }
        }
        Err(e) => unreadable.push(e),
    }
    // A string no cell currently points at is still a string the file carries.
    if let Ok(strings) = xlsx_strings(bytes) {
        let located: Vec<String> = hits.iter().map(|h| h.context.clone()).collect();
        for (i, text) in strings.iter().enumerate() {
            let mut own = Vec::new();
            hits_in(&format!("shared string #{i}"), text, false, &mut own);
            hits.extend(own.into_iter().filter(|h| !located.contains(&h.context)));
        }
    }
    let mut problems = problems_of(file, hits);
    for e in unreadable {
        problems.push(Problem::new(
            file,
            "workbook",
            format!("the workbook could not be read for the text-style check: {e}"),
            None,
        ));
    }
    problems
}

/// What reading a PDF member's text gave.
pub enum PdfText {
    /// The text of each page.
    Pages(Vec<String>),
    /// Not a document this check can judge: it does not start `%PDF` (the dataset-set check owns
    /// that), or it is a stand-in (the assembly zips one before the PDF is compiled, and tests
    /// zip others).
    NotJudged,
    /// A real-looking PDF whose text could not be read, and why.
    Unreadable(String),
}

/// A PDF this size or with a trailer is a real document: a Camerata report's cover alone is
/// kilobytes, and Typst always writes `%%EOF`. Anything shorter without a trailer is a stand-in.
const REAL_PDF_MIN_BYTES: usize = 1024;

fn looks_like_a_real_pdf(bytes: &[u8]) -> bool {
    bytes.len() >= REAL_PDF_MIN_BYTES || bytes.windows(5).any(|w| w == b"%%EOF")
}

/// The text of a compiled PDF, page by page. The extractor can panic on a malformed file, which
/// is caught and reported like any other failure to read: a PDF the check cannot open is not a
/// PDF the check has passed.
pub fn pdf_text(bytes: &[u8]) -> PdfText {
    if !bytes.starts_with(b"%PDF") || !looks_like_a_real_pdf(bytes) {
        return PdfText::NotJudged;
    }
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        pdf_extract::extract_text_from_mem_by_pages(bytes)
    })) {
        Ok(Ok(pages)) => PdfText::Pages(pages),
        Ok(Err(e)) => PdfText::Unreadable(e.to_string()),
        Err(_) => PdfText::Unreadable("the reader stopped on it".to_string()),
    }
}

/// A compiled PDF's text, located by page. A real PDF whose text cannot be read is itself a
/// problem: the export fails closed.
pub fn pdf_problems(file: &str, bytes: &[u8]) -> Vec<Problem> {
    let pages = match pdf_text(bytes) {
        PdfText::Pages(pages) => pages,
        PdfText::NotJudged => return Vec::new(),
        PdfText::Unreadable(why) => {
            return vec![Problem::new(
                file,
                "PDF text",
                format!(
                    "the PDF's text could not be read, so it could not be checked for dashes and \
                     for a whole file printed as line 0 ({why})"
                ),
                None,
            )]
        }
    };
    let mut hits = Vec::new();
    for (i, page) in pages.iter().enumerate() {
        hits_in(&format!("page {}", i + 1), page, false, &mut hits);
    }
    problems_of(file, hits)
}

/// Every member of the archive, by kind. `pdf_text` is the PDF's text when the caller already
/// extracted it (the CLI's `release-gate`); otherwise the PDF is read here.
pub fn archive_problems(
    entries: &BTreeMap<String, Vec<u8>>,
    pdf_text: Option<&str>,
) -> Vec<Problem> {
    let mut problems = Vec::new();
    for (name, bytes) in entries {
        if name.chars().any(is_banned) {
            problems.extend(text_problems("(archive member name)", name));
        }
        if name.ends_with(".xlsx") {
            problems.extend(xlsx_problems(name, bytes));
        } else if name.ends_with(".json") {
            problems.extend(json_bytes_problems(name, bytes));
        } else if name.ends_with(".csv") {
            problems.extend(csv_problems(name, bytes));
        } else if name.ends_with(".pdf") {
            match pdf_text {
                Some(text) => problems.extend(text_problems(name, text)),
                None => problems.extend(pdf_problems(name, bytes)),
            }
        } else {
            problems.extend(text_problems(name, &String::from_utf8_lossy(bytes)));
        }
    }
    problems
}
