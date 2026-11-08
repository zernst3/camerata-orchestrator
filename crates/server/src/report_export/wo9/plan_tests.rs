//! WO9 and WO9b: the Fix plan sheet and the ticket CSV, one row per fix, with ruled-out
//! problems and ignored places left out (design 3B items 2 and 4, D16).

use camerata_api_types::decisions::DecisionState;

use super::support::*;
use crate::release_gate::{xlsx_sheet_grid, xlsx_sheet_names};
use crate::report_export::apply_overlay;

fn workbook(entries: &std::collections::BTreeMap<String, Vec<u8>>) -> Vec<u8> {
    entries
        .iter()
        .find(|(n, _)| n.ends_with(".xlsx"))
        .map(|(_, b)| b.clone())
        .expect("a workbook")
}

/// The mixed report with the primary SQL row's own fix, a site ignored and a site a false
/// positive.
fn reviewed_for_plan() -> (
    crate::report_export::ReviewedReport,
    crate::onboard::SealedReport,
) {
    let mut report = mixed_sealed().into_inner();
    report.findings[0].fix_specific = Some("Bind the parameters with the driver.".to_string());
    let sealed = report.seal();
    let records = vec![
        decision(
            "dec-000001",
            &key_at(&sealed, "src/c.rs"),
            DecisionState::Ignored,
            "accepted for now",
        ),
        decision(
            "dec-000002",
            &key_at(&sealed, "src/e.rs"),
            DecisionState::FalsePositive,
            "the layer is allowed here",
        ),
    ];
    (apply_overlay(&sealed, &records, &[], &[]), sealed)
}

fn cell<'a>(grid: &[Vec<String>], row: &'a [String], header: &str) -> &'a str {
    let col = grid[0]
        .iter()
        .position(|h| h == header)
        .unwrap_or_else(|| panic!("no {header:?} column in {:?}", grid[0]));
    &row[col]
}

#[test]
#[ignore = "WO-B: the workbook's own strings in xlsx_export.rs still carry dashes, so a real package is refused until that branch lands"]
fn the_remediation_plan_has_one_row_per_subgroup_and_excludes_false_positives_and_ignored() {
    let (reviewed, sealed) = reviewed_for_plan();
    let entries = released_entries(&reviewed, &client_options());
    let xlsx = workbook(&entries);

    // It is the first sheet of the "What to fix" group, after Start here and the Glossary.
    let names = xlsx_sheet_names(&xlsx).expect("sheet names");
    assert_eq!(names[2], "Fix plan", "{names:?}");

    let grid = xlsx_sheet_grid(&xlsx, "Fix plan").expect("the plan sheet");
    for header in [
        "Fix id",
        "Rules",
        "What is wrong",
        "Severity",
        "Places",
        "Fix",
        "Example from your code",
        "Effort",
        "Folders",
        "Fix order",
    ] {
        assert!(
            grid[0].iter().any(|h| h == header),
            "{header}: {:?}",
            grid[0]
        );
    }
    // One row per sub-group that has something left to fix: the SQL group (one site ignored),
    // the layering group (one site a false positive) and the dependency advisory.
    let rows: Vec<&Vec<String>> = grid.iter().skip(1).collect();
    assert_eq!(rows.len(), 3, "{rows:?}");
    let sql_group = sealed.findings[0].group_path[2].clone();
    let layering_group = sealed
        .findings
        .iter()
        .find(|f| f.path == "src/d.rs")
        .unwrap()
        .group_path[2]
        .clone();
    let row_of = |group: &str| -> &[String] {
        rows.iter()
            .find(|r| cell(&grid, r, "Fix id") == group)
            .map(|r| r.as_slice())
            .unwrap_or_else(|| panic!("no plan row for {group}"))
    };
    let sql = row_of(&sql_group);
    assert_eq!(
        cell(&grid, sql, "Places"),
        "2",
        "the ignored site is not a site to fix"
    );
    assert_eq!(cell(&grid, sql, "Severity"), "High");
    assert_eq!(
        cell(&grid, sql, "Example from your code"),
        "Bind the parameters with the driver."
    );
    assert_eq!(cell(&grid, sql, "Fix order"), "First");
    assert!(cell(&grid, sql, "Folders").contains("src (2)"), "{sql:?}");
    let layering = row_of(&layering_group);
    assert_eq!(
        cell(&grid, layering, "Places"),
        "1",
        "the false positive is excluded"
    );
    assert_eq!(cell(&grid, layering, "Fix order"), "Next");
    // First leads.
    assert_eq!(cell(&grid, rows[0], "Fix order"), "First");
    assert_eq!(cell(&grid, rows[rows.len() - 1], "Fix order"), "Next");
    // Neither the false positive nor the ignored site is named anywhere on the sheet.
    let sheet_text: String = grid
        .iter()
        .flatten()
        .cloned()
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!sheet_text.contains("src/e.rs"), "{sheet_text}");
    assert!(!sheet_text.contains("accepted for now"), "{sheet_text}");
}

#[test]
#[ignore = "WO-B: the workbook's own strings in xlsx_export.rs still carry dashes, so a real package is refused until that branch lands"]
fn ticket_csv_has_one_row_per_subgroup_and_excludes_false_positives() {
    let (reviewed, sealed) = reviewed_for_plan();
    let entries = released_entries(&reviewed, &client_options());
    let mut reader = csv::Reader::from_reader(entries["csv/tickets.csv"].as_slice());
    let headers: Vec<String> = reader.headers().unwrap().iter().map(String::from).collect();
    assert_eq!(
        headers,
        [
            "Title",
            "Description",
            "Priority",
            "Labels",
            "Estimate",
            "Group id",
            "Site count",
            "Sites"
        ]
    );
    let rows: Vec<csv::StringRecord> = reader.records().map(|r| r.unwrap()).collect();
    assert_eq!(
        rows.len(),
        3,
        "one ticket per sub-group with something to fix"
    );
    let sql_group = sealed.findings[0].group_path[2].clone();
    let sql = rows
        .iter()
        .find(|r| &r[5] == sql_group.as_str())
        .expect("the SQL ticket");
    assert_eq!(&sql[2], "High");
    assert_eq!(&sql[6], "2");
    assert!(
        sql[7].contains("src/a.rs:10") && sql[7].contains("src/b.rs:20"),
        "{:?}",
        &sql[7]
    );
    assert!(
        !sql[7].contains("src/c.rs"),
        "the ignored site is not on the ticket"
    );
    assert!(
        sql[1].contains("Bind the parameters with the driver."),
        "{:?}",
        &sql[1]
    );
    for row in &rows {
        assert!(
            !row[7].contains("src/e.rs"),
            "the false positive is on no ticket"
        );
        assert!(!row[0].is_empty());
    }
    // The package states the file in the manifest.
    let manifest: serde_json::Value = serde_json::from_slice(&entries["manifest.json"]).unwrap();
    assert!(
        manifest["files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["path"] == "csv/tickets.csv"),
        "{manifest}"
    );
    // The README maps the columns to the three importers.
    let readme = String::from_utf8_lossy(&entries["README.txt"]).into_owned();
    for importer in ["Jira", "Linear", "GitHub Issues", "csv/tickets.csv"] {
        assert!(
            readme.contains(importer),
            "the README does not mention {importer}"
        );
    }
}

#[test]
fn a_ticket_lists_fifty_places_and_points_at_the_all_problems_sheet_for_the_rest() {
    let mut report = crate::release_gate::test_scan_report();
    report.findings = (0..60)
        .map(|i| sql_row(&format!("src/handlers/h{i:02}.rs"), 10 + i))
        .collect();
    let sealed = report.seal();
    let group = sealed.findings[0].group_path[2].clone();
    let reviewed = apply_overlay(&sealed, &[], &[], &[]);
    let entries = released_entries(&reviewed, &client_options());
    let mut reader = csv::Reader::from_reader(entries["csv/tickets.csv"].as_slice());
    let rows: Vec<csv::StringRecord> = reader.records().map(|r| r.unwrap()).collect();
    assert_eq!(rows.len(), 1);
    let sites = rows[0][7].to_string();
    assert_eq!(&rows[0][6], "60");
    assert_eq!(sites.matches("src/handlers/").count(), 50, "{sites}");
    assert!(
        sites.ends_with(&format!(
            "and 10 more rows, see the All problems sheet, filter Fix id = {group}"
        )),
        "{sites}"
    );
}

/// The mixed report whose first SQL row also stands for two more places (an older report's
/// merge): the sub-group has three rows and five sites.
fn sealed_with_a_row_standing_for_three_sites() -> crate::onboard::SealedReport {
    let mut report = mixed_sealed().into_inner();
    report.findings[0].also_locations = (0..2)
        .map(|i| {
            crate::onboard::MergedLocation::site(
                "acme/portal".to_string(),
                format!("src/also_{i}.rs"),
                i + 1,
                "SEC-NO-RAW-SQL-CONCAT-1".to_string(),
                format!("let q = format!(\"SELECT {i}\");"),
                false,
            )
        })
        .collect();
    report.seal()
}

#[test]
#[ignore = "WO-B: the workbook's own strings in xlsx_export.rs still carry dashes, so a real package is refused until that branch lands"]
fn plan_ticket_and_pdf_print_the_same_site_count_sites_not_rows() {
    let sealed = sealed_with_a_row_standing_for_three_sites();
    let group = sealed.findings[0].group_path[2].clone();
    let reviewed = apply_overlay(&sealed, &[], &[], &[]);
    let options = client_options();
    let entries = released_entries(&reviewed, &options);

    let grid = xlsx_sheet_grid(&workbook(&entries), "Fix plan").expect("the plan sheet");
    let plan_row = grid
        .iter()
        .skip(1)
        .find(|r| cell(&grid, r, "Fix id") == group)
        .expect("the plan row");
    let plan_sites = cell(&grid, plan_row, "Places").to_string();

    let mut reader = csv::Reader::from_reader(entries["csv/tickets.csv"].as_slice());
    let tickets: Vec<csv::StringRecord> = reader.records().map(|r| r.unwrap()).collect();
    let ticket = tickets
        .iter()
        .find(|r| &r[5] == group.as_str())
        .expect("the ticket");
    let ticket_sites = ticket[6].to_string();

    let json = serde_json::to_value(crate::report_export::build_report_json(
        &reviewed, None, &options,
    ))
    .unwrap();
    let pattern = json["sub_group_patterns"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["group"] == group.as_str())
        .expect("the pattern");
    let pdf_sites = pattern["open_sites"].to_string();

    // Three rows, five sites: all three files say five.
    assert_eq!(pdf_sites, "5", "the PDF counts sites");
    assert_eq!(plan_sites, pdf_sites, "the plan counts sites, not rows");
    assert_eq!(ticket_sites, pdf_sites, "the ticket counts sites, not rows");
    assert!(
        ticket[1].contains("5 places, listed as rows below"),
        "the ticket description counts sites: {:?}",
        &ticket[1]
    );
}

#[test]
#[ignore = "WO-B: the workbook's own strings in xlsx_export.rs still carry dashes, so a real package is refused until that branch lands"]
fn a_report_with_no_tree_says_so_in_the_plan_the_tickets_and_the_pdf_patterns() {
    // A report with no tree: one stored before the review tree existed, exported as it is.
    let sealed = mixed_sealed().overlaid(|r| r.tree = None);
    assert!(sealed.tree.is_none(), "the fixture has no tree");
    let reviewed = apply_overlay(&sealed, &[], &[], &[]);
    let options = client_options();
    let entries = released_entries(&reviewed, &options);

    // The plan sheet: its header, then one row that says why there is no plan.
    let grid = xlsx_sheet_grid(&workbook(&entries), "Fix plan").expect("the plan sheet");
    assert_eq!(grid.len(), 2, "{grid:?}");
    assert!(
        grid[1][0].contains("no review tree"),
        "the plan says so: {:?}",
        grid[1]
    );

    // The ticket file: its header and no ticket row (a note row would import as a ticket); the
    // package README says why.
    let mut reader = csv::Reader::from_reader(entries["csv/tickets.csv"].as_slice());
    assert_eq!(
        reader.headers().unwrap().len(),
        crate::xlsx_export::tickets::TICKET_HEADERS.len(),
        "the tickets file keeps its header"
    );
    let tickets: Vec<csv::StringRecord> = reader.records().map(|r| r.unwrap()).collect();
    assert!(tickets.is_empty(), "no ticket rows: {tickets:?}");
    let readme = String::from_utf8(entries["README.txt"].clone()).unwrap();
    let tickets_paragraph = readme
        .split("csv/tickets.csv")
        .nth(1)
        .expect("the README describes the tickets file");
    assert!(
        tickets_paragraph.contains("no review tree"),
        "the README says why the tickets file is empty: {tickets_paragraph}"
    );

    // The PDF's patterns: no pattern, and a sentence in the data the Patterns section prints.
    let json = serde_json::to_value(crate::report_export::build_report_json(
        &reviewed, None, &options,
    ))
    .unwrap();
    assert!(json["sub_group_patterns"].as_array().unwrap().is_empty());
    assert!(
        json["sub_group_patterns_note"]
            .as_str()
            .unwrap_or_default()
            .contains("no review tree"),
        "the PDF says so: {}",
        json["sub_group_patterns_note"]
    );

    // A report with a tree, or with no finding at all, carries no such note.
    let with_tree = apply_overlay(&mixed_sealed(), &[], &[], &[]);
    let json = serde_json::to_value(crate::report_export::build_report_json(
        &with_tree, None, &options,
    ))
    .unwrap();
    assert_eq!(json["sub_group_patterns_note"], "");
    let (reviewed, _) = (apply_overlay(&sealed_of(0), &[], &[], &[]), ());
    let json = serde_json::to_value(crate::report_export::build_report_json(
        &reviewed, None, &options,
    ))
    .unwrap();
    assert_eq!(json["sub_group_patterns_note"], "");
}
