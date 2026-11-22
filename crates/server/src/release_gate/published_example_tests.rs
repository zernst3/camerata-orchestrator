//! V1 item 25: a published documentation key (Stripe's docs test key) is MASKED in a client
//! package like any other key, and the gate proves it over the shipped bytes: an unmasked one
//! fails the package on every surface, naming file and shape, never printing the value. Keys that
//! are public by design still ship readable. Every value is built at runtime by
//! `camerata_secrets::fixtures`; this file holds no secret-shaped literal.

use std::collections::HashMap;

use camerata_secrets::fixtures::{body, jwt, stripe_docs_test_key};
use camerata_secrets::{detect_all, Twin};

use super::tests::{client_options, package, plant, scan_report, Surface, ALL_SURFACES};
use super::*;
use crate::onboard::Finding;
use crate::report_export::{build_report_json, render_data_json};

fn blocked(package: ProductPackage) -> GateFailure {
    match release(package) {
        Err(ReleaseError::Blocked(failure)) => failure,
        Err(other) => panic!("expected the gate to block, got {other}"),
        Ok(_) => panic!("expected the gate to block, but the package was released"),
    }
}

/// A package built through the normal export path around a report with `findings`.
fn exported(findings: Vec<Finding>) -> ProductPackage {
    let mut report = scan_report();
    report.findings = findings;
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
        "acme-inspection",
        (json, data, b"%PDF-1.7 stand-in for the gate tests".to_vec()),
    )
    .expect("the package assembles")
}

/// A finding whose snippet and detail carry `key`, as a ruled-out row would.
fn finding_with(key: &str) -> Finding {
    Finding {
        repo: "acme/portal".to_string(),
        path: "src/billing.js".to_string(),
        line: 4,
        rule_id: "SEC-NO-VENDOR-TOKEN-1".to_string(),
        severity: "low".to_string(),
        snippet: format!("const stripe = require('stripe')('{key}');"),
        detail: format!("The documentation key {key} is the one every Stripe snippet uses."),
        ..Finding::default()
    }
}

fn every_text_member(package: &ProductPackage) -> Vec<(String, String)> {
    let mut members = vec![
        ("data.json".to_string(), package.data_json.clone()),
        (
            "findings.json".to_string(),
            String::from_utf8_lossy(&package.findings_json).into_owned(),
        ),
        ("README.txt".to_string(), package.readme.clone()),
    ];
    for (path, bytes) in &package.datasets {
        members.push((path.clone(), String::from_utf8_lossy(bytes).into_owned()));
    }
    members
}

#[test]
fn the_gate_refuses_an_unmasked_published_example_key_on_every_surface_naming_file_and_shape() {
    let key = stripe_docs_test_key();
    for surface in ALL_SURFACES {
        let mut pkg = package();
        plant(&mut pkg, surface, &format!("STRIPE_SECRET_KEY={key}"));
        let failure = blocked(pkg);
        let problems: Vec<&Problem> = failure
            .report
            .checks
            .iter()
            .filter(|o| o.check == CheckId::Secrets)
            .flat_map(|o| o.problems.iter())
            .collect();
        assert!(!problems.is_empty(), "{surface:?}: no secrets problem");
        let shown = format!("{failure}\n{}", failure.report.problems_json());
        assert!(!shown.contains(&key), "{surface:?}: the value was printed");
        assert!(
            problems.iter().any(|q| q.message.contains("Stripe test")),
            "{surface:?}: the shape is not named: {problems:?}"
        );
        assert!(
            problems.iter().all(|q| !q.file.is_empty()),
            "{surface:?}: a problem names no file"
        );
    }
}

#[test]
fn the_normal_export_path_masks_the_published_example_key_and_the_gate_accepts_it() {
    let key = stripe_docs_test_key();
    let pkg = exported(vec![finding_with(&key)]);
    for (name, text) in every_text_member(&pkg) {
        assert!(!text.contains(&key), "{name} carries the key unmasked");
    }
    let xlsx_strings = super::archive::xlsx_strings(&pkg.xlsx).expect("workbook reads");
    assert!(
        xlsx_strings.iter().all(|s| !s.contains(&key)),
        "the workbook carries the key unmasked"
    );
    let findings = String::from_utf8_lossy(&pkg.findings_json).into_owned();
    assert!(
        findings.contains("published example"),
        "the mask must say it is a public example: {findings}"
    );
    release(pkg).expect("the masked package must clear the gate");
}

#[test]
fn keys_that_are_public_by_design_still_ship_unmasked() {
    let publishable = format!(
        "pk_live_{}",
        body(40, "abcdefghijklmnopqrstuvwxyz0123456789", 51)
    );
    let anon = jwt(Some("anon"));
    let lines = [
        format!("NEXT_PUBLIC_STRIPE_KEY={publishable}"),
        format!("NEXT_PUBLIC_SUPABASE_ANON_KEY={anon}"),
    ];
    for line in &lines {
        let twins: Vec<Option<Twin>> = detect_all(line).into_iter().map(|m| m.twin).collect();
        assert!(
            !twins.is_empty() && twins.iter().all(Option::is_some),
            "the fixture must be a twin"
        );
    }
    for surface in ALL_SURFACES {
        let mut pkg = package();
        plant(&mut pkg, surface, &lines.join("\n"));
        // The stem is a file name, not a client file's text: the gate reads it differently.
        if matches!(surface, Surface::Stem) {
            continue;
        }
        release(pkg).unwrap_or_else(|e| {
            panic!("{surface:?}: a public-by-design key must not block the export: {e}")
        });
    }
    let pkg = exported(vec![finding_with(&publishable)]);
    let findings = String::from_utf8_lossy(&pkg.findings_json).into_owned();
    assert!(
        findings.contains(&publishable),
        "the publishable key was masked"
    );
}

#[test]
fn the_long_alphanumeric_runs_in_findings_json_are_only_hashes_and_commit_ids() {
    let pkg = exported(vec![finding_with(&stripe_docs_test_key())]);
    let doc: serde_json::Value = serde_json::from_slice(&pkg.findings_json).expect("parses");
    let mut strings = Vec::new();
    collect_strings(&doc, &mut strings);
    let mut runs = 0;
    for text in &strings {
        for run in text.split(|c: char| !c.is_ascii_alphanumeric()) {
            if run.len() < 32 {
                continue;
            }
            runs += 1;
            assert!(
                run.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f'))
                    && matches!(run.len(), 40 | 64),
                "a long alphanumeric run in findings.json is neither a sha1 nor a sha256 hex \
                 digest (length {})",
                run.len()
            );
        }
    }
    assert!(runs > 0, "the fixture has no long run to prove the point");
    assert!(
        strings
            .iter()
            .all(|s| camerata_secrets::first_secret(s).is_none()),
        "the detector reads a secret in findings.json"
    );
}

fn collect_strings(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::String(s) => out.push(s.clone()),
        serde_json::Value::Array(items) => items.iter().for_each(|v| collect_strings(v, out)),
        serde_json::Value::Object(map) => {
            for (k, v) in map {
                out.push(k.clone());
                collect_strings(v, out);
            }
        }
        _ => {}
    }
}
