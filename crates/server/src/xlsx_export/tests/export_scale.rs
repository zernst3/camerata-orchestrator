//! M8 (Fable engine review): the workbook must export at 30,000 rows. A cell that joins a
//! variable-length list used to abort the whole export at Excel's 32,767-character limit.
//!
//! The rule that binds every test here: the client's developers receive every site. Capping a
//! cell may never hide a site or a violation. What no longer fits a cell must appear in full on
//! the Sites sheet, with a visible pointer in the cell.

use super::*;
use camerata_api_types::audit::{ConventionObservedKind, DeviationKind, FindingKind};

const EXCEL_CELL_LIMIT: usize = 32_767;

fn site(i: usize) -> crate::onboard::MergedLocation {
    crate::onboard::MergedLocation::site(
        "owner/repo".to_string(),
        format!("src/generated/module_{i:05}.rs"),
        i + 1,
        "SEC-NO-RAW-SQL-CONCAT-1".to_string(),
        format!("let q = format!(\"SELECT {i}\");"),
        false,
    )
}

/// One finding that stands for `n` sites: its own plus `n - 1` absorbed ones.
fn finding_with_sites(n: usize) -> Finding {
    let mut f = finding("SEC-NO-RAW-SQL-CONCAT-1", "src/primary.rs", 7, "high");
    f.also_locations = (0..n - 1).map(site).collect();
    f
}

fn sheet_row_count(bytes: &[u8], sheet: &str) -> usize {
    read_zip_entry(bytes, &sheet_entry_for(bytes, sheet))
        .matches("<row ")
        .count()
}

#[test]
fn a_finding_with_5000_sites_exports_and_every_site_is_on_the_sites_sheet() {
    let report = report_with(
        vec![finding_with_sites(5_000)],
        vec!["SEC-NO-RAW-SQL-CONCAT-1"],
    );
    let bytes = build_workbook(&report.clone().seal(), &HashMap::new(), None, &empty_opts())
        .expect("a finding with 5,000 sites must not abort the export");

    let also = cell_text(&bytes, &sheet_entry_for(&bytes, "All Findings"), "Z2");
    assert!(
        also.chars().count() < EXCEL_CELL_LIMIT,
        "the Also found at cell must sit under Excel's limit, was {} chars",
        also.chars().count()
    );
    assert!(
        also.contains("more sites, see Sites sheet"),
        "a capped cell must say where the rest went: tail {:?}",
        &also[also.len().saturating_sub(120)..]
    );

    // Header row plus one row per site: the primary and the 4,999 absorbed ones.
    assert_eq!(
        sheet_row_count(&bytes, "Sites"),
        1 + 5_000,
        "the Sites sheet must hold every site, in full"
    );
    let strings = sheet_cell_strings(&bytes, "Sites");
    for i in [0usize, 1, 2_500, 4_998] {
        let path = format!("src/generated/module_{i:05}.rs");
        assert!(
            strings.contains(&path),
            "site {path} is missing from the Sites sheet"
        );
    }
    assert!(strings.iter().any(|s| s == "src/primary.rs"));
}

#[test]
fn the_pointer_counts_exactly_the_sites_the_cell_dropped() {
    let report = report_with(
        vec![finding_with_sites(5_000)],
        vec!["SEC-NO-RAW-SQL-CONCAT-1"],
    );
    let bytes =
        build_workbook(&report.clone().seal(), &HashMap::new(), None, &empty_opts()).unwrap();
    let also = cell_text(&bytes, &sheet_entry_for(&bytes, "All Findings"), "Z2");
    let (shown, pointer) = also.rsplit_once("\nand ").expect("pointer on its own line");
    let dropped: usize = pointer
        .split_whitespace()
        .next()
        .and_then(|n| n.parse().ok())
        .expect("pointer starts with the dropped count");
    // The cell lists the absorbed sites (the primary has its own columns), one per line.
    let listed = shown.lines().count();
    assert_eq!(
        listed + dropped,
        4_999,
        "listed {listed} + dropped {dropped}"
    );
}

#[test]
fn a_deviation_group_and_a_convention_with_thousands_of_sites_do_not_abort() {
    let mut dev = finding("ARCH-API-VERSIONING-1", "src/api/users.rs", 4, "medium");
    dev.kind = Some(FindingKind::Deviation(DeviationKind {
        deviating_sites: 4_000,
        assessed_sites: Some(5_000),
        convention_option_id: None,
        convention_label: "URI-versioned endpoints".to_string(),
    }));
    dev.also_locations = (0..3_999).map(site).collect();
    let mut conv = finding("ARCH-PAGINATION-1", "src/api/list.rs", 7, "info");
    conv.kind = Some(FindingKind::ConventionObserved(ConventionObservedKind {
        convention_option_id: "cursor".to_string(),
        convention_label: "Cursor pagination".to_string(),
    }));
    conv.also_locations = (0..4_000).map(site).collect();

    let report = report_with(vec![dev, conv], vec![]);
    let bytes = build_workbook(&report.clone().seal(), &HashMap::new(), None, &empty_opts())
        .expect("large deviation and convention groups must not abort the export");
    let where_cell = cell_text(&bytes, &sheet_entry_for(&bytes, "Deviations"), "G2");
    assert!(where_cell.chars().count() < EXCEL_CELL_LIMIT);
    assert!(where_cell.contains("more sites, see Sites sheet"));
    let also = cell_text(&bytes, &sheet_entry_for(&bytes, "Conventions"), "I2");
    assert!(also.chars().count() < EXCEL_CELL_LIMIT);
    assert!(also.contains("more sites, see Sites sheet"));
    // 4,000 + 4,001 sites, every one on the Sites sheet.
    assert_eq!(sheet_row_count(&bytes, "Sites"), 1 + 4_000 + 4_001);
}

#[test]
fn a_small_report_keeps_its_cells_exactly_as_before() {
    let mut f = finding_with_sites(3);
    f.also_locations = vec![site(0), site(1)];
    let report = report_with(vec![f], vec!["SEC-NO-RAW-SQL-CONCAT-1"]);
    let bytes =
        build_workbook(&report.clone().seal(), &HashMap::new(), None, &empty_opts()).unwrap();
    let also = cell_text(&bytes, &sheet_entry_for(&bytes, "All Findings"), "Z2");
    assert_eq!(
        also,
        "src/generated/module_00000.rs:1\nsrc/generated/module_00001.rs:2"
    );
    assert!(!also.contains("see Sites sheet"));
}

#[test]
fn any_other_oversized_text_cell_is_cut_with_a_pointer_instead_of_aborting() {
    let mut f = finding("SEC-NO-RAW-SQL-CONCAT-1", "src/a.rs", 1, "high");
    f.detail = "x".repeat(60_000);
    let report = report_with(vec![f], vec!["SEC-NO-RAW-SQL-CONCAT-1"]);
    let bytes = build_workbook(&report.clone().seal(), &HashMap::new(), None, &empty_opts())
        .expect("an oversized prose cell must not abort the export");
    let detail = cell_text(&bytes, &sheet_entry_for(&bytes, "All Findings"), "U2");
    assert!(detail.chars().count() < EXCEL_CELL_LIMIT);
    assert!(
        detail.contains("full text is in findings.json"),
        "the cut must say where the full text is"
    );
}

#[test]
fn more_sites_than_one_sheet_holds_continue_on_a_second_sheet() {
    let mut f = finding_with_sites(7);
    f.also_locations.truncate(6);
    let report = report_with(vec![f], vec!["SEC-NO-RAW-SQL-CONCAT-1"]);
    let (rows, _) = partition_rows(
        &report.clone().seal(),
        &HashMap::new(),
        None,
        &HashMap::new(),
    );
    let sites = build_site_rows(&rows);
    assert_eq!(sites.len(), 7);
    let mut wb = Workbook::new();
    let names = vec![
        "Sites".to_string(),
        "Sites (2)".to_string(),
        "Sites (3)".to_string(),
    ];
    write_sites_sheets(&mut wb, &names, &sites, 3, &Formats::new()).unwrap();
    let bytes = wb.save_to_buffer().unwrap();
    // 3 + 3 + 1 data rows, each sheet with its own header row.
    assert_eq!(sheet_row_count(&bytes, "Sites"), 1 + 3);
    assert_eq!(sheet_row_count(&bytes, "Sites (2)"), 1 + 3);
    assert_eq!(sheet_row_count(&bytes, "Sites (3)"), 1 + 1);
}

#[test]
fn an_area_called_sites_cannot_take_the_name_the_pointer_uses() {
    let mut f = finding_with_sites(3);
    f.category = Some("Sites".to_string());
    let report = report_with(vec![f], vec!["SEC-NO-RAW-SQL-CONCAT-1"]);
    let bytes =
        build_workbook(&report.clone().seal(), &HashMap::new(), None, &empty_opts()).unwrap();
    let strings = sheet_cell_strings(&bytes, "Sites");
    assert!(
        strings.iter().any(|s| s.starts_with("Finding (rule @")),
        "the sheet called Sites must be the site list, got {strings:?}"
    );
}

#[test]
fn the_cap_helper_is_exact_at_the_boundary() {
    let items: Vec<String> = (0..10).map(|i| format!("site-{i}")).collect();
    // 10 items of 6 chars + 9 separators = 69.
    assert_eq!(site_list_cell_within(&items, 69), items.join("\n"));
    let capped = site_list_cell_within(&items, 68);
    assert!(capped.chars().count() <= 68);
    assert!(capped.ends_with("more sites, see Sites sheet"));
    let shown = capped.lines().count() - 1;
    let left_out: usize = capped
        .lines()
        .last()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(shown + left_out, 10);
}
