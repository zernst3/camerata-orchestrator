//! A row whose severity is held for a decision (owner ruling R-7) tells the client only that it
//! is held. Which reviewer stage doubted it, and why, is the operator's: it appears in the
//! internal workbook and nowhere a client reads. Synthetic rows only.

use std::collections::HashMap;

use super::tests::{empty_opts, finding};
use super::wo9::support::client_options;
use super::*;
use crate::release_gate::{xlsx_sheet_grid, xlsx_sheet_names};
use crate::xlsx_export::vocabulary as v;

/// Words that belong to the operator's triage and never to a client.
const OPERATOR_WORDS: &[&str] = &[
    "reviewer",
    "false alarm",
    "misfit",
    "per-site",
    "group reviewer",
    "judged",
];

fn corpus() -> camerata_rules::RuleSet {
    let mut rule = camerata_rules::bare_rule("SYN-HOLD-TEXT-1", "security");
    rule.min_severity = Some("critical".to_string());
    let mut set = camerata_rules::RuleSet::default();
    set.push(rule);
    set
}

/// A row stage 1 judged not real and the group reviewer called a likely false alarm, after the
/// pipeline's bounds and the pass that follows the reviewers.
fn doubted_report(set: &camerata_rules::RuleSet) -> crate::onboard::SealedReport {
    let mut f = finding("SYN-HOLD-TEXT-1", "src/hold.rs", 7, "low");
    f.kind = Some(camerata_api_types::audit::FindingKind::Defect);
    f.captures.insert(
        crate::ai_audit::STAGE1_CAPTURE.to_string(),
        serde_json::json!({ "real": "no", "reason": "synthetic", "severity": "low" }).to_string(),
    );
    f.review.escalation_reason =
        Some(camerata_api_types::review::EscalationReason::LikelyFalseAlarmAtHighSeverity);
    let mut rows = vec![f];
    crate::severity_bounds::apply_scan_severity_bounds(&mut rows, Some(set));
    crate::follow_reviewers::follow_reviewers(&mut rows, Some(set));
    assert_eq!(rows[0].severity, "low");
    assert!(rows[0].needs_review);
    super::tests::report_with(rows, vec!["SYN-HOLD-TEXT-1"]).seal()
}

/// Every cell under the severity-note column of every sheet that has one.
fn severity_notes(xlsx: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    for sheet in xlsx_sheet_names(xlsx).unwrap() {
        // The glossary names the column; it is not a row's note.
        if sheet == v::GLOSSARY {
            continue;
        }
        let grid = xlsx_sheet_grid(xlsx, &sheet).unwrap();
        for (at, header_row) in grid.iter().enumerate() {
            let Some(col) = header_row.iter().position(|h| h == v::H_SEVERITY_NOTE) else {
                continue;
            };
            for row in grid[at + 1..]
                .iter()
                .take_while(|r| r.iter().any(|c| !c.is_empty()))
            {
                if let Some(cell) = row.get(col).filter(|c| !c.is_empty()) {
                    out.push(cell.clone());
                }
            }
        }
    }
    out
}

/// Every cell of every sheet, for the operator's detail.
fn every_cell(xlsx: &[u8]) -> String {
    let mut out = String::new();
    for sheet in xlsx_sheet_names(xlsx).unwrap() {
        for row in xlsx_sheet_grid(xlsx, &sheet).unwrap() {
            for cell in row {
                out.push_str(&cell);
                out.push('\n');
            }
        }
    }
    out.to_ascii_lowercase()
}

fn workbook(
    sealed: &crate::onboard::SealedReport,
    set: &camerata_rules::RuleSet,
    internal: bool,
) -> Vec<u8> {
    let reviewed = ReviewedReport::unreviewed(sealed, &HashMap::new());
    let options = ReportOptions {
        internal,
        ..client_options()
    };
    crate::xlsx_export::build_workbook(&reviewed, Some(set), &options).expect("the workbook builds")
}

fn assert_neutral(what: &str, text: &str) {
    let lower = text.to_ascii_lowercase();
    for word in OPERATOR_WORDS {
        assert!(!lower.contains(word), "{what} says {word:?}: {text}");
    }
}

#[test]
fn a_client_workbook_row_says_only_that_the_severity_is_held() {
    let set = corpus();
    let notes = severity_notes(&workbook(&doubted_report(&set), &set, false));
    assert!(!notes.is_empty(), "the row carries a severity note");
    for note in &notes {
        assert_neutral("the client workbook's severity note", note);
    }
    assert!(
        notes
            .iter()
            .any(|n| n == "Severity held at low pending a decision."),
        "{notes:?}"
    );
}

#[test]
fn a_client_pdf_row_says_only_that_the_severity_is_held() {
    let set = corpus();
    let sealed = doubted_report(&set);
    let json = build_report_json(
        &ReviewedReport::unreviewed(&sealed, &HashMap::new()),
        Some(&set),
        &empty_opts(),
    );
    let notes: Vec<String> = json
        .curated_findings
        .iter()
        .chain(json.held_for_review_findings.iter())
        .flat_map(|g| g.sites.iter())
        .flat_map(|s| s.severity_adjustments.clone())
        .collect();
    assert!(!notes.is_empty(), "the row carries a severity note");
    for note in &notes {
        assert_neutral("the PDF's severity note", note);
    }
    assert!(
        notes
            .iter()
            .any(|n| n == "Severity held at low pending a decision."),
        "{notes:?}"
    );
}

#[test]
fn the_internal_workbook_carries_the_reviewer_detail() {
    let set = corpus();
    let sealed = doubted_report(&set);
    let internal = every_cell(&workbook(&sealed, &set, true));
    assert!(internal.contains("judged it not real"), "{internal}");
    assert!(internal.contains("minimum severity"), "{internal}");
    // The client workbook has none of it anywhere a row is.
    let client = every_cell(&workbook(&sealed, &set, false));
    assert!(!client.contains("judged it not real"), "{client}");
}

/// A row the floor raised and a reviewer's doubt sent back to its reviewed severity: the client
/// reads one neutral sentence, with no raise and no reviewer in it.
#[test]
fn a_client_workbook_row_returned_to_its_reviewed_severity_says_so_in_one_sentence() {
    let set = corpus();
    let mut f = finding("SYN-HOLD-TEXT-1", "src/return.rs", 9, "low");
    f.kind = Some(camerata_api_types::audit::FindingKind::Defect);
    f.captures.insert(
        crate::ai_audit::STAGE1_CAPTURE.to_string(),
        serde_json::json!({ "real": "yes", "reason": "synthetic", "severity": "low" }).to_string(),
    );
    let mut rows = vec![f];
    crate::severity_bounds::apply_scan_severity_bounds(&mut rows, Some(&set));
    assert_eq!(rows[0].severity, "critical", "the floor raised it");
    rows[0].review.escalation_reason =
        Some(camerata_api_types::review::EscalationReason::RuleMisfit);
    crate::follow_reviewers::follow_reviewers(&mut rows, Some(&set));
    assert_eq!(rows[0].severity, "low");
    let sealed = super::tests::report_with(rows, vec!["SYN-HOLD-TEXT-1"]).seal();
    let notes = severity_notes(&workbook(&sealed, &set, false));
    assert!(!notes.is_empty(), "the row carries a severity note");
    for note in &notes {
        assert_eq!(note, "Severity returned to low pending a decision.");
    }
}
