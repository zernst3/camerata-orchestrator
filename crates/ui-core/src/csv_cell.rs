//! CSV injection (V1 item 33, finding 39). Lives in ui-core so the server's package writers and
//! the cockpit's own CSV downloads share the one function.
//!
//! CSV injection: a spreadsheet runs a cell that opens with `=`, `+`,
//! `-` or `@` (or a tab or carriage return) as a formula. A snippet quoted from a client's
//! repository is such a cell when it is a shell line (`- run: npm ci`) and a prompt in Excel when
//! it is worse. The one rule here is the standard OWASP defence: a leading apostrophe, which a
//! spreadsheet reads as "this is text" and does not show.
//!
//! [`neutralize`] is the ONE function every CSV the package writes passes its cells through
//! (the dataset CSVs, `tickets.csv`, the triage log, the tech-debt issue CSV), and
//! [`starts_a_formula`] is what the release gate asks of every cell it reads back.

use std::borrow::Cow;

/// The characters that make a spreadsheet read a cell as a formula.
const FORMULA_LEADERS: [char; 6] = ['=', '+', '-', '@', '\t', '\r'];

/// Whether `cell` would be read as a formula: it opens with a [`FORMULA_LEADERS`] character and
/// is not a plain number (`-3`, `+7`, `-1.5e3` are numbers, and a number is not neutralized).
pub fn starts_a_formula(cell: &str) -> bool {
    cell.starts_with(FORMULA_LEADERS) && cell.trim().parse::<f64>().is_err()
}

/// `cell` with a leading apostrophe when it would be read as a formula; unchanged otherwise.
/// Idempotent: the apostrophe is not a leader, so a neutralized cell is left as it is.
pub fn neutralize(cell: &str) -> Cow<'_, str> {
    if starts_a_formula(cell) {
        Cow::Owned(format!("'{cell}"))
    } else {
        Cow::Borrowed(cell)
    }
}

/// A record of cells, each [`neutralize`]d, ready for `csv::Writer::write_record`.
pub fn safe_record<I, S>(cells: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    cells
        .into_iter()
        .map(|cell| neutralize(cell.as_ref()).into_owned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_formula_leader_gets_an_apostrophe_and_nothing_else_does() {
        for cell in [
            "=cmd|' /C calc'!A0",
            "+1+1",
            "-2+3",
            "@SUM(A1)",
            "\t=1",
            "\r=1",
            "- run: npm ci",
            "-",
        ] {
            assert_eq!(neutralize(cell), format!("'{cell}"), "{cell:?}");
        }
        for cell in [
            "",
            "plain",
            "a=b",
            "'=already",
            "-3",
            "+7",
            "-1.5e3",
            "0",
            " =x",
        ] {
            assert_eq!(neutralize(cell), cell, "{cell:?}");
        }
    }

    #[test]
    fn neutralizing_twice_changes_nothing() {
        let once = neutralize("=cmd").into_owned();
        assert_eq!(neutralize(&once), once);
    }
}
