//! Defect D5: a merged finding's absorbed sites (`also_locations`) reach every client file, and a
//! credential in one of them must be redacted in EVERY file, while a harmless site must never
//! block the export.
//!
//! The report here is built by hand with the credential left in the site's snippet and headline
//! (the shape an older cached row, or a producer that forgot to redact, would carry), so the
//! export itself is what is being tested. A real package is built through `assemble_package`
//! around a stand-in PDF, like the gate's other tests, so these run without Typst.

use std::collections::{BTreeMap, HashMap};

use camerata_secrets::fixtures::{planted, Planted};
use camerata_secrets::SecretShape;

use super::archive::{xlsx_cells, xlsx_strings};
use super::tests::{client_options, scan_report};
use super::*;
use crate::onboard::{Finding, MergedLocation, ScanReport};
use crate::report_export::{build_report_json, render_data_json};

const STEM: &str = "acme-inspection";

/// A finding that absorbed one more site, whose snippet and headline carry `snippet`.
fn merged_report(site_rule: &str, snippet: &str, headline: &str) -> ScanReport {
    let mut report = scan_report();
    let mut primary: Finding = report.findings.remove(0);
    primary.also_matches = vec![site_rule.to_string()];
    primary.also_locations =
        vec![
            MergedLocation::site("acme/portal", "src/other.rs", 40, site_rule, snippet, false)
                .with_rating("high", headline),
        ];
    report.findings.push(primary);
    report
}

fn package_for(report: ScanReport) -> ProductPackage {
    let sealed = report.seal();
    let options = client_options();
    let json = build_report_json(&sealed, &HashMap::new(), None, &options);
    let data = render_data_json(&json).expect("data.json renders");
    assemble_package(
        &sealed,
        &HashMap::new(),
        None,
        &options,
        STEM,
        (json, data, b"%PDF-1.7 stand-in for the gate tests".to_vec()),
    )
    .expect("the package assembles")
}

/// Every client-visible text of a package: the PDF's data, findings.json, every dataset, and the
/// workbook's cells and shared strings.
fn every_surface(package: &ProductPackage) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    out.insert("PDF data".to_string(), package.data_json.clone());
    out.insert(
        "findings.json".to_string(),
        String::from_utf8_lossy(&package.findings_json).into_owned(),
    );
    for (name, bytes) in &package.datasets {
        out.insert(name.clone(), String::from_utf8_lossy(bytes).into_owned());
    }
    let cells = xlsx_cells(&package.xlsx).expect("workbook cells");
    out.insert(
        "workbook cells".to_string(),
        cells
            .iter()
            .map(|c| c.text.clone())
            .collect::<Vec<_>>()
            .join("\n"),
    );
    out.insert(
        "workbook strings".to_string(),
        xlsx_strings(&package.xlsx)
            .expect("shared strings")
            .join("\n"),
    );
    out
}

/// The `also_locations` entries of every findings.json row.
fn merged_sites(package: &ProductPackage) -> Vec<serde_json::Value> {
    let doc: serde_json::Value = serde_json::from_slice(&package.findings_json).expect("parses");
    doc["findings"]
        .as_array()
        .expect("findings")
        .iter()
        .flat_map(|row| {
            row["also_locations"]
                .as_array()
                .cloned()
                .unwrap_or_default()
        })
        .collect()
}

fn leaked(haystack: &str, p: &Planted) -> bool {
    p.value.lines().any(|line| haystack.contains(line))
}

/// A credential in a merged site's snippet and headline is redacted in findings.json, every CSV
/// and JSON dataset, the workbook and the PDF's data, and the export clears the gate.
#[test]
fn a_credential_in_a_merged_site_is_redacted_in_every_client_file() {
    for shape in [
        SecretShape::SupabaseSecretKey,
        SecretShape::StripeLiveSecretKey,
        SecretShape::GitHubToken,
    ] {
        let p = planted(shape);
        let package = package_for(merged_report(
            "SEC-HARDCODED-SECRET-1",
            &p.text,
            &format!("Key {} committed", p.value),
        ));
        // Anti-vacuity: the merged site is on the page the export is built from.
        assert!(
            !merged_sites(&package).is_empty(),
            "{shape:?}: the fixture must export a merged row"
        );
        for (surface, text) in every_surface(&package) {
            assert!(!leaked(&text, &p), "{shape:?} leaked in {surface}");
        }
        let released = release(package).expect("the redacted export clears the gate");
        assert!(released.report.is_clean());
        let entries = read_zip_entries(&released.bytes).expect("zip");
        for (name, bytes) in &entries {
            assert!(
                !leaked(&String::from_utf8_lossy(bytes), &p),
                "{shape:?} leaked in {name}"
            );
        }
    }
}

/// The merged site's secret must be redacted by the SAME redaction as the primary row: the
/// redacted form is what the primary row would show for the same text.
#[test]
fn a_merged_site_shows_the_same_redacted_form_as_a_primary_row() {
    let p = planted(SecretShape::SupabaseSecretKey);
    let package = package_for(merged_report("SEC-HARDCODED-SECRET-1", &p.text, "headline"));
    let expected = camerata_secrets::redact(&p.text).text;
    let sites = merged_sites(&package);
    assert_eq!(sites[0]["snippet"], expected.as_str());
}

/// A harmless site (an environment variable NAME, no value) must not block the export just
/// because the CSV cell holding the site list is JSON text whose neighbouring field (a rule id
/// naming a key) turns the name into a "named secret literal" when the cell is scanned as one
/// string. The gate's own refusal of a clean file is the bug.
#[test]
fn an_environment_variable_name_in_a_merged_site_does_not_block_the_export() {
    let name = "NEXT_PUBLIC_SUPABASE_SERVICE_ROLE_KEY=";
    let package = package_for(merged_report(
        "SUPABASE-SERVICE-ROLE-KEY-PUBLIC-1",
        name,
        "Service role key in a public variable",
    ));
    let released = release(package).unwrap_or_else(|e| panic!("a clean export was refused: {e}"));
    assert!(released.report.is_clean());
}

/// The relaxed CSV scan must stay strict: a real credential inside the JSON list in a CSV cell is
/// still found, whatever the surrounding fields are.
#[test]
fn a_credential_inside_a_json_cell_of_a_csv_is_still_blocked() {
    let p = planted(SecretShape::StripeLiveSecretKey);
    let cell = serde_json::json!([{ "rule_id": "X-1", "snippet": p.text }]).to_string();
    let csv = format!("id,also_locations\n1,{}\n", csv_quote(&cell));
    let problems = secrets::csv_problems("csv/all-findings.csv", csv.as_bytes());
    assert!(!problems.is_empty(), "the key in the JSON cell was missed");
    let shown = format!("{problems:?}");
    assert!(!leaked(&shown, &p), "{shown}");
}

fn csv_quote(cell: &str) -> String {
    format!("\"{}\"", cell.replace('"', "\"\""))
}
