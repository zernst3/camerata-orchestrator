//! The review screen's severity bounds line is the operator's. No client export (PDF data,
//! workbook, JSON, CSV) carries its fields, and a floor-raised row exports the severity it always
//! did. A pin: nothing here changed a client byte. Synthetic rows only.

use std::collections::HashMap;

use super::tests::{finding, report_with};
use super::wo9::support::client_options;
use super::*;
use crate::release_gate::{xlsx_sheet_grid, xlsx_sheet_names};

const FIELD_NAMES: &[&str] = &[
    "severity_bounds",
    "applied_from",
    "applied_bound",
    "deviation_ceiling",
];

fn floor_raised() -> (crate::onboard::SealedReport, camerata_rules::RuleSet) {
    let mut rule = camerata_rules::bare_rule("SYN-PIN-1", "security");
    rule.min_severity = Some("high".to_string());
    let mut set = camerata_rules::RuleSet::default();
    set.push(rule);
    let mut f = finding("SYN-PIN-1", "src/pin.rs", 7, "low");
    f.kind = Some(camerata_api_types::audit::FindingKind::Defect);
    let mut rows = vec![f];
    crate::severity_bounds::apply_scan_severity_bounds(&mut rows, Some(&set));
    assert_eq!(rows[0].severity, "high", "the floor raised the row");
    (report_with(rows, vec!["SYN-PIN-1"]).seal(), set)
}

fn assert_clean(what: &str, text: &str) {
    for name in FIELD_NAMES {
        assert!(!text.contains(name), "{what} carries {name}");
    }
}

#[test]
fn no_client_export_of_a_floor_raised_row_carries_the_bounds_fields() {
    let (sealed, set) = floor_raised();
    let reviewed = ReviewedReport::unreviewed(&sealed, &HashMap::new());
    for internal in [false, true] {
        let opts = ReportOptions {
            internal,
            ..client_options()
        };
        let json = build_report_json(&reviewed, Some(&set), &opts);
        let text = serde_json::to_string(&json).unwrap();
        assert_clean("the report JSON", &text);
        assert!(text.contains("\"high\""), "the row exports at its severity");

        let xlsx = crate::xlsx_export::build_workbook(&reviewed, Some(&set), &opts).unwrap();
        for sheet in xlsx_sheet_names(&xlsx).unwrap() {
            for row in xlsx_sheet_grid(&xlsx, &sheet).unwrap() {
                for cell in row {
                    assert_clean("a workbook cell", &cell.to_ascii_lowercase());
                }
            }
        }
        for (path, bytes) in
            crate::xlsx_export::build_json_exports(&reviewed, Some(&set), &opts).unwrap()
        {
            assert_clean(&path, &String::from_utf8_lossy(&bytes));
        }
        for (path, bytes) in
            crate::xlsx_export::build_csv_exports(&reviewed, Some(&set), &opts).unwrap()
        {
            assert_clean(&path, &String::from_utf8_lossy(&bytes));
        }
    }
}
