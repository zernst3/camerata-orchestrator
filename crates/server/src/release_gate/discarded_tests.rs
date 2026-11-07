//! A10 follow-ups: a thrown-out row never leaves a stale tree behind, carries a site key, and is
//! visible in the INTERNAL workbook (a "Thrown out by the engine" sheet) and nowhere in a client
//! package. A real package is built through `assemble_package` around a stand-in PDF.

use std::collections::HashMap;

use super::tests::{client_options, scan_report};
use super::*;
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

/// A placeholder row, as an older engine stored it: a real finding in the rows, not yet apart.
fn placeholder_row() -> Finding {
    Finding {
        repo: "acme/portal".to_string(),
        path: "lib/placeholder_zone.ts".to_string(),
        line: 8,
        rule_id: "AI-TYPESCRIPT-PLACEHOLDER".to_string(),
        severity: "low".to_string(),
        snippet: "x".to_string(),
        detail: "placeholder \u{2014} x".to_string(),
        ..Finding::default()
    }
}

fn workbook_of(zip: &[u8]) -> Vec<u8> {
    read_zip_entries(zip)
        .expect("zip")
        .into_iter()
        .find(|(name, _)| name.ends_with(".xlsx"))
        .map(|(_, bytes)| bytes)
        .expect("the package holds a workbook")
}

/// A stored report with a placeholder row has a tree that holds the row. Sealing throws the row
/// out; the tree it ships with names only rows that exist, the thrown-out one in its category's
/// thrown-out node (A12).
#[test]
fn a_stored_report_with_a_placeholder_row_seals_to_a_tree_whose_members_all_exist() {
    let mut stored = scan_report();
    stored.findings.push(placeholder_row());
    stored.assign_site_keys();
    let placeholder_key = stored
        .findings
        .iter()
        .find(|f| f.rule_id == "AI-TYPESCRIPT-PLACEHOLDER")
        .map(|f| f.site_key.clone())
        .expect("the row is keyed");
    assert!(
        stored
            .tree
            .as_ref()
            .is_some_and(|t| t.nodes.iter().any(|n| n.members.contains(&placeholder_key))),
        "the stored tree holds the row"
    );

    let sealed = stored.seal();
    assert_eq!(sealed.discarded.len(), 1);
    let keys: std::collections::HashSet<&str> = sealed
        .findings
        .iter()
        .chain(sealed.discarded.iter())
        .map(|f| f.site_key.as_str())
        .collect();
    let tree = sealed.tree.as_ref().expect("a sealed report has a tree");
    for node in &tree.nodes {
        for member in &node.members {
            assert!(keys.contains(member.as_str()), "dangling member {member}");
        }
    }
    assert_eq!(
        tree.nodes.iter().map(|n| n.members.len()).sum::<usize>(),
        sealed.findings.len() + sealed.discarded.len(),
        "every row, the thrown-out one included, is in exactly one node"
    );
}

/// Every discarded row carries a site key after the seal, so a later decision or an Engine Check
/// can address it.
#[test]
fn every_discarded_row_carries_a_site_key_after_the_seal() {
    let mut report = scan_report();
    report.findings.push(placeholder_row());
    let mut twin = placeholder_row();
    twin.line = 40;
    twin.path = "lib/other_zone.ts".to_string();
    report.findings.push(twin);
    let sealed = report.seal();
    assert_eq!(sealed.discarded.len(), 2);
    assert!(
        sealed.discarded.iter().all(|f| !f.site_key.is_empty()),
        "{:?}",
        sealed.discarded
    );
    let distinct: std::collections::HashSet<&str> = sealed
        .discarded
        .iter()
        .map(|f| f.site_key.as_str())
        .collect();
    assert_eq!(distinct.len(), 2);
}

/// The internal workbook lists every discarded row on the Ruled out sheet (ruled out by the Engine)
/// with its reason, rule, place and problem; a client package has no such sheet and none of the rows.
#[test]
fn the_internal_workbook_lists_thrown_out_rows_and_a_client_package_never_does() {
    let rows = || {
        let mut report = scan_report();
        report.findings.push(placeholder_row());
        report
    };
    let internal_options = ReportOptions {
        internal: true,
        ..client_options()
    };
    let internal = release_internal(package_with(rows(), &internal_options)).expect("internal");
    let wb = workbook_of(&internal.bytes);
    let grid = xlsx_sheet_grid(&wb, "Ruled out").expect("the sheet exists");
    let col = |name: &str| grid[0].iter().position(|h| h == name).expect(name);
    let engine: Vec<&Vec<String>> = grid
        .iter()
        .skip(1)
        .filter(|r| r[col("Ruled out by")] == "Engine")
        .collect();
    assert_eq!(engine.len(), 1, "{grid:?}");
    assert!(
        engine[0][col("Reason")].contains("placeholder"),
        "{:?}",
        engine[0]
    );
    assert_eq!(engine[0][col("Rule id")], "AI-TYPESCRIPT-PLACEHOLDER");
    assert_eq!(engine[0][col("File")], "lib/placeholder_zone.ts");
    assert_eq!(engine[0][col("Line")], "8");

    let client = release(package_with(rows(), &client_options())).expect("client");
    assert!(!xlsx_sheet_names(&workbook_of(&client.bytes))
        .unwrap()
        .iter()
        .any(|n| n.contains("Thrown out")));
    for (name, bytes) in read_zip_entries(&client.bytes).expect("zip") {
        let text = if name.ends_with(".xlsx") {
            xlsx_strings(&bytes).expect("strings").join("\n")
        } else {
            String::from_utf8_lossy(&bytes).into_owned()
        };
        assert!(!text.contains("placeholder_zone"), "row place in {name}");
        assert!(
            !text.contains("Thrown out by the engine"),
            "sheet in {name}"
        );
    }
}
