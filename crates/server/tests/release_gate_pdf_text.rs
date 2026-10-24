#![allow(clippy::unwrap_used)]
//! The PDF smoke tests for the release gate (docs/design/2026-10-23_secret-redaction-and-
//! release-gate.md section 8.6). Typst-present-only: skipped with a note when it is absent.
//!
//! The gate's check of record on the PDF reads `data.json` (the only data the Typst template
//! reads), because Typst stores page text in compressed content streams with subset fonts and a
//! byte scan of the PDF is meaningless. These tests close the loop from the other side: they
//! extract the RENDERED text with `pdf-extract` and prove (1) a package built from findings that
//! carried secrets renders only the redacted forms, and (2) the extraction approach CAN fail, so
//! a clean result means something.

use std::collections::HashMap;

use camerata_secrets::fixtures::planted_all;
use camerata_server::onboard::{
    AuditedRef, Finding, LanguageVolume, ScanProvenance, ScanReport, SealedReport,
};
use camerata_server::release_gate::{audit_zip, build_product_package, release};
use camerata_server::report_export::{
    build_report_json, compile_pdf_from_data, render_data_json, ReportOptions,
};
use serde_json::Value;

fn typst_on_path() -> bool {
    std::process::Command::new("typst")
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .is_some()
}

/// One high-severity finding per fixture shape, each carrying its secret in the snippet, the
/// detail and the fix. The values are built at runtime by `camerata_secrets::fixtures`.
fn report_with_one_finding_per_shape() -> SealedReport {
    let findings: Vec<Finding> = planted_all()
        .into_iter()
        .enumerate()
        .map(|(i, p)| Finding {
            repo: "acme/portal".to_string(),
            path: format!("src/config{i}.env"),
            line: i + 1,
            rule_id: "SEC-NO-VENDOR-TOKEN-1".to_string(),
            severity: "high".to_string(),
            snippet: p.text.clone(),
            detail: format!("A credential is committed here: {}", p.text),
            fix_specific: Some(format!(
                "Move the value in {} to your secrets manager.",
                p.text
            )),
            ..Finding::default()
        })
        .collect();
    ScanReport {
        repos: vec!["acme/portal".to_string()],
        stacks: Vec::new(),
        files_scanned: 10,
        test_file_count: 0,
        files_excluded: 0,
        code_chars: 5000,
        code_lines: 400,
        code_lines_by_language: vec![LanguageVolume {
            language: "Rust".to_string(),
            lines: 400,
        }],
        excluded_mechanical_rules: Vec::new(),
        findings,
        proposed_rules: Vec::new(),
        gated: false,
        blocked: false,
        ai_blocked_reason: None,
        ai_error: None,
        message: None,
        actual_usage: None,
        deep: None,
        coverage_notes: Vec::new(),
        provenance: ScanProvenance {
            audited_refs: vec![AuditedRef {
                repo: "acme/portal".to_string(),
                sha: Some("abcdef1234567890".to_string()),
                branch: Some("main".to_string()),
                dirty: false,
                ..AuditedRef::default()
            }],
            audit_model: None,
            calibration_model: None,
            clone_judge_model: None,
            mode: "parallel".to_string(),
            thorough: false,
            deep: false,
            rules_fingerprint: "fp".to_string(),
            audited_rule_ids: vec!["SEC-NO-VENDOR-TOKEN-1".to_string()],
            camerata_version: "0.0.0-test".to_string(),
            osv_scanner_version: None,
            started_at: "2026-10-21T00:00:00Z".to_string(),
            finished_at: "2026-10-21T00:05:00Z".to_string(),
            model_sessions: Vec::new(),
        },
        recommendations: HashMap::new(),
        failed_passes: Vec::new(),
        ledger: camerata_server::scan_ledger::ScanLedger::new(),
    }
    .seal()
}

/// Whether any line of `value` is present in `text` (a PEM body leaks if any one line does).
fn contains_value(text: &str, value: &str) -> bool {
    value.lines().any(|line| text.contains(line))
}

#[tokio::test]
async fn pdf_text_contains_the_redacted_form_and_never_the_value() {
    if !typst_on_path() {
        eprintln!("skipping pdf_text_contains_the_redacted_form_and_never_the_value: no typst");
        return;
    }
    let sealed = report_with_one_finding_per_shape();
    let package = build_product_package(&sealed, &HashMap::new(), None, &ReportOptions::default())
        .await
        .expect("the package assembles");
    let pdf = package.pdf.clone();
    let released = release(package).expect("redacted findings clear the gate");

    let text = pdf_extract::extract_text_from_mem(&pdf).expect("the PDF's text extracts");
    // Typst wraps long tokens, so every assertion also runs over the text with line breaks
    // removed (the sweep scan needed the joined variant to see a wrapped key).
    let joined: String = text.chars().filter(|c| *c != '\n').collect();

    for p in planted_all() {
        for haystack in [&text, &joined] {
            assert!(
                !contains_value(haystack, &p.value),
                "{:?}: the rendered PDF text carries the value",
                p.shape
            );
        }
    }
    assert!(
        joined.contains('\u{2026}'),
        "the redacted forms (with their ellipsis) are what the PDF shows"
    );
    assert!(
        joined.contains("sb_secret_\u{2026}") || joined.contains("sk_live_\u{2026}"),
        "a vendor prefix survives in the redacted form: {joined}"
    );

    // And the gate, given the extracted text as the CLI does, agrees.
    let report = audit_zip(&released.bytes, Some(&text));
    assert!(
        report.is_clean(),
        "the gate over zip plus PDF text: {:?}",
        report.problems().collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn pdf_text_extraction_detects_a_raw_secret_when_the_gate_is_bypassed() {
    if !typst_on_path() {
        eprintln!(
            "skipping pdf_text_extraction_detects_a_raw_secret_when_the_gate_is_bypassed: no typst"
        );
        return;
    }
    // Bypass construction-time redaction and the gate on purpose: render a hand-built data.json
    // whose every `snippet` is a raw key, then look at what the PDF shows. This proves the
    // extraction approach CAN fail (a check that can never fail proves nothing), and that the
    // data.json check is the load-bearing one: a raw value in data.json is a raw value in the PDF.
    let sealed = report_with_one_finding_per_shape();
    let json = build_report_json(&sealed, &HashMap::new(), None, &ReportOptions::default());
    let mut data: Value =
        serde_json::from_str(&render_data_json(&json).expect("renders")).expect("parses");
    let raw =
        camerata_secrets::fixtures::planted(camerata_secrets::SecretShape::StripeLiveSecretKey)
            .value;
    set_every_snippet(&mut data, &raw);
    let pdf = compile_pdf_from_data(&data.to_string())
        .await
        .expect("typst compiles the hand-built data");

    let text = pdf_extract::extract_text_from_mem(&pdf).expect("the PDF's text extracts");
    let joined: String = text.chars().filter(|c| *c != '\n').collect();
    assert!(
        camerata_secrets::first_secret(&text).is_some()
            || camerata_secrets::first_secret(&joined).is_some(),
        "a raw key in data.json must be visible in the extracted PDF text"
    );
}

fn set_every_snippet(value: &mut Value, raw: &str) {
    match value {
        Value::Object(map) => {
            for (key, item) in map.iter_mut() {
                if key == "snippet" && item.is_string() {
                    *item = Value::String(format!("const key = \"{raw}\";"));
                } else {
                    set_every_snippet(item, raw);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|i| set_every_snippet(i, raw)),
        _ => {}
    }
}
