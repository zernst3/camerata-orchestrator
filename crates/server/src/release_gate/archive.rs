//! Reading a released package back: the zip's members and the workbook's cells and strings.
//! The gate and the export tests both read through these, so there is one reader.

use std::collections::BTreeMap;
use std::io::Read;

/// Every member of a zip archive, by name.
pub fn read_zip_entries(bytes: &[u8]) -> Result<BTreeMap<String, Vec<u8>>, String> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|e| format!("not a readable zip archive: {e}"))?;
    let mut out = BTreeMap::new();
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| format!("unreadable archive member #{i}: {e}"))?;
        if entry.is_dir() {
            continue;
        }
        let mut data = Vec::new();
        entry
            .read_to_end(&mut data)
            .map_err(|e| format!("unreadable archive member {}: {e}", entry.name()))?;
        out.insert(entry.name().to_string(), data);
    }
    Ok(out)
}

/// One workbook cell: the sheet it is on, its A1 reference, and its text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XlsxCell {
    pub sheet: String,
    /// `V2`.
    pub reference: String,
    /// Zero-based.
    pub row: usize,
    /// Zero-based.
    pub col: usize,
    pub text: String,
}

fn part_text(xlsx: &[u8], name: &str) -> Result<String, String> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(xlsx))
        .map_err(|e| format!("the workbook is not a zip: {e}"))?;
    let mut file = archive
        .by_name(name)
        .map_err(|_| format!("the workbook has no part {name}"))?;
    let mut text = String::new();
    file.read_to_string(&mut text)
        .map_err(|e| format!("the workbook part {name} is not UTF-8: {e}"))?;
    Ok(text)
}

/// Decode the five XML entities. `&amp;` last, so `&amp;lt;` stays `&lt;`.
fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

/// Every string of `xl/sharedStrings.xml`, in pool order: the text of each `<si>`'s `<t>` runs,
/// concatenated. `rust_xlsxwriter` writes every string cell as plain runs, so this is
/// exhaustive for our workbook.
pub fn xlsx_strings(xlsx: &[u8]) -> Result<Vec<String>, String> {
    let xml = part_text(xlsx, "xl/sharedStrings.xml")?;
    Ok(xml
        .split("<si>")
        .skip(1)
        .map(|si| {
            let si = si.split("</si>").next().unwrap_or("");
            let mut text = String::new();
            let mut rest = si;
            while let Some(open) = rest.find("<t") {
                let after = &rest[open..];
                let Some(gt) = after.find('>') else { break };
                let body = &after[gt + 1..];
                let Some(close) = body.find("</t>") else {
                    break;
                };
                text.push_str(&unescape(&body[..close]));
                rest = &body[close + 4..];
            }
            text
        })
        .collect())
}

/// `"AB12"` to (col 27, row 11), both zero-based.
fn split_ref(reference: &str) -> Option<(usize, usize)> {
    let letters: String = reference
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect();
    let digits: String = reference.chars().skip(letters.len()).collect();
    if letters.is_empty() {
        return None;
    }
    let col = letters
        .bytes()
        .fold(0usize, |acc, b| {
            acc * 26 + usize::from(b.to_ascii_uppercase() - b'A' + 1)
        })
        .checked_sub(1)?;
    Some((col, digits.parse::<usize>().ok()?.checked_sub(1)?))
}

fn sheet_names(workbook_xml: &str) -> Vec<String> {
    workbook_xml
        .split("<sheet name=\"")
        .skip(1)
        .filter_map(|c| Some(unescape(&c[..c.find('"')?])))
        .collect()
}

/// Every non-empty cell of every worksheet, with its sheet and A1 reference. Numbers are their
/// literal XML value; shared and inline strings are resolved to text.
pub fn xlsx_cells(xlsx: &[u8]) -> Result<Vec<XlsxCell>, String> {
    let names = sheet_names(&part_text(xlsx, "xl/workbook.xml")?);
    let strings = xlsx_strings(xlsx).unwrap_or_default();
    let mut cells = Vec::new();
    for (idx, sheet) in names.iter().enumerate() {
        let xml = part_text(xlsx, &format!("xl/worksheets/sheet{}.xml", idx + 1))?;
        for chunk in xml.split("<c ").skip(1) {
            let Some(tag_end) = chunk.find('>') else {
                continue;
            };
            let tag = &chunk[..tag_end];
            let attr = |name: &str| -> Option<String> {
                let key = format!("{name}=\"");
                let at = tag.find(&key)? + key.len();
                Some(tag[at..at + tag[at..].find('"')?].to_string())
            };
            let Some(reference) = attr("r") else { continue };
            let Some((col, row)) = split_ref(&reference) else {
                continue;
            };
            if tag.ends_with('/') {
                continue;
            }
            let body = &chunk[tag_end + 1..];
            let body = &body[..body.find("</c>").unwrap_or(body.len())];
            let value = body
                .find("<v>")
                .and_then(|s| body[s + 3..].find("</v>").map(|e| &body[s + 3..s + 3 + e]));
            let text = match (attr("t").as_deref(), value) {
                (Some("s"), Some(v)) => v
                    .parse::<usize>()
                    .ok()
                    .and_then(|i| strings.get(i).cloned())
                    .unwrap_or_default(),
                (Some("inlineStr"), _) => body
                    .find("<t")
                    .and_then(|s| {
                        let b = &body[s..];
                        let gt = b.find('>')?;
                        Some(unescape(&b[gt + 1..b.find("</t>")?]))
                    })
                    .unwrap_or_default(),
                (_, Some(v)) => unescape(v),
                (_, None) => String::new(),
            };
            if !text.is_empty() {
                cells.push(XlsxCell {
                    sheet: sheet.clone(),
                    reference,
                    row,
                    col,
                    text,
                });
            }
        }
    }
    Ok(cells)
}

/// A worksheet as a dense grid of cell text (`grid[row][col]`, blank cells as `""`).
pub fn xlsx_sheet_grid(xlsx: &[u8], sheet_name: &str) -> Result<Vec<Vec<String>>, String> {
    let names = sheet_names(&part_text(xlsx, "xl/workbook.xml")?);
    if !names.iter().any(|n| n == sheet_name) {
        return Err(format!("no worksheet {sheet_name:?}; have {names:?}"));
    }
    let cells: Vec<XlsxCell> = xlsx_cells(xlsx)?
        .into_iter()
        .filter(|c| c.sheet == sheet_name)
        .collect();
    let rows = cells.iter().map(|c| c.row).max().map_or(0, |m| m + 1);
    let cols = cells.iter().map(|c| c.col).max().map_or(0, |m| m + 1);
    let mut grid = vec![vec![String::new(); cols]; rows];
    for cell in cells {
        grid[cell.row][cell.col] = cell.text;
    }
    Ok(grid)
}
