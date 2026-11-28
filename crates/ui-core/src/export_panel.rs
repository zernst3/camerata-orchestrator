//! The words of the export panel, built from the package's own names.
//!
//! The package is built by the server (`xlsx_export::vocabulary` holds its sheet and section
//! names). This crate cannot read that module, so the names the panel uses are listed here and a
//! server test (`export_panel_names`) proves every one exists in the package: every section is in
//! `PDF_SECTIONS`, every sheet is in `SHEETS`, every file is written by the package writer and
//! named by its README, and the panel's text holds no word the package retired. Add a name here
//! only after the package has it.

/// The title of the panel's help topic.
pub const HELP_TITLE: &str = "Export product";

/// The panel's heading.
pub const HEADING: &str = "Export product (ZIP: PDF + Excel + JSON)";

/// The PDF sections the panel names, spelled as the PDF names them.
pub const NAMED_PDF_SECTIONS: [&str; 8] = [
    "Executive summary",
    "Area scorecard",
    "Severity by effort matrix",
    "The most serious problems",
    "The other fixes",
    "What is healthy",
    "Dependencies",
    "How this was done and its limits",
];

/// The workbook sheets the panel names, spelled as the workbook names them.
pub const NAMED_SHEETS: [&str; 7] = [
    "Start here",
    "Fix plan",
    "All problems",
    "Needs a decision",
    "Ruled out",
    "Not checked",
    "Reviewer decisions",
];

/// The files besides the PDF and the workbook the panel names, by their path in the ZIP.
pub const NAMED_FILES: [&str; 5] = camerata_api_types::package_paths::NAMED_FILES;

/// The line the panel shows when the release gate refuses, with the table's and Review's share of
/// the rows named. `rows` is how many rows need a decision, `in_review` how many of them are not
/// in the triage table and so are only in Review, and `groups` and `group_sites` describe group
/// decisions that miss records (always in Review).
pub fn blocked_line(rows: usize, in_review: usize, groups: usize, group_sites: usize) -> String {
    let mut line = String::from("Export blocked:");
    if rows > 0 {
        line.push_str(&format!(
            " {} held {} a decision",
            thousands(rows),
            if rows == 1 { "row needs" } else { "rows need" }
        ));
        if in_review > 0 {
            line.push_str(&format!(
                " ({} {} in Review)",
                if in_review == rows {
                    if rows == 1 {
                        "it".to_string()
                    } else {
                        "all".to_string()
                    }
                } else {
                    thousands(in_review)
                },
                if in_review == 1 { "is" } else { "are" }
            ));
        }
        line.push('.');
    }
    if group_sites > 0 {
        line.push_str(&format!(
            " {} group {} missing records for {} {}.",
            thousands(groups),
            if groups == 1 {
                "decision is"
            } else {
                "decisions are"
            },
            thousands(group_sites),
            if group_sites == 1 { "place" } else { "places" }
        ));
    }
    line
}

fn thousands(n: usize) -> String {
    crate::tree::thousands(n)
}

/// Names joined with commas and a final "and", no serial comma: `A`, `A and B`, `A, B and C`.
fn list(names: &[&str]) -> String {
    match names {
        [] => String::new(),
        [one] => (*one).to_string(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// The help dialog's text: what the package holds, from the constants above. Paragraphs are
/// separated by a blank line.
pub fn help_body() -> String {
    let [findings, decisions, tickets, manifest, readme] = NAMED_FILES;
    [
        "One ZIP from one inspection: a PDF report, an Excel workbook and the files below. \
         Everything in it comes from the same inspection and from the decisions stored for the \
         project."
            .to_string(),
        format!(
            "The PDF is written for a board, a buyer or a decision maker. It has the {}.",
            list(&NAMED_PDF_SECTIONS)
        ),
        format!(
            "The workbook is the working list for the engineers who do the fixes. Open the {} \
             sheet first. The {} sheet has one row for each fix, in the order to do them. The \
             {} sheet has every problem once. The {} sheet holds the problems a reviewer must \
             rule on before anyone acts on them. The {} sheet gives the reason each problem was \
             ruled out and the {} sheet lists what was not checked, so nothing is dropped \
             silently. The {} sheet lists every decision a reviewer recorded.",
            NAMED_SHEETS[0],
            NAMED_SHEETS[1],
            NAMED_SHEETS[2],
            NAMED_SHEETS[3],
            NAMED_SHEETS[4],
            NAMED_SHEETS[5],
            NAMED_SHEETS[6],
        ),
        format!(
            "{findings} holds every row that was not ruled out, for scripts and other tools. \
             {decisions} holds every decision record the report was reviewed with. {tickets} has \
             one ticket for each fix, for a Jira, Linear or GitHub Issues importer. {manifest} \
             lists the files with a checksum for each and {readme} says what each file is."
        ),
        "The export is refused while any row needs a decision. Decide those rows first. The \
         triage log is not part of this package: it is an internal file you download from the \
         operator files block."
            .to_string(),
    ]
    .join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_blocked_line_counts_rows_and_says_how_many_are_in_review() {
        assert_eq!(
            blocked_line(8, 0, 0, 0),
            "Export blocked: 8 held rows need a decision."
        );
        assert_eq!(
            blocked_line(1, 0, 0, 0),
            "Export blocked: 1 held row needs a decision."
        );
        assert_eq!(
            blocked_line(8, 3, 0, 0),
            "Export blocked: 8 held rows need a decision (3 are in Review)."
        );
        assert_eq!(
            blocked_line(8, 8, 0, 0),
            "Export blocked: 8 held rows need a decision (all are in Review)."
        );
        assert_eq!(
            blocked_line(1, 1, 0, 0),
            "Export blocked: 1 held row needs a decision (it is in Review)."
        );
        assert_eq!(
            blocked_line(2, 1, 0, 0),
            "Export blocked: 2 held rows need a decision (1 is in Review)."
        );
        assert_eq!(
            blocked_line(0, 0, 2, 5),
            "Export blocked: 2 group decisions are missing records for 5 places."
        );
    }

    #[test]
    fn the_names_are_plain_text_with_no_dashes() {
        let all: Vec<&str> = NAMED_PDF_SECTIONS
            .iter()
            .chain(&NAMED_SHEETS)
            .chain(&NAMED_FILES)
            .copied()
            .collect();
        for name in all {
            assert!(!name.contains(['\u{2013}', '\u{2014}']), "{name}");
        }
    }

    #[test]
    fn the_help_body_names_every_listed_section_sheet_and_file() {
        let body = help_body();
        for name in NAMED_PDF_SECTIONS
            .iter()
            .chain(&NAMED_SHEETS)
            .chain(&NAMED_FILES)
        {
            assert!(body.contains(name), "the body does not name {name}: {body}");
        }
    }

    #[test]
    fn the_help_body_states_the_truth_about_drafts_and_the_triage_log() {
        let body = help_body();
        assert!(
            body.contains("The export is refused while any row needs a decision."),
            "{body}"
        );
        assert!(!body.contains("draft mid-engagement"), "{body}");
        assert!(!body.contains("category scorecard"), "{body}");
        assert!(!body.contains("per-category"), "{body}");
        assert!(!body.contains("False Positives"), "{body}");
        assert!(!body.contains("curated findings"), "{body}");
    }

    #[test]
    fn a_list_reads_with_and_before_the_last_name_and_no_serial_comma() {
        assert_eq!(list(&["A"]), "A");
        assert_eq!(list(&["A", "B"]), "A and B");
        assert_eq!(list(&["A", "B", "C"]), "A, B and C");
    }
}
