//! The gate's operator-only check: no package, client or internal, carries the operator's rewrite
//! advisory or any of its names (`camerata_api_types::rewrite_advisory`).
//!
//! Owner ruling: whether a client fixes or rewrites is the architect's decision alone, and no
//! client file ever states or hints at it. The advisory is written beside the failure report in
//! the state directory and shown in the cockpit's internal panel; this check is the tripwire that
//! refuses a package if one of its keys or phrases reached a zip anyway. Unlike the text-style
//! check it holds the operator's own internal package too: "never in any zip, internal included".
//!
//! It reads every surface a package has, so a field added tomorrow is covered the day it exists:
//! every member's name (the file name stem is in each), every member's text, every part of the
//! workbook (sheets, shared strings, document properties, headers and footers) and its cell and
//! string text, the PDF's text and the PDF's data. Terms match ignoring case. The bare word
//! "rewrite" is not a term (owner ruling), so a client's own code and an ordinary fix can say it.
//! A PDF whose text cannot be read is refused: a PDF the check could not read is not one it has
//! passed.

use std::collections::BTreeMap;

use camerata_api_types::rewrite_advisory::terms_in;

use super::archive::{read_zip_entries, xlsx_cells, xlsx_strings};
use super::report::Problem;
use super::text_style::PdfText;

/// The problems for the terms found in `text`, one per term.
pub fn text_problems(file: &str, location: &str, text: &str) -> Vec<Problem> {
    terms_in(text)
        .into_iter()
        .map(|term| {
            Problem::new(
                file,
                location,
                format!(
                    "an operator-only name ({term:?}) is in a package; the rewrite advisory is \
                     internal to the operator and never part of any export"
                ),
                Some(term.to_string()),
            )
        })
        .collect()
}

/// A workbook: every part of it as text (sheets, shared strings, document properties, headers and
/// footers), then its cells and shared strings for any term no part showed whole: a rich-text
/// string can split a phrase across runs in the XML, and the cell text joins them. A term is
/// reported once per place it is found.
fn xlsx_problems(file: &str, bytes: &[u8]) -> Vec<Problem> {
    let mut problems = Vec::new();
    match read_zip_entries(bytes) {
        Ok(parts) => {
            for (part, part_bytes) in &parts {
                problems.extend(text_problems(
                    file,
                    &format!("workbook part {part}"),
                    &String::from_utf8_lossy(part_bytes),
                ));
            }
        }
        Err(e) => problems.push(Problem::new(
            file,
            "workbook",
            format!("the workbook could not be read for the operator-only check: {e}"),
            None,
        )),
    }
    let mut texts: Vec<(String, String)> = Vec::new();
    if let Ok(cells) = xlsx_cells(bytes) {
        texts.extend(
            cells
                .into_iter()
                .map(|c| (format!("sheet {:?} cell {}", c.sheet, c.reference), c.text)),
        );
    }
    if let Ok(strings) = xlsx_strings(bytes) {
        texts.extend(
            strings
                .into_iter()
                .enumerate()
                .map(|(i, text)| (format!("shared string #{i}"), text)),
        );
    }
    let mut seen: std::collections::BTreeSet<String> =
        problems.iter().filter_map(|p| p.evidence.clone()).collect();
    for (location, text) in texts {
        for problem in text_problems(file, &location, &text) {
            if problem
                .evidence
                .as_ref()
                .is_some_and(|t| seen.insert(t.clone()))
            {
                problems.push(problem);
            }
        }
    }
    problems
}

/// A PDF member, from its text: the caller's extraction when it has one, else the pages read
/// once for the archive pass.
fn pdf_problems(file: &str, bytes: &[u8], read: Option<&PdfText>) -> Vec<Problem> {
    match read {
        Some(PdfText::Pages(pages)) => pages
            .iter()
            .enumerate()
            .flat_map(|(i, page)| text_problems(file, &format!("page {}", i + 1), page))
            .collect(),
        // A stand-in, not a compiled document: its bytes are its text.
        Some(PdfText::NotJudged) | None => {
            text_problems(file, "PDF bytes", &String::from_utf8_lossy(bytes))
        }
        Some(PdfText::Unreadable(why)) => vec![Problem::new(
            file,
            "PDF text",
            format!(
                "the PDF's text could not be read, so it could not be checked for operator-only \
                 names ({why})"
            ),
            None,
        )],
    }
}

/// Every member of the archive. `pdf_text` is the caller's own extraction of the PDF's text (the
/// CLI's `release-gate`); `pdf_pages` is the archive pass's own reading of each PDF member, by
/// name, when the caller has none.
pub fn archive_problems(
    entries: &BTreeMap<String, Vec<u8>>,
    pdf_text: Option<&str>,
    pdf_pages: &BTreeMap<String, PdfText>,
) -> Vec<Problem> {
    let mut problems = Vec::new();
    for (name, bytes) in entries {
        problems.extend(text_problems("(archive member name)", "file name", name));
        if name.ends_with(".xlsx") {
            problems.extend(xlsx_problems(name, bytes));
        } else if name.ends_with(".pdf") {
            match pdf_text {
                Some(text) => problems.extend(text_problems(name, "PDF text", text)),
                None => problems.extend(pdf_problems(name, bytes, pdf_pages.get(name))),
            }
        } else {
            problems.extend(text_problems(
                name,
                "file text",
                &String::from_utf8_lossy(bytes),
            ));
        }
    }
    problems
}
