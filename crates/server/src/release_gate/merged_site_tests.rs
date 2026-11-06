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
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&sealed, &HashMap::new()),
        None,
        &options,
    );
    let data = render_data_json(&json).expect("data.json renders");
    assemble_package(
        &crate::report_export::ReviewedReport::unreviewed(&sealed, &HashMap::new()),
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

/// The parsed walk must not make the CSV check weaker than scanning the cell as one string did:
/// a credential value whose NAME sits in a neighbouring field (the rule id names a key, the
/// snippet is the bare value), a value hidden behind escaped quotes inside a snippet, and an
/// unquoted `.env` line that the whole-cell scan could not see are all still blocked.
#[test]
fn a_credential_split_or_escaped_inside_a_json_cell_of_a_csv_is_still_blocked() {
    let value = "Qm3vX9pL2sKd8Rt4Wn7Yz1Hb5Jc0Tf6";
    let cells = [
        // Split across keys: the name in `rule_id`, the bare value in `snippet`.
        serde_json::json!([{ "rule_id": "SEC-API-KEY-1", "snippet": value }]),
        // Escaped quotes inside the snippet.
        serde_json::json!([{ "rule_id": "X-1", "snippet": format!("const apiKey = \"{value}\";") }]),
        // An unquoted `.env` line: only visible when the string is scanned on its own.
        serde_json::json!([{ "rule_id": "X-1", "snippet": format!("SERVICE_KEY={value}") }]),
        // Nested one level deeper.
        serde_json::json!([{ "rule_id": "X-1", "site": { "snippet": format!("token = '{value}'") } }]),
    ];
    for cell in cells {
        let csv = format!("id,also_locations\n1,{}\n", csv_quote(&cell.to_string()));
        let problems = secrets::csv_problems("csv/all-findings.csv", csv.as_bytes());
        assert!(!problems.is_empty(), "missed in {cell}");
        let shown = format!("{problems:?}");
        assert!(!shown.contains(value), "{shown}");
    }
}

/// The exact cell of the efficiency run: a rule id naming a key beside an environment variable
/// NAME (no value) is clean, and stays clean with a digit in the name.
#[test]
fn an_environment_variable_name_beside_a_key_rule_id_in_a_csv_cell_is_clean() {
    for name in [
        "NEXT_PUBLIC_SUPABASE_SERVICE_ROLE_KEY=",
        "export NEXT_PUBLIC_S3_SECRET_ACCESS_KEY=",
    ] {
        let cell = serde_json::json!([{
            "rule_id": "SUPABASE-SERVICE-ROLE-KEY-PUBLIC-1", "snippet": name, "line": 3
        }]);
        let csv = format!("id,also_locations\n1,{}\n", csv_quote(&cell.to_string()));
        let problems = secrets::csv_problems("csv/all-findings.csv", csv.as_bytes());
        assert!(problems.is_empty(), "{name}: {problems:?}");
    }
}

fn csv_quote(cell: &str) -> String {
    format!("\"{}\"", cell.replace('"', "\"\""))
}

/// Defect D6, the exact cells of the Saturday accuracy runs: a merged site whose snippet is a
/// CORS header NAME (`Access-Control-Allow-Credentials`, `Access-Control-Allow-Origin`) beside
/// a rule id that names credentials. Scanned as one string, the rule id reads as the name of a
/// secret and the header name (32 and 27 characters, hyphen-joined words) as its value; the gate
/// refused the faktura and lumen exports on `also_locations`. A header name is not a credential.
#[test]
fn an_http_header_name_beside_a_credentials_rule_id_in_a_csv_cell_is_clean() {
    let cells = [
        // faktura: the redacted evidence was "...ials (32 chars)".
        serde_json::json!([{
            "repo": "faktura", "path": "app/clients/search/route.ts", "line": 19,
            "rule_id": "AI-PERMISSIVE-CORS-WITH-CREDENTIALS",
            "snippet": "'Access-Control-Allow-Credentials': 'true',",
            "consequence": false, "severity": "high",
            "headline": "Arbitrary Origin reflected with Allow-Credentials true"
        }]),
        // lumen: the redacted evidence was "...igin (27 chars)".
        serde_json::json!([{
            "repo": "lumen", "path": "middleware.ts", "line": 40,
            "rule_id": "AI-CORS-ORIGIN-REFLECTION-WITH-CREDENTIALS",
            "snippet": "res.headers.set('Access-Control-Allow-Origin', req.headers.get('origin') ?? '*')",
            "consequence": false, "severity": "high",
            "headline": "CORS reflects any Origin (or '*') with Allow-Credentials true on all /api routes"
        }]),
    ];
    for cell in cells {
        let csv = format!("id,also_locations\n1,{}\n", csv_quote(&cell.to_string()));
        let problems = secrets::csv_problems("csv/all-findings.csv", csv.as_bytes());
        assert!(problems.is_empty(), "{cell}: {problems:?}");
    }
}

/// The header-name exemption must not be a way past the gate: a real key beside a header name, in
/// the same snippet or in the next site, is still refused, as is a header-shaped passphrase made
/// of ordinary words and a mixed-case hyphenated random token.
#[test]
fn a_real_key_beside_an_http_header_name_in_a_csv_cell_is_still_blocked() {
    let key = "Qm3vX9pL2sKd8Rt4Wn7Yz1Hb5Jc0Tf6";
    let cells = [
        serde_json::json!([{
            "rule_id": "AI-PERMISSIVE-CORS-WITH-CREDENTIALS",
            "snippet": "'Access-Control-Allow-Credentials': 'true',"
        }, {
            "rule_id": "SEC-API-KEY-1", "snippet": key
        }]),
        serde_json::json!([{
            "rule_id": "AI-PERMISSIVE-CORS-WITH-CREDENTIALS",
            "snippet": format!("'Access-Control-Allow-Credentials': 'true', apiKey: '{key}'")
        }]),
        // A passphrase shaped like a header name, named by a credentials rule id.
        serde_json::json!([{
            "rule_id": "SEC-HARDCODED-CREDENTIALS-1",
            "snippet": "password = 'Correct-Horse-Battery-Staple-Mountain'"
        }]),
        // Hyphen-joined random chunks that only look like words.
        serde_json::json!([{
            "rule_id": "SEC-HARDCODED-CREDENTIALS-1",
            "snippet": "token = 'Qm3vX9-pL2sKd8-Rt4Wn7-Yz1Hb5Jc0'"
        }]),
    ];
    for cell in cells {
        let csv = format!("id,also_locations\n1,{}\n", csv_quote(&cell.to_string()));
        let problems = secrets::csv_problems("csv/all-findings.csv", csv.as_bytes());
        assert!(!problems.is_empty(), "missed in {cell}");
        let shown = format!("{problems:?}");
        assert!(!shown.contains(key), "{shown}");
    }
}

/// D6 review: the other security header names a merged CORS or hardening site can quote, beside a
/// rule id that names a key, a token, a secret or a credential, are not credentials either.
#[test]
fn more_http_header_names_beside_a_secret_naming_rule_id_in_a_csv_cell_are_clean() {
    for header in [
        "X-Permitted-Cross-Domain-Policies",
        "Upgrade-Insecure-Requests",
        "Public-Key-Pins-Report-Only",
    ] {
        for rule in [
            "SEC-HARDCODED-KEY-1",
            "SEC-TOKEN-IN-HEADER-1",
            "SEC-SECRET-LEAK-1",
            "AI-WEAK-CREDENTIALS-HEADERS",
        ] {
            let cell = serde_json::json!([{
                "rule_id": rule,
                "snippet": format!("res.setHeader('{header}', value)"),
            }]);
            let csv = format!("id,also_locations\n1,{}\n", csv_quote(&cell.to_string()));
            let problems = secrets::csv_problems("csv/all-findings.csv", csv.as_bytes());
            assert!(problems.is_empty(), "{header} beside {rule}: {problems:?}");
        }
    }
}
