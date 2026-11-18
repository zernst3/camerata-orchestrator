//! Why a problem matters, said twice: in business terms and in technical terms. The workbook
//! carries both; the PDF's data carries only the business text; the reviewer's summary of a
//! sub-group reaches no client file.

use super::support::*;
use crate::release_gate::{read_zip_entries, xlsx_sheet_grid};
use crate::report_export::apply_overlay;

const SUMMARY_MARKER: &str = "REVIEWER-SUMMARY-MARKER";
const BUSINESS_MARKER: &str = "GROUP-BUSINESS-MARKER";
const TECHNICAL_MARKER: &str = "GROUP-TECHNICAL-MARKER";
const ROW_BUSINESS_MARKER: &str = "ROW-BUSINESS-MARKER";
const ROW_TECHNICAL_MARKER: &str = "ROW-TECHNICAL-MARKER";
const SQL_SUB: &str = "sub:SEC-NO-RAW-SQL-CONCAT-1/0";

fn workbook(entries: &std::collections::BTreeMap<String, Vec<u8>>) -> Vec<u8> {
    entries
        .iter()
        .find(|(n, _)| n.ends_with(".xlsx"))
        .map(|(_, b)| b.clone())
        .expect("a workbook")
}

/// The mixed report whose SQL sub-group carries distinct marker text in its reviewer summary and
/// in both client reasons, and whose first SQL row carries finder text for both reasons.
fn marked() -> crate::onboard::SealedReport {
    let mut report = mixed_sealed().into_inner();
    let tree = report.tree.as_mut().expect("a tree");
    let node = tree
        .nodes
        .iter_mut()
        .find(|n| n.id == SQL_SUB)
        .expect("the SQL sub-group");
    node.summary = Some(format!("{SUMMARY_MARKER}, src/a.rs:10 is an outlier."));
    node.why_business = Some(format!("{BUSINESS_MARKER} data can quietly go wrong."));
    node.why_technical = Some(format!("{TECHNICAL_MARKER} a write skips the check."));
    node.cause = Some("the query is built from a request value".to_string());
    let row = report
        .findings
        .iter_mut()
        .find(|f| f.path == "src/a.rs")
        .expect("a SQL row");
    row.captures.insert(
        crate::ai_audit::agent_finder::reply::FINDER_BUSINESS_CAPTURE.to_string(),
        format!("{ROW_BUSINESS_MARKER} someone new may misread it."),
    );
    row.captures.insert(
        crate::ai_audit::agent_finder::reply::FINDER_WHY_CAPTURE.to_string(),
        format!("{ROW_TECHNICAL_MARKER} the query breaks on a quote."),
    );
    report.seal()
}

fn cell<'a>(grid: &[Vec<String>], row: &'a [String], header: &str) -> &'a str {
    let col = grid[0]
        .iter()
        .position(|h| h == header)
        .unwrap_or_else(|| panic!("no {header:?} column in {:?}", grid[0]));
    &row[col]
}

/// Every client-visible text of a released package, the PDF's data and every workbook string
/// included.
fn client_text(reviewed: &crate::report_export::ReviewedReport) -> String {
    let package = package_of(reviewed, &client_options());
    let mut text = package.data_json.clone();
    let entries = read_zip_entries(
        &crate::release_gate::release(package)
            .expect("the package releases")
            .bytes,
    )
    .unwrap();
    for (name, bytes) in &entries {
        if name.ends_with(".xlsx") {
            text.push_str(&crate::release_gate::xlsx_strings(bytes).unwrap().join("\n"));
        } else if !name.ends_with(".pdf") {
            text.push_str(&String::from_utf8_lossy(bytes));
        }
    }
    text
}

#[test]
fn the_reviewers_summary_reaches_no_client_file() {
    let reviewed = apply_overlay(&marked(), &[], &[], &[]);
    let text = client_text(&reviewed);
    assert!(
        !text.contains(SUMMARY_MARKER),
        "a client file carries the reviewer's summary"
    );
    // The client reasons are there.
    assert!(text.contains(BUSINESS_MARKER));
}

#[test]
fn the_fix_plan_says_why_it_matters_to_the_business_and_technically() {
    let sealed = marked();
    let reviewed = apply_overlay(&sealed, &[], &[], &[]);
    let entries = released_entries(&reviewed, &client_options());
    let xlsx = workbook(&entries);
    let grid = xlsx_sheet_grid(&xlsx, "Fix plan").expect("the plan sheet");
    assert!(
        !grid[0].iter().any(|h| h == "Why it matters"),
        "the single column is retired: {:?}",
        grid[0]
    );
    let row = grid
        .iter()
        .skip(1)
        .find(|r| {
            cell(&grid, r, "Rules").contains("SEC-NO-RAW-SQL-CONCAT-1")
                || cell(&grid, r, "What is wrong").contains("request value")
        })
        .expect("the SQL fix row");
    assert!(cell(&grid, row, "Why it matters to the business").contains(BUSINESS_MARKER));
    assert!(cell(&grid, row, "Why it matters technically").contains(TECHNICAL_MARKER));
    assert!(!cell(&grid, row, "Why it matters to the business").contains(TECHNICAL_MARKER));
    assert!(!cell(&grid, row, "Why it matters technically").contains(BUSINESS_MARKER));
}

#[test]
fn all_problems_carries_both_reasons_of_each_row_and_empty_cells_where_there_are_none() {
    let sealed = marked();
    let reviewed = apply_overlay(&sealed, &[], &[], &[]);
    let entries = released_entries(&reviewed, &client_options());
    let xlsx = workbook(&entries);
    let grid = xlsx_sheet_grid(&xlsx, "All problems").expect("the sheet");
    let at = |path: &str| {
        grid.iter()
            .skip(1)
            .find(|r| cell(&grid, r, "File") == path)
            .unwrap_or_else(|| panic!("no row at {path}"))
            .clone()
    };
    let a = at("src/a.rs");
    assert!(cell(&grid, &a, "Why it matters to the business").contains(ROW_BUSINESS_MARKER));
    assert!(cell(&grid, &a, "Why it matters technically").contains(ROW_TECHNICAL_MARKER));
    let b = at("src/b.rs");
    assert_eq!(cell(&grid, &b, "Why it matters to the business"), "");
    assert_eq!(cell(&grid, &b, "Why it matters technically"), "");
    // "What is wrong" of the Fix plan and "Problem" here are not changed by the new columns.
    assert!(grid[0].iter().any(|h| h == "Problem"));
}

#[test]
fn the_glossary_names_both_reasons_and_not_the_retired_single_one() {
    let sealed = marked();
    let reviewed = apply_overlay(&sealed, &[], &[], &[]);
    let entries = released_entries(&reviewed, &client_options());
    let xlsx = workbook(&entries);
    let strings = crate::release_gate::xlsx_strings(&xlsx).unwrap();
    assert!(strings
        .iter()
        .any(|s| s == "Why it matters to the business"));
    assert!(strings.iter().any(|s| s == "Why it matters technically"));
    assert!(!strings.iter().any(|s| s == "Why it matters"));
}

#[test]
fn the_pdfs_data_carries_only_the_business_text() {
    let sealed = marked();
    let reviewed = apply_overlay(&sealed, &[], &[], &[]);
    let package = package_of(&reviewed, &client_options());
    let data = &package.data_json;
    assert!(
        data.contains(BUSINESS_MARKER),
        "the business text is printed"
    );
    for marker in [SUMMARY_MARKER, TECHNICAL_MARKER, ROW_TECHNICAL_MARKER] {
        assert!(!data.contains(marker), "the PDF's data carries {marker}");
    }
}

#[test]
fn a_withheld_business_text_leaves_an_empty_client_cell_and_a_reviewer_flag() {
    let mut report = mixed_sealed().into_inner();
    let node = report
        .tree
        .as_mut()
        .expect("a tree")
        .nodes
        .iter_mut()
        .find(|n| n.id == SQL_SUB)
        .expect("the SQL sub-group");
    node.why_business =
        Some("LEAK-MARKER the scanner flagged these as regex-scan hits.".to_string());
    let reviewed = apply_overlay(&report.seal(), &[], &[], &[]);
    // The client sees nothing of it.
    let text = client_text(&reviewed);
    assert!(!text.contains("LEAK-MARKER"));
    // The reviewer sees the flag beside the node.
    let internal = crate::report_export::tree_view::tree_view(&reviewed, true);
    let flags = &internal
        .iter()
        .find(|n| n.id == SQL_SUB)
        .and_then(|n| n.internal.as_ref())
        .expect("the internal triage")
        .flags;
    assert!(
        flags
            .iter()
            .any(|f| f.starts_with("why_business:methodology_leak:")),
        "{flags:?}"
    );
}

// ── The reviewer can edit every reason before export ─────────────────────────────────────────

const EDIT_BUSINESS: &str = "EDITED-BUSINESS-MARKER the client may lose orders.";
const EDIT_TECHNICAL: &str = "EDITED-TECHNICAL-MARKER the write skips the check.";
const EDIT_ROW: &str = "EDITED-ROW-BUSINESS-MARKER a new hire may copy it.";

fn node_edit(
    sealed: &crate::onboard::SealedReport,
    n: usize,
    field: camerata_api_types::edits::EditField,
    new: &str,
) -> camerata_api_types::edits::Edit {
    use camerata_api_types::edits::EditField;
    let node = sealed.tree.as_ref().unwrap().node(SQL_SUB).unwrap();
    let model = match field {
        EditField::WhyBusiness => node.why_business.clone(),
        _ => node.why_technical.clone(),
    }
    .unwrap_or_default();
    camerata_api_types::edits::Edit {
        id: format!("edit-{n:06}"),
        key: SQL_SUB.to_string(),
        field,
        old: model.clone(),
        new: new.to_string(),
        reason: "SECRET-REASON the wording was wrong".to_string(),
        by: "SECRET-REVIEWER".to_string(),
        at: "2026-11-17T00:00:00Z".to_string(),
        run_id: "insp-1".to_string(),
        fingerprint_at_edit: crate::report_export::summary_fingerprint(&model),
        stale: false,
        discarded_at: None,
        reapplied_from: None,
    }
}

fn fix_plan_cell(reviewed: &crate::report_export::ReviewedReport, header: &str) -> String {
    let entries = released_entries(reviewed, &client_options());
    let grid = xlsx_sheet_grid(&workbook(&entries), "Fix plan").expect("plan");
    let row = grid
        .iter()
        .skip(1)
        .find(|r| cell(&grid, r, "What is wrong").contains("request value"))
        .expect("the SQL fix row");
    cell(&grid, row, header).to_string()
}

#[test]
fn an_edit_of_the_business_reason_reaches_the_fix_plan_and_the_pdf_data() {
    use camerata_api_types::edits::EditField;
    let sealed = marked();
    let edit = node_edit(&sealed, 1, EditField::WhyBusiness, EDIT_BUSINESS);
    let reviewed = apply_overlay(&sealed, &[], std::slice::from_ref(&edit), &[]);
    assert_eq!(
        fix_plan_cell(&reviewed, "Why it matters to the business"),
        EDIT_BUSINESS
    );
    let data = package_of(&reviewed, &client_options()).data_json;
    assert!(data.contains("EDITED-BUSINESS-MARKER"));
    assert!(
        !data.contains(BUSINESS_MARKER),
        "the edit replaces the model text"
    );
    // The reviewer's reason and name reach no client file.
    let text = client_text(&reviewed);
    assert!(!text.contains("SECRET-REASON") && !text.contains("SECRET-REVIEWER"));
}

#[test]
fn an_edit_of_the_technical_reason_reaches_the_workbook_only() {
    use camerata_api_types::edits::EditField;
    let sealed = marked();
    let edit = node_edit(&sealed, 1, EditField::WhyTechnical, EDIT_TECHNICAL);
    let reviewed = apply_overlay(&sealed, &[], std::slice::from_ref(&edit), &[]);
    assert_eq!(
        fix_plan_cell(&reviewed, "Why it matters technically"),
        EDIT_TECHNICAL
    );
    let data = package_of(&reviewed, &client_options()).data_json;
    assert!(!data.contains("EDITED-TECHNICAL-MARKER"));
}

#[test]
fn an_edit_of_a_rows_business_text_reaches_the_all_problems_cell() {
    use camerata_api_types::edits::EditField;
    let sealed = marked();
    let row = row_at(&sealed, "src/a.rs");
    let mut edit = edit_of(1, &row, EditField::Business, EDIT_ROW);
    edit.old = ROW_BUSINESS_MARKER.to_string();
    let reviewed = apply_overlay(&sealed, &[], std::slice::from_ref(&edit), &[]);
    let entries = released_entries(&reviewed, &client_options());
    let grid = xlsx_sheet_grid(&workbook(&entries), "All problems").expect("sheet");
    let r = grid
        .iter()
        .skip(1)
        .find(|r| cell(&grid, r, "File") == "src/a.rs")
        .expect("row");
    assert_eq!(cell(&grid, r, "Why it matters to the business"), EDIT_ROW);
}

#[test]
fn an_edit_of_a_reason_without_a_reason_is_refused() {
    use crate::edits::{EditCtx, EditDraft, EditStore, EditStoreError};
    use camerata_api_types::edits::EditField;
    for field in [
        EditField::WhyBusiness,
        EditField::WhyTechnical,
        EditField::Business,
    ] {
        let draft = |reason: &str| EditDraft {
            key: "k".to_string(),
            field,
            old: "old".to_string(),
            new: "new".to_string(),
            reason: reason.to_string(),
            by: String::new(),
            run_id: String::new(),
            fingerprint: "fp".to_string(),
        };
        let ctx = || EditCtx {
            run_id: "insp-1",
            now: "2026-11-17T00:00:00Z",
        };
        let store = EditStore::new();
        assert!(matches!(
            store.record_edit("p", draft("  "), ctx()),
            Err(EditStoreError::ReasonRequired(_))
        ));
        assert!(store.record_edit("p", draft("because"), ctx()).is_ok());
    }
}
