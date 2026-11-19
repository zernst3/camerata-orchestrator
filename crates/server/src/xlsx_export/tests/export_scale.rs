//! M8 (Fable engine review): the workbook must export at 30,000 rows. A cell that joins a
//! variable-length list used to abort the whole export at Excel's 32,767-character limit.
//!
//! The rule that binds every test here: the client's developers receive every place. Capping a
//! cell may never hide a place or a problem. What no longer fits a cell must appear in full on
//! the All source rows sheet, with a visible pointer in the cell.

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
    let bytes = build_workbook(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    )
    .expect("a finding with 5,000 sites must not abort the export");

    let also = cell_by_header(&bytes, "All problems", "Also found at", 1);
    assert!(
        also.chars().count() < EXCEL_CELL_LIMIT,
        "the Also found at cell must sit under Excel's limit, was {} chars",
        also.chars().count()
    );
    assert!(
        also.contains("more places, see All source rows sheet"),
        "a capped cell must say where the rest went: tail {:?}",
        &also[also.len().saturating_sub(120)..]
    );

    // A title and a first line, the header row, then one row per place: the primary and the
    // 4,999 absorbed ones.
    assert_eq!(
        sheet_row_count(&bytes, "All source rows"),
        3 + 5_000,
        "the All source rows sheet must hold every place, in full"
    );
    let strings = sheet_cell_strings(&bytes, "All source rows");
    for i in [0usize, 1, 2_500, 4_998] {
        let path = format!("src/generated/module_{i:05}.rs");
        assert!(
            strings.contains(&path),
            "place {path} is missing from the All source rows sheet"
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
    let bytes = build_workbook(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    )
    .unwrap();
    let also = cell_by_header(&bytes, "All problems", "Also found at", 1);
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
    let bytes = build_workbook(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    )
    .expect("large deviation and convention groups must not abort the export");
    let where_cell = cell_by_header(&bytes, "Departures", "Where", 1);
    assert!(where_cell.chars().count() < EXCEL_CELL_LIMIT);
    assert!(where_cell.contains("more places, see All source rows sheet"));
    let also = cell_by_header(&bytes, "Your conventions", "Also found at", 1);
    assert!(also.chars().count() < EXCEL_CELL_LIMIT);
    assert!(also.contains("more places, see All source rows sheet"));
    // 4,000 + 4,001 places, every one on the All source rows sheet (and its three lines above).
    assert_eq!(
        sheet_row_count(&bytes, "All source rows"),
        3 + 4_000 + 4_001
    );
}

#[test]
fn a_small_report_keeps_its_cells_exactly_as_before() {
    let mut f = finding_with_sites(3);
    f.also_locations = vec![site(0), site(1)];
    let report = report_with(vec![f], vec!["SEC-NO-RAW-SQL-CONCAT-1"]);
    let bytes = build_workbook(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    )
    .unwrap();
    let also = cell_by_header(&bytes, "All problems", "Also found at", 1);
    assert_eq!(
        also,
        "src/generated/module_00000.rs:1\nsrc/generated/module_00001.rs:2"
    );
    assert!(!also.contains("see All source rows sheet"));
}

#[test]
fn any_other_oversized_text_cell_is_cut_with_a_pointer_instead_of_aborting() {
    let mut f = finding("SEC-NO-RAW-SQL-CONCAT-1", "src/a.rs", 1, "high");
    f.detail = "x".repeat(60_000);
    let report = report_with(vec![f], vec!["SEC-NO-RAW-SQL-CONCAT-1"]);
    let bytes = build_workbook(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    )
    .expect("an oversized prose cell must not abort the export");
    let detail = cell_by_header(&bytes, "All problems", "More detail", 1);
    assert!(detail.chars().count() < EXCEL_CELL_LIMIT);
    assert!(
        detail.contains("full text is in findings.json"),
        "the cut must say where the full text is"
    );
}

#[test]
fn more_places_than_one_sheet_holds_continue_on_a_second_sheet() {
    let mut f = finding_with_sites(7);
    f.also_locations.truncate(6);
    let report = report_with(vec![f], vec!["SEC-NO-RAW-SQL-CONCAT-1"]);
    let (rows, _) = partition_rows(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &HashMap::new(),
    );
    let parents: Vec<&FindingRow> = rows.iter().collect();
    let places = build_site_rows(&parents, &[]);
    assert_eq!(places.len(), 7);
    let lay = sheets::Layout::new(&rows, &[], &[], false, false);
    let names = vec![
        "All source rows".to_string(),
        "All source rows (2)".to_string(),
        "All source rows (3)".to_string(),
    ];
    let tables = sheets::source_rows_tables(&places, &lay, None, 3, &names);
    let mut wb = Workbook::new();
    let fmts = Formats::new();
    for table in &tables {
        tables::write_table(&mut wb, &fmts, table).unwrap();
    }
    let bytes = wb.save_to_buffer().unwrap();
    // 3 + 3 + 1 data rows, each sheet with its own title, first line and header row.
    assert_eq!(sheet_row_count(&bytes, "All source rows"), 3 + 3);
    assert_eq!(sheet_row_count(&bytes, "All source rows (2)"), 3 + 3);
    assert_eq!(sheet_row_count(&bytes, "All source rows (3)"), 3 + 1);
}

#[test]
fn an_area_named_like_a_sheet_is_only_a_cell() {
    let mut f = finding_with_sites(3);
    f.category = Some("All source rows".to_string());
    let report = report_with(vec![f], vec!["SEC-NO-RAW-SQL-CONCAT-1"]);
    let bytes = build_workbook(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    )
    .unwrap();
    let names = crate::release_gate::xlsx_sheet_names(&bytes).unwrap();
    assert_eq!(
        names.iter().filter(|n| *n == "All source rows").count(),
        1,
        "{names:?}"
    );
    let strings = sheet_cell_strings(&bytes, "All source rows");
    assert!(
        strings.iter().any(|s| s.starts_with("Raw record.")),
        "the sheet called All source rows must be the record of every row, got {strings:?}"
    );
}

#[test]
fn the_cap_helper_is_exact_at_the_boundary() {
    let items: Vec<String> = (0..10).map(|i| format!("site-{i}")).collect();
    // 10 items of 6 chars + 9 separators = 69.
    assert_eq!(site_list_cell_within(&items, 69), items.join("\n"));
    let capped = site_list_cell_within(&items, 68);
    assert!(capped.chars().count() <= 68);
    assert!(capped.ends_with("more places, see All source rows sheet"));
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

// ── M8 item 4: a synthetic report at scale ──────────────────────────────────────────────

/// `rows` findings spread across `rules` rules, each at its own file and line, with severities
/// cycling so every tier is populated. Rule ids are invented, so no corpus entry is needed.
fn scale_report(rows: usize, rules: usize) -> ScanReport {
    let severities = ["critical", "high", "medium", "low"];
    let findings: Vec<Finding> = (0..rows)
        .map(|i| {
            let rule = i % rules;
            let mut f = finding(
                &format!("SCALE-RULE-{rule:03}-1"),
                &format!("src/area_{rule:03}/file_{i:05}.rs"),
                1 + (i % 400),
                severities[i % severities.len()],
            );
            f.detail = format!("Finding {i} under rule {rule}: the call at this site builds its query by concatenation.");
            f
        })
        .collect();
    let rule_ids: Vec<String> = (0..rules).map(|r| format!("SCALE-RULE-{r:03}-1")).collect();
    report_with(findings, rule_ids.iter().map(String::as_str).collect())
}

/// Exports `report` to the workbook and to the data the PDF template renders, and, when
/// `render_pdf` is set and typst is installed, compiles the PDF. Returns what each step cost.
async fn export_everything(report: &ScanReport, render_pdf: bool) -> (usize, usize, Option<usize>) {
    let sealed = report.clone().seal();
    let started = std::time::Instant::now();
    let xlsx = build_workbook(
        &crate::report_export::ReviewedReport::unreviewed(&sealed, &HashMap::new()),
        None,
        &empty_opts(),
    )
    .expect("the workbook must export");
    eprintln!(
        "scale: workbook {} bytes in {:?}",
        xlsx.len(),
        started.elapsed()
    );
    assert_eq!(
        sheet_row_count(&xlsx, "All source rows"),
        3 + report.findings.len(),
        "every row's place must be on the All source rows sheet"
    );

    let started = std::time::Instant::now();
    let json = crate::report_export::build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&sealed, &HashMap::new()),
        None,
        &empty_opts(),
    );
    let data = crate::report_export::render_data_json(&json).expect("the PDF data must serialize");
    eprintln!(
        "scale: PDF data {} bytes in {:?}",
        data.len(),
        started.elapsed()
    );

    let mut pdf_bytes = None;
    if render_pdf && camerata_test_env::typst_on_path() {
        let started = std::time::Instant::now();
        let pdf = crate::report_export::compile_pdf_from_data(&data)
            .await
            .expect("the PDF must render");
        eprintln!("scale: PDF {} bytes in {:?}", pdf.len(), started.elapsed());
        assert!(pdf.starts_with(b"%PDF"));
        pdf_bytes = Some(pdf.len());
    }
    (xlsx.len(), data.len(), pdf_bytes)
}

/// The always-on size: big enough to catch a per-row cost that grows, small enough for the
/// normal suite.
#[tokio::test]
async fn a_report_of_2000_rows_across_100_rules_exports_to_xlsx_and_pdf() {
    let report = scale_report(2_000, 100);
    let (xlsx, data, _) = export_everything(&report, true).await;
    assert!(xlsx > 0 && data > 0);
}

/// The size the engine review set as the target: 30,000 rows across 100 rules. Run on demand:
/// `cargo test -p camerata-server --lib export_scale -- --ignored --nocapture`. The workbook
/// and the PDF data are exported in full; the PDF render is slow, so it is part of this run
/// and not of the normal suite. Measured on a development laptop (debug test build, release
/// Typst 0.15): workbook 7.4 MB in 19 to 27 s, PDF data 38 MB in 4 to 7 s, PDF 54 MB in 22 to
/// 33 s with Typst peaking near 4.8 GB resident.
#[tokio::test]
#[ignore = "30,000-row export including the PDF render; slow, run on demand"]
async fn a_report_of_30000_rows_across_100_rules_exports_to_xlsx_and_pdf() {
    let report = scale_report(30_000, 100);
    let (xlsx, data, pdf) = export_everything(&report, true).await;
    assert!(xlsx > 0 && data > 0);
    if camerata_test_env::typst_on_path() {
        assert!(pdf.is_some());
    }
}

// ── M8-1: the cap is enforced by type, and this scan keeps it that way ──────────────────

/// Every sheet-writing function takes a [`CappedSheet`], whose text writers cut an oversized
/// cell with a pointer. The raw `rust_xlsxwriter` writers that take text abort the whole export
/// at 32,767 characters, so they may be named only inside `mod capped_sheet`. A later change
/// that reaches for one (or takes a raw `Worksheet`) fails here instead of failing a client's
/// export.
#[test]
fn no_raw_string_writer_outside_capped_sheet() {
    const RAW_TEXT_WRITERS: [&str; 7] = [
        "write_string",
        "write_rich_string",
        "write_formula",
        "write_url_with_format",
        ".write(",
        ".write_with_format(",
        "add_note",
    ];
    let main = include_str!("../../xlsx_export.rs");
    let production = main
        .split("\n#[cfg(test)]\nmod tests {")
        .next()
        .expect("the production half of xlsx_export.rs");
    let impl_start = production
        .find("mod capped_sheet {")
        .expect("the capped_sheet module");
    let impl_end = impl_start
        + production[impl_start..]
            .find("\n}\n")
            .expect("the end of the capped_sheet module");
    let outside_wrapper = format!("{}{}", &production[..impl_start], &production[impl_end..]);
    let sources = [
        ("xlsx_export.rs", outside_wrapper.as_str()),
        (
            "xlsx_export/triage_log.rs",
            include_str!("../triage_log.rs"),
        ),
        ("xlsx_export/sheets.rs", include_str!("../sheets.rs")),
        ("xlsx_export/tables.rs", include_str!("../tables.rs")),
        (
            "xlsx_export/review_sheets.rs",
            include_str!("../review_sheets.rs"),
        ),
    ];
    for (file, text) in sources {
        for (n, line) in text.lines().enumerate() {
            let code = line.split("//").next().unwrap_or("");
            for writer in RAW_TEXT_WRITERS {
                assert!(
                    !code.contains(writer),
                    "{file}:{} calls a raw text writer ({writer}) outside CappedSheet: {line}\n\
                     Write text through CappedSheet::write_text so an oversized cell is cut \
                     with a pointer instead of aborting the export.",
                    n + 1
                );
            }
        }
    }
    assert!(
        !outside_wrapper.contains("&mut Worksheet")
            || outside_wrapper.matches("&mut Worksheet").count() == 0,
        "sheet code must take a CappedSheet, not a raw Worksheet"
    );
}
