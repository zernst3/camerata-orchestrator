//! H4 item 3: the release gate admits the optional triage log and scans it. Built through the
//! production assembly around a stand-in PDF, like the gate's own tests.

use std::collections::HashMap;

use camerata_secrets::fixtures::planted;
use camerata_secrets::SecretShape;

use super::tests::scan_report;
use super::*;
use crate::report_export::{build_report_json, render_data_json};
use crate::xlsx_export::triage_log::TRIAGE_LOG_CSV;

const STEM: &str = "acme-inspection";
const STAND_IN_PDF: &[u8] = b"%PDF-1.7 stand-in for the gate tests";

fn package_with(options: &ReportOptions) -> ProductPackage {
    let sealed = scan_report().seal();
    let json = build_report_json(&sealed, &HashMap::new(), None, options);
    let data = render_data_json(&json).expect("data.json renders");
    assemble_package(
        &sealed,
        &HashMap::new(),
        None,
        options,
        STEM,
        (json, data, STAND_IN_PDF.to_vec()),
    )
    .expect("the package assembles")
}

/// A client engagement: the confidentiality marking names the client, so the gate's marking
/// check (every PDF page and workbook sheet) has a real line to find.
fn base_options() -> ReportOptions {
    ReportOptions {
        client_name: "Example Client Ltd".to_string(),
        ..ReportOptions::default()
    }
}

fn with_triage_log() -> ReportOptions {
    ReportOptions {
        triage_log: true,
        ..base_options()
    }
}

#[test]
fn a_package_carries_the_triage_log_only_when_asked_and_the_gate_clears_both() {
    let default = package_with(&base_options());
    assert!(default.datasets.iter().all(|(p, _)| p != TRIAGE_LOG_CSV));
    release(default).expect("a package without the triage log clears the gate");

    let asked = package_with(&with_triage_log());
    assert!(asked.datasets.iter().any(|(p, _)| p == TRIAGE_LOG_CSV));
    let released = release(asked).expect("a package with the triage log clears the gate");
    let entries = read_zip_entries(&released.bytes).expect("a zip");
    assert!(entries.contains_key(TRIAGE_LOG_CSV));
    let manifest: serde_json::Value =
        serde_json::from_slice(&entries["manifest.json"]).expect("manifest parses");
    let listed = manifest["files"]
        .as_array()
        .expect("files")
        .iter()
        .any(|f| f["path"] == TRIAGE_LOG_CSV);
    assert!(listed, "the manifest lists the triage log: {manifest}");
}

#[test]
fn the_gate_scans_the_triage_log_for_secrets() {
    let p = planted(SecretShape::StripeLiveSecretKey);
    let mut package = package_with(&with_triage_log());
    for (path, bytes) in package.datasets.iter_mut() {
        if path == TRIAGE_LOG_CSV {
            bytes.extend_from_slice(format!("AI-LEAK,leak,1,0,0,0,1,\"{}\"\n", p.text).as_bytes());
        }
    }
    let failure = match release(package) {
        Err(ReleaseError::Blocked(failure)) => failure,
        Err(other) => panic!("expected the gate to block, got {other}"),
        Ok(_) => panic!("a secret in the triage log must block the package"),
    };
    let hit = failure
        .report
        .problems()
        .any(|(check, problem)| check == CheckId::Secrets && problem.file == TRIAGE_LOG_CSV);
    assert!(hit, "{failure}");
}

#[test]
fn any_other_unexpected_csv_is_still_refused() {
    let mut package = package_with(&ReportOptions::default());
    package
        .datasets
        .push(("csv/surprise.csv".to_string(), b"a\n1\n".to_vec()));
    let failure = match release(package) {
        Err(ReleaseError::Blocked(failure)) => failure,
        Err(other) => panic!("expected the gate to block, got {other}"),
        Ok(_) => panic!("an unexpected member must block the package"),
    };
    assert!(
        failure
            .report
            .problems()
            .any(|(check, problem)| check == CheckId::DatasetSet
                && problem.file == "csv/surprise.csv")
    );
}
