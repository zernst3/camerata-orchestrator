//! WO9, amendment A1: a collapse never loses a location. Every collapsed source still appears as
//! its own row on the Sites sheet, with the surviving row's review level and sub-group.

use super::support::*;
use crate::release_gate::xlsx_sheet_grid;
use crate::report_export::apply_overlay;
use crate::review_tree::FlaggedBy;

#[test]
fn a_collapsed_source_has_its_own_row_on_the_sites_sheet() {
    let mut report = mixed_sealed().into_inner();
    report.findings[0].flagged_by = vec![
        FlaggedBy {
            rule_id: "camerata.security.sql-string-concat".to_string(),
            tool: Some("semgrep".to_string()),
            site_key: "acme/portal\0src/a.rs\0other\00".to_string(),
            path: "src/a.rs".to_string(),
            line: 12,
        },
        FlaggedBy {
            rule_id: "ESLINT-SQL".to_string(),
            tool: Some("eslint".to_string()),
            site_key: "acme/portal\0src/a.rs\0third\00".to_string(),
            path: "src/a.rs".to_string(),
            line: 10,
        },
    ];
    let sealed = report.seal();
    let reviewed = apply_overlay(&sealed, &[], &[], &[]);
    let entries = released_entries(&reviewed, &client_options());
    let xlsx = entries
        .iter()
        .find(|(n, _)| n.ends_with(".xlsx"))
        .map(|(_, b)| b.clone())
        .unwrap();
    let grid = xlsx_sheet_grid(&xlsx, "Sites").expect("a Sites sheet");
    let header = &grid[0];
    let col = |name: &str| header.iter().position(|h| h == name).expect(name);
    let (file, line, site, group, rule) = (
        col("File"),
        col("Line"),
        col("Site"),
        col("Group"),
        col("Rule ID"),
    );
    let at_a: Vec<&Vec<String>> = grid
        .iter()
        .skip(1)
        .filter(|r| r[file] == "src/a.rs")
        .collect();
    let primary = at_a
        .iter()
        .find(|r| r[site] == "Primary")
        .expect("the primary");
    let sources: Vec<&&Vec<String>> = at_a
        .iter()
        .filter(|r| r[site] == "Collapsed source")
        .collect();
    assert_eq!(sources.len(), 2, "one row per collapsed source: {at_a:?}");
    for (want_rule, want_line) in [
        ("camerata.security.sql-string-concat", "12"),
        ("ESLINT-SQL", "10"),
    ] {
        let row = sources
            .iter()
            .find(|r| r[rule] == want_rule)
            .unwrap_or_else(|| panic!("no row for {want_rule}"));
        assert_eq!(row[line], want_line, "the source keeps its own line");
        assert_eq!(
            row[group], primary[group],
            "it sits in the survivor's sub-group"
        );
    }
    // The primary row keeps its own count: sources are extra rows of the Sites sheet only.
    let total_rows = grid.len() - 1;
    assert_eq!(total_rows, 6 + 2, "six sites plus two collapsed sources");
}
