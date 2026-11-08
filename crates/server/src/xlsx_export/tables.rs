//! One writer for every table of the workbook.
//!
//! Every table a person filters is written here, so every table has the same shape: the header on
//! row 1 (or straight under the banner of the one sheet that carries a banner), an autofilter over
//! the whole table, the top row and the first text column frozen, sensible widths, a "Back to
//! Start here" link in the last visible header cell, and a collapsed band of technical columns
//! (ids for tooling) at the right. A sheet cannot skip any of it because it has no other way to be
//! written.

use rust_xlsxwriter::{Color, Format, FormatAlign, FormatUnderline, Workbook, XlsxError};

use super::vocabulary::{self as v};
use super::{CappedSheet, Formats};

/// Excel allows 65,530 hyperlinks on one sheet; past this many a link is written as its text.
pub(super) const LINKS_PER_SHEET_MAX: usize = 60_000;

/// One cell of a table.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Cell {
    Empty,
    Text(String),
    /// Text that wraps.
    Wrap(String),
    /// Wrapped monospace text (code).
    Mono(String),
    Num(f64),
    /// A severity in its lower-case wire form, written title-cased and coloured.
    Severity(String),
    /// A link to a row of another sheet. `row` is the zero-based row in that sheet.
    Link {
        sheet: String,
        row: u32,
        text: String,
    },
}

impl Cell {
    pub(super) fn text(s: impl Into<String>) -> Self {
        Self::Text(s.into())
    }

    pub(super) fn wrap(s: impl Into<String>) -> Self {
        Self::Wrap(s.into())
    }

    pub(super) fn link(sheet: &str, row: u32, text: impl Into<String>) -> Self {
        Self::Link {
            sheet: sheet.to_string(),
            row,
            text: text.into(),
        }
    }
}

/// One column: its header, its width, and whether it is in the technical band.
#[derive(Debug, Clone)]
pub(super) struct Col {
    pub header: String,
    pub width: f64,
    pub technical: bool,
}

impl Col {
    pub(super) fn plain(header: &str, width: f64) -> Self {
        Self {
            header: header.to_string(),
            width,
            technical: false,
        }
    }

    pub(super) fn technical(header: &str, width: f64) -> Self {
        Self {
            header: header.to_string(),
            width,
            technical: true,
        }
    }
}

/// The sheet one table is written to.
pub(super) struct Table {
    pub name: String,
    /// The columns, plain ones first and the technical ones after them.
    pub cols: Vec<Col>,
    /// One vector per row, in `cols` order.
    pub rows: Vec<Vec<Cell>>,
    /// The sheet's title line and the first line under it, for the one sheet that carries a
    /// banner above its header (All source rows).
    pub banner: Option<(String, String)>,
    /// What an empty table says in place of its first row.
    pub empty_note: Option<String>,
    /// A note stored on the first header cell.
    pub header_note: Option<String>,
    /// How many left columns stay on screen when the sheet scrolls sideways.
    pub frozen_cols: u16,
    /// Hide the technical band behind its plus sign. True for every sheet.
    pub collapse_technical: bool,
}

impl Table {
    pub(super) fn new(name: &str, cols: Vec<Col>) -> Self {
        Self {
            name: name.to_string(),
            cols,
            rows: Vec::new(),
            banner: None,
            empty_note: None,
            header_note: None,
            frozen_cols: 1,
            collapse_technical: true,
        }
    }

    /// The zero-based row of the header.
    pub(super) fn header_row(&self) -> u32 {
        if self.banner.is_some() {
            2
        } else {
            0
        }
    }

    /// The zero-based sheet row of data row `index`.
    pub(super) fn data_row(&self, index: usize) -> u32 {
        self.header_row() + 1 + index as u32
    }

    fn plain_count(&self) -> usize {
        self.cols.iter().filter(|c| !c.technical).count()
    }
}

/// The A1 target of a link to `row` (zero-based) of `sheet`.
pub(super) fn link_target(sheet: &str, row: u32) -> String {
    format!("internal:'{}'!A{}", sheet.replace('\'', "''"), row + 1)
}

fn link_format() -> Format {
    Format::new()
        .set_font_color("#0563C1")
        .set_underline(FormatUnderline::Single)
        .set_align(FormatAlign::Top)
}

fn back_link_format() -> Format {
    Format::new()
        .set_bold()
        .set_font_color("#FFFFFF")
        .set_underline(FormatUnderline::Single)
        .set_background_color("#1F2937")
        .set_align(FormatAlign::VerticalCenter)
}

fn technical_header_format() -> Format {
    Format::new()
        .set_bold()
        .set_font_color("#FFFFFF")
        .set_background_color("#6B7280")
        .set_font_size(11)
        .set_align(FormatAlign::VerticalCenter)
}

/// Write `table` as the next worksheet of `wb`.
pub(super) fn write_table(
    wb: &mut Workbook,
    fmts: &Formats,
    table: &Table,
) -> Result<(), XlsxError> {
    let mut ws = CappedSheet::new(wb.add_worksheet());
    ws.set_name(&table.name)?;
    let _ = ws.set_tab_color(Color::from(v::tab_color_of(&table.name)));

    let plain = table.plain_count();
    let spacer = plain as u16;
    let tech_first = spacer + 1;
    // plain + spacer + technical = cols.len() + 1 columns, so the last index is cols.len().
    let last_col = table.cols.len() as u16;
    let header_row = table.header_row();

    if let Some((title, line)) = &table.banner {
        ws.write_text_with_format(0, 0, title, &fmts.title)?;
        ws.write_text(1, 0, line)?;
    }

    // Header cells: plain, then the spacer that carries the way back, then the technical band.
    let mut col_of = Vec::with_capacity(table.cols.len());
    let (mut p, mut t) = (0u16, tech_first);
    for col in &table.cols {
        if col.technical {
            col_of.push(t);
            t += 1;
        } else {
            col_of.push(p);
            p += 1;
        }
    }
    for (col, at) in table.cols.iter().zip(&col_of) {
        let format = if col.technical {
            technical_header_format()
        } else {
            fmts.header.clone()
        };
        ws.write_text_with_format(header_row, *at, &col.header, &format)?;
        ws.set_column_width(*at, col.width)?;
    }
    ws.write_link_with_format(
        header_row,
        spacer,
        &link_target(v::START_HERE, 0),
        v::H_BACK_TO_START,
        &back_link_format(),
    )?;
    ws.set_column_width(spacer, 20.0)?;
    ws.set_row_height(header_row, 28)?;
    if let (Some(note), true) = (&table.header_note, !table.cols.is_empty()) {
        ws.insert_note(header_row, 0, note)?;
    }
    if let Some(first_tech) = table.cols.iter().position(|c| c.technical) {
        ws.insert_note(header_row, col_of[first_tech], v::TECHNICAL_BAND_NOTE)?;
    }

    // Data.
    let text = fmts.cell(None, false, false);
    let wrap = fmts.cell(None, true, false);
    let mono = fmts.cell(None, true, true);
    let link_fmt = link_format();
    let mut links = 0usize;
    for (i, row) in table.rows.iter().enumerate() {
        let r = table.data_row(i);
        for (cell, at) in row.iter().zip(&col_of) {
            match cell {
                Cell::Empty => {}
                Cell::Text(s) => {
                    ws.write_text_with_format(r, *at, s, &text)?;
                }
                Cell::Wrap(s) => {
                    ws.write_text_with_format(r, *at, s, &wrap)?;
                }
                Cell::Mono(s) => {
                    ws.write_text_with_format(r, *at, s, &mono)?;
                }
                Cell::Num(n) => {
                    ws.write_number_with_format(r, *at, *n, &text)?;
                }
                Cell::Severity(sev) => {
                    ws.write_text_with_format(
                        r,
                        *at,
                        super::title_case(sev),
                        &fmts.severity_cell(sev),
                    )?;
                }
                Cell::Link {
                    sheet,
                    row: target,
                    text: label,
                } => {
                    if links < LINKS_PER_SHEET_MAX {
                        links += 1;
                        ws.write_link_with_format(
                            r,
                            *at,
                            &link_target(sheet, *target),
                            label,
                            &link_fmt,
                        )?;
                    } else {
                        ws.write_text_with_format(r, *at, label, &text)?;
                    }
                }
            }
        }
    }
    if table.rows.is_empty() {
        if let Some(note) = &table.empty_note {
            ws.write_text_with_format(header_row + 1, 0, note, &wrap)?;
        }
    }

    // Filters, frozen panes and the technical band.
    let last_data_row = if table.rows.is_empty() {
        header_row + u32::from(table.empty_note.is_some())
    } else {
        header_row + table.rows.len() as u32
    };
    ws.autofilter(
        header_row,
        0,
        last_data_row.max(header_row),
        last_col.max(spacer),
    )?;
    ws.set_freeze_panes(header_row + 1, table.frozen_cols)?;
    if table.cols.iter().any(|c| c.technical) && table.collapse_technical {
        ws.group_columns_collapsed(tech_first, last_col)?;
    }
    Ok(())
}
