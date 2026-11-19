//! M4: an INTERNAL export (the operator's own review, never a client's) may carry rows held for a
//! reviewer's decision, each marked held in its workbook; a CLIENT export with the same rows is
//! refused until each has a decision. The review flags the engine left on a row are shown in the
//! internal workbook's "Review flags" column and appear nowhere in a client package. A real
//! package is built through `assemble_package` around a stand-in PDF, so these run without Typst.

use std::collections::HashMap;

use super::tests::{client_options, scan_report};
use super::*;
use crate::onboard::evidence_check::EVIDENCE_UNVERIFIED_CAPTURE;
use crate::onboard::{Finding, ScanReport};
use crate::report_export::{build_report_json, render_data_json, ReportOptions};

fn package_with(report: ScanReport, options: &ReportOptions) -> ProductPackage {
    let sealed = report.seal();
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&sealed, &HashMap::new()),
        None,
        options,
    );
    let data = render_data_json(&json).expect("data.json renders");
    assemble_package(
        &crate::report_export::ReviewedReport::unreviewed(&sealed, &HashMap::new()),
        None,
        options,
        "acme-inspection",
        (json, data, b"%PDF-1.7 stand-in for the gate tests".to_vec()),
    )
    .expect("the package assembles")
}

fn internal_options() -> ReportOptions {
    ReportOptions {
        internal: true,
        ..client_options()
    }
}

fn row(rule_id: &str, detail: &str) -> Finding {
    Finding {
        repo: "acme/portal".to_string(),
        path: "lib/supabase/admin.ts".to_string(),
        line: 8,
        rule_id: rule_id.to_string(),
        severity: "low".to_string(),
        snippet: "db.query(sql)".to_string(),
        detail: detail.to_string(),
        ..Finding::default()
    }
}

fn unverified() -> Finding {
    let mut f = row("AI-RAW-SQL", "Raw SQL \u{2014} Built from input.");
    f.captures.insert(
        EVIDENCE_UNVERIFIED_CAPTURE.to_string(),
        "the snippet was not found within 3 lines of line 8".to_string(),
    );
    f
}

fn junk() -> Finding {
    row(
        "AI-SERVICE-ROLE",
        "Service role key \u{2014} I cannot determine this.",
    )
}

fn flagged() -> Finding {
    let mut f = row(
        "AI-OPEN-REDIRECT",
        "Open redirect \u{2014} The target comes from the query.",
    );
    f.captures.insert(
        crate::review_flags::REVIEW_FLAGS_CAPTURE.to_string(),
        r#"["fix_ungrounded"]"#.to_string(),
    );
    f
}

fn report_with(rows: Vec<Finding>) -> ScanReport {
    let mut report = scan_report();
    report.findings.extend(rows);
    report
}

fn workbook_of(zip: &[u8]) -> Vec<u8> {
    read_zip_entries(zip)
        .expect("zip")
        .into_iter()
        .find(|(name, _)| name.ends_with(".xlsx"))
        .map(|(_, bytes)| bytes)
        .expect("the package holds a workbook")
}

/// An internal export carries the held rows and marks each held; the same rows in a client export
/// are refused.
#[test]
fn an_internal_export_carries_held_rows_marked_held_and_a_client_export_is_refused() {
    let rows = || report_with(vec![unverified(), junk()]);

    let client = release(package_with(rows(), &client_options()));
    assert!(
        matches!(client, Err(ReleaseError::Blocked(_))),
        "a client export with held rows is refused: {:?}",
        client.map(|z| z.stem)
    );

    let internal = release_internal(package_with(rows(), &internal_options()))
        .expect("an internal export may carry held rows");
    let grid = xlsx_sheet_grid(&workbook_of(&internal.bytes), "All problems").unwrap();
    let held_col = grid[0]
        .iter()
        .position(|h| h == "Held")
        .expect("the internal workbook has a Held column");
    let held: Vec<&String> = grid[1..]
        .iter()
        .map(|r| &r[held_col])
        .filter(|c| !c.is_empty())
        .collect();
    // Both rows are at one place, so All problems has one row for them and its Held cell names
    // both reasons: no held row goes unmarked.
    assert_eq!(held.len(), 1, "{grid:?}");
    assert!(held.iter().any(|h| h.contains("evidence not verified")));
    assert!(held.iter().any(|h| h.contains("junk text")));
}

/// The internal workbook has a "Review flags" column; no client artifact has the column or any
/// flag text anywhere.
#[test]
fn review_flags_are_in_the_internal_workbook_and_nowhere_in_a_client_package() {
    let rows = || report_with(vec![flagged()]);

    let internal = release_internal(package_with(rows(), &internal_options())).expect("internal");
    let grid = xlsx_sheet_grid(&workbook_of(&internal.bytes), "All problems").unwrap();
    let col = grid[0]
        .iter()
        .position(|h| h == "Review flags")
        .expect("the internal workbook has a Review flags column");
    assert!(
        grid[1..].iter().any(|r| r[col] == "fix_ungrounded"),
        "{grid:?}"
    );

    let client = release(package_with(rows(), &client_options())).expect("client");
    let entries = read_zip_entries(&client.bytes).expect("zip");
    for (name, bytes) in &entries {
        let text = if name.ends_with(".xlsx") {
            xlsx_strings(bytes).expect("strings").join("\n")
        } else {
            String::from_utf8_lossy(bytes).into_owned()
        };
        assert!(!text.contains("fix_ungrounded"), "flag text in {name}");
        assert!(!text.contains("Review flags"), "column in {name}");
        assert!(!text.contains("review_flags"), "capture name in {name}");
    }
    let grid = xlsx_sheet_grid(&workbook_of(&client.bytes), "All problems").unwrap();
    assert!(!grid[0].iter().any(|h| h == "Review flags" || h == "Held"));
}
