//! The README of a CLIENT export says what each file is and how to import the tickets, and
//! nothing else. The operator's own (internal) README adds the provenance lines. The gate holds
//! both: an internal README equals the manifest, a client README carries none of it.

use std::collections::HashMap;

use super::tests::{client_options, rezip, scan_report};
use super::*;
use crate::onboard::ScanReport;
use crate::report_export::{build_report_json, render_data_json, ReviewedReport};

/// A fixture scan that records the AI passes as having run, so the manifest holds model roles and
/// names that a README could leak.
fn scan_with_models() -> ScanReport {
    let mut report = scan_report();
    report.provenance.audit_model = Some("claude-sonnet-5".to_string());
    report.provenance.calibration_model = Some("claude-opus-5".to_string());
    report.provenance.clone_judge_model = Some("claude-haiku-5".to_string());
    report
}

fn package_for(internal: bool) -> ProductPackage {
    let sealed = scan_with_models().seal();
    let options = ReportOptions {
        model: Some(
            crate::ModelRecord::anthropic(crate::llm::ProjectBackend::Api)
                .with_client_engagement(true),
        ),
        internal,
        ..client_options()
    };
    let reviewed = ReviewedReport::unreviewed(&sealed, &HashMap::new());
    let json = build_report_json(&reviewed, None, &options);
    let data = render_data_json(&json).expect("data.json renders");
    assemble_package(
        &reviewed,
        None,
        &options,
        "acme-inspection",
        (json, data, b"%PDF-1.7 stand-in".to_vec()),
    )
    .expect("the package assembles")
}

/// What a client README must never name: how the inspection was run.
const OPERATOR_WORDS: [&str; 14] = [
    "Model (",
    "Model backend",
    "Model provider",
    "claude-",
    "sha256:",
    "Corpus version",
    "Corpus rules applied",
    "Engine version",
    "commit",
    "Commit (",
    "Release checks",
    "Rules last reviewed",
    "Counted lines",
    "Client engagement",
];

#[test]
fn a_client_readme_names_no_model_role_hash_commit_or_release_check() {
    let package = package_for(false);
    // The manifest does hold them, so a README that stays silent is a choice and not a gap.
    let manifest = package.provenance.inspection_json();
    assert!(manifest["models"].as_array().is_some_and(|m| !m.is_empty()));
    assert!(manifest["release_checks"]
        .as_array()
        .is_some_and(|c| !c.is_empty()));
    for word in OPERATOR_WORDS {
        assert!(
            !package.readme.contains(word),
            "a client README must not name {word:?}:\n{}",
            package.readme
        );
    }
    assert!(!package.readme.contains("INTERNAL"), "{}", package.readme);
}

#[test]
fn a_client_readme_says_what_each_file_is_and_how_to_import_the_tickets() {
    let readme = package_for(false).readme;
    for expected in [
        "acme-inspection.pdf",
        "acme-inspection-findings.xlsx",
        "findings.json",
        "decisions.json",
        "csv/tickets.csv",
        "manifest.json",
        "Jira",
        "Linear",
        "GitHub Issues",
        "Group id",
    ] {
        assert!(readme.contains(expected), "missing {expected:?}:\n{readme}");
    }
    // Nothing follows the import steps but the end of the file.
    assert!(
        readme
            .trim_end()
            .ends_with(crate::tickets_paragraph_end().trim_end()),
        "{readme}"
    );
}

#[test]
fn an_internal_readme_carries_the_provenance_lines_and_the_disclaimer() {
    let readme = package_for(true).readme;
    for expected in [
        "Model backend: api\n",
        "Model (the scan): claude-sonnet-5\n",
        "Engine version: camerata ",
        "Release checks: ",
        "Repos inspected: ",
        "Prepared for: Example Client Ltd\n",
        "is not a certification, warranty or guarantee",
    ] {
        assert!(readme.contains(expected), "missing {expected:?}:\n{readme}");
    }
}

#[test]
fn a_client_package_clears_the_gate_and_an_internal_one_clears_it_as_internal() {
    release(package_for(false)).expect("a client package with a plain README is released");
    release_internal(package_for(true)).expect("an internal package with the full README");
}

#[test]
fn the_gate_refuses_a_client_readme_that_carries_a_provenance_line() {
    let released = release(package_for(false)).expect("clean");
    let entries = read_zip_entries(&released.bytes).expect("zip");
    for line in [
        "Release checks: secrets, manifest (all passed)",
        "Model (the scan): claude-sonnet-5",
        "Engine version: camerata 0.0.0",
        "Corpus version: sha256:abc",
        "Commit (acme/portal): abcdef1234567890",
        "Model backend: api",
    ] {
        let mut edited = entries.clone();
        let readme = edited.get_mut("README.txt").expect("README");
        readme.extend_from_slice(format!("\n{line}\n").as_bytes());
        let report = audit_zip(&rezip(&edited), None);
        assert!(
            report
                .checks
                .iter()
                .filter(|o| o.check == CheckId::Provenance)
                .flat_map(|o| o.problems.iter())
                .any(|p| p.file == "README.txt" && p.message.contains("client README")),
            "{line}: {:?}",
            report.problems().collect::<Vec<_>>()
        );
    }
}
