//! WO9, design 5.5 item 9: every export carries `reviewer_edited` and the edit ids, now on the
//! Sites sheet and the dependency rows too.

use camerata_api_types::edits::EditField;
use serde_json::Value;

use super::support::*;
use crate::report_export::apply_overlay;

#[test]
fn every_export_carries_reviewer_edited_and_the_edit_ids_on_the_sites_sheet_and_dependency_rows() {
    let sealed = mixed_sealed();
    let sql = row_at(&sealed, "src/a.rs");
    let dep = row_at(&sealed, "package.json");
    let edits = vec![
        edit_of(1, &sql, EditField::Headline, "Query built from input"),
        edit_of(2, &dep, EditField::Severity, "low"),
    ];
    let reviewed = apply_overlay(&sealed, &[], &edits, &[]);
    let entries = released_entries(&reviewed, &client_options());

    // The dependency dataset (JSON and CSV).
    let deps: Value = serde_json::from_slice(&entries["json/dependencies.json"]).unwrap();
    let dep_row = &deps[0];
    assert_eq!(dep_row["reviewer_edited"], true, "{dep_row}");
    assert_eq!(dep_row["edit_ids"], serde_json::json!(["edit-000002"]));
    let csv = String::from_utf8_lossy(&entries["csv/dependencies.csv"]).into_owned();
    assert!(
        csv.lines().next().unwrap().contains("reviewer_edited"),
        "{csv}"
    );

    // The workbook: the All source rows sheet and the Dependencies sheet.
    let xlsx = entries
        .iter()
        .find(|(n, _)| n.ends_with(".xlsx"))
        .map(|(_, b)| b.clone())
        .unwrap();
    for (sheet, file) in [
        ("All source rows", "src/a.rs"),
        ("All source rows", "package.json"),
        ("Dependencies", "package.json"),
    ] {
        let grid = sheet_table(&xlsx, sheet);
        let col = |name: &str| {
            grid[0]
                .iter()
                .position(|h| h == name)
                .unwrap_or_else(|| panic!("{sheet} has no {name:?} column: {:?}", grid[0]))
        };
        let (edited, ids) = (col("Reviewer edited"), col("Edit ids"));
        let file_col = col("File");
        let row = grid
            .iter()
            .skip(1)
            .find(|r| r[file_col] == file)
            .unwrap_or_else(|| panic!("no {file} row on {sheet}"));
        assert_eq!(row[edited], "Yes", "{sheet}: {file}");
        assert!(
            row[ids].starts_with("edit-00000"),
            "{sheet}: {file}: {:?}",
            row[ids]
        );
    }
    // A row nobody edited says so.
    let grid = sheet_table(&xlsx, "All source rows");
    let edited = grid[0].iter().position(|h| h == "Reviewer edited").unwrap();
    let file = grid[0].iter().position(|h| h == "File").unwrap();
    let plain = grid.iter().skip(1).find(|r| r[file] == "src/b.rs").unwrap();
    assert_eq!(plain[edited], "");
}
