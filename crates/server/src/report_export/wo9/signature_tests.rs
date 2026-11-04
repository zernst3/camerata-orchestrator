//! WO9: every export is built from the one reviewed value (design 3B). A source scan pins the
//! signatures, so a new caller cannot hand an exporter a sealed report and a map of its own.

/// The text of the parameter list of `fn <name>(` in `source`.
fn params_of(source: &str, name: &str) -> String {
    let marker = format!("fn {name}(");
    let at = source
        .find(&marker)
        .unwrap_or_else(|| panic!("no `{marker}` in the source"));
    let rest = &source[at + marker.len()..];
    let mut depth = 1usize;
    for (i, c) in rest.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return rest[..i].to_string();
                }
            }
            _ => {}
        }
    }
    panic!("`{marker}` has no closing parenthesis");
}

#[test]
fn every_export_takes_the_reviewed_report_and_no_sealed_report_or_map() {
    let report_export = include_str!("../../report_export.rs");
    let xlsx = include_str!("../../xlsx_export.rs");
    let triage = include_str!("../../xlsx_export/triage_log.rs");
    let gate = include_str!("../../release_gate.rs");
    let exports: [(&str, &str); 9] = [
        (report_export, "build_report_json"),
        (xlsx, "build_workbook"),
        (xlsx, "build_findings_export"),
        (xlsx, "build_json_exports"),
        (xlsx, "build_csv_exports"),
        (xlsx, "partition_rows"),
        (triage, "build_triage_log"),
        (gate, "build_product_package"),
        (gate, "assemble_package"),
    ];
    for (source, name) in exports {
        let params = params_of(source, name);
        assert!(
            params.contains("&ReviewedReport"),
            "{name} must take the reviewed report: ({params})"
        );
        for banned in ["SealedReport", "ScanReport", "DispositionWire"] {
            assert!(
                !params.contains(banned),
                "{name} takes a {banned} beside the reviewed report, so a caller could skip the \
                 overlay: ({params})"
            );
        }
    }
}
