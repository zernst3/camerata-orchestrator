//! WO9 and WO9b: the Remediation plan sheet and the ticket CSV, one row per sub-group, with false
//! positives and ignored sites left out (design 3B items 2 and 4, D16).

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
fn the_remediation_plan_has_one_row_per_subgroup_and_excludes_false_positives_and_ignored() {
    let (reviewed, sealed) = reviewed_for_plan();
    let entries = released_entries(&reviewed, &client_options());
    let xlsx = workbook(&entries);

    // It is the first sheet of the workbook.
    let names = xlsx_sheet_names(&xlsx).expect("sheet names");
    assert_eq!(names[0], "Remediation plan", "{names:?}");

    let grid = xlsx_sheet_grid(&xlsx, "Remediation plan").expect("the plan sheet");
    for header in [
        "Group id",
        "Rule",
        "Cause",
        "Severity",
        "Sites",
        "Fix",
        "Worked example",
        "Effort",
        "Top folders",
        "Sequence",
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
            .find(|r| cell(&grid, r, "Group id") == group)
            .map(|r| r.as_slice())
            .unwrap_or_else(|| panic!("no plan row for {group}"))
    };
    let sql = row_of(&sql_group);
    assert_eq!(
        cell(&grid, sql, "Sites"),
        "2",
        "the ignored site is not a site to fix"
    );
    assert_eq!(cell(&grid, sql, "Severity"), "High");
    assert_eq!(
        cell(&grid, sql, "Worked example"),
        "Bind the parameters with the driver."
    );
    assert_eq!(cell(&grid, sql, "Sequence"), "Fix first");
    assert!(
        cell(&grid, sql, "Top folders").contains("src (2)"),
        "{sql:?}"
    );
    let layering = row_of(&layering_group);
    assert_eq!(
        cell(&grid, layering, "Sites"),
        "1",
        "the false positive is excluded"
    );
    assert_eq!(cell(&grid, layering, "Sequence"), "Next");
    // Fix first leads.
    assert_eq!(cell(&grid, rows[0], "Sequence"), "Fix first");
    assert_eq!(cell(&grid, rows[rows.len() - 1], "Sequence"), "Next");
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
fn a_ticket_lists_fifty_sites_and_points_at_the_sites_sheet_for_the_rest() {
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
            "and 10 more sites, see Sites sheet, filter Group = {group}"
        )),
        "{sites}"
    );
}
