//! Run to completion: a client deliverable must never contain a failed or unfinished part.
//! Zach, 2026-10-28: "We can't ship a product to anyone that didn't fully run end-to-end
//! properly."
//!
//! * A client export of a run with a failed part, a part that did not run (spend limit or outage
//!   stop), a fatal AI error or an uninspected repo is refused, and nothing is built.
//! * An internal export of the same run is written, marked INTERNAL INCOMPLETE on the cover, in
//!   `findings.json`, in the manifest and in the README.
//! * The release gate (and `camerata release-gate <zip>`) refuses any package that records such a
//!   run, internal or not.
//! * A complete run's output carries none of this.

use std::collections::HashMap;

use super::tests::{client_options, scan_report};
use super::*;
use crate::ai_audit::{FailedPass, FailureClass};
use crate::report_export::{build_report_json, render_data_json, RunIncomplete};

const STEM: &str = "acme-inspection";
const STAND_IN_PDF: &[u8] = b"%PDF-1.7 stand-in for the gate tests";

fn failed(class: FailureClass) -> FailedPass {
    FailedPass {
        repo: "acme/portal".to_string(),
        pass: "audit pass[rust] 1/2 \u{b7} rules 1/1".to_string(),
        reason: "Anthropic API HTTP 529: overloaded".to_string(),
        files_not_reviewed: 3,
        class,
        files: vec!["src/db.rs".to_string()],
        attempts: Vec::new(),
        scope_note: false,
    }
}

fn incomplete_report() -> crate::onboard::ScanReport {
    let mut report = scan_report();
    report.failed_passes = vec![failed(FailureClass::Transport)];
    report
}

/// A package from `report` and `options` through the production assembly, around a stand-in PDF.
fn package_from(
    report: crate::onboard::ScanReport,
    options: &ReportOptions,
) -> anyhow::Result<ProductPackage> {
    let sealed = report.seal();
    crate::report_export::require_complete_run(&sealed, options)?;
    let json = build_report_json(&sealed, &HashMap::new(), None, options);
    let data = render_data_json(&json)?;
    assemble_package(
        &sealed,
        &HashMap::new(),
        None,
        options,
        STEM,
        (json, data, STAND_IN_PDF.to_vec()),
    )
}

fn internal_options() -> ReportOptions {
    ReportOptions {
        internal: true,
        ..client_options()
    }
}

#[test]
fn a_complete_run_has_no_incomplete_reasons() {
    assert!(scan_report().incomplete_reasons().is_empty());
}

#[test]
fn every_kind_of_incompleteness_is_a_reason() {
    for (name, mutate) in [
        (
            "a failed pass",
            Box::new(|r: &mut crate::onboard::ScanReport| {
                r.failed_passes.push(failed(FailureClass::Content));
            }) as Box<dyn Fn(&mut crate::onboard::ScanReport)>,
        ),
        (
            "a part that did not run (spend limit)",
            Box::new(|r| r.failed_passes.push(failed(FailureClass::SpendLimit))),
        ),
        (
            "a part that did not run (outage stop)",
            Box::new(|r| r.failed_passes.push(failed(FailureClass::OutageStop))),
        ),
        (
            "an AI error",
            Box::new(|r| r.ai_error = Some("every pass failed".to_string())),
        ),
        (
            "a repo not inspected",
            Box::new(|r| {
                r.repos_not_inspected
                    .push(("acme/gone".to_string(), "folder is gone".to_string()));
            }),
        ),
    ] {
        let mut report = scan_report();
        mutate(&mut report);
        assert!(
            !report.incomplete_reasons().is_empty(),
            "{name} must make the run incomplete"
        );
    }
}

#[test]
fn a_client_export_of_an_incomplete_run_is_refused_and_builds_nothing() {
    let err = match package_from(incomplete_report(), &client_options()) {
        Ok(_) => panic!("a client package must not be built for an incomplete run"),
        Err(e) => e,
    };
    let refusal = err
        .downcast_ref::<RunIncomplete>()
        .expect("the refusal is the typed RunIncomplete");
    assert!(
        refusal.message.contains("did not run to completion"),
        "{}",
        refusal.message
    );
    assert!(!refusal.reasons.is_empty());
    assert!(!refusal.outage);
}

#[test]
fn an_internal_export_of_an_incomplete_run_is_marked_everywhere() {
    let package =
        package_from(incomplete_report(), &internal_options()).expect("internal may export");
    let marking = "INTERNAL INCOMPLETE";
    assert!(
        package.report_json.cover.internal_incomplete.is_some(),
        "the cover carries the marking"
    );
    assert!(
        package.data_json.contains(marking),
        "the PDF's data carries it (the cover prints it)"
    );
    let findings: serde_json::Value =
        serde_json::from_slice(&package.findings_json).expect("findings.json parses");
    assert!(
        findings["provenance"]["internal_incomplete"]
            .as_str()
            .is_some_and(|s| s.contains(marking)),
        "findings.json carries it"
    );
    assert!(
        package.readme.contains(marking),
        "the README says so: {}",
        package.readme
    );
    let released = release_internal(package).expect("the internal route releases it");
    let entries = read_zip_entries(&released.bytes).expect("zip reads");
    let manifest: serde_json::Value =
        serde_json::from_slice(&entries["manifest.json"]).expect("manifest parses");
    assert_eq!(manifest["inspection"]["run_incomplete"], true);
}

#[test]
fn the_release_gate_refuses_a_package_that_records_an_incomplete_run() {
    // The client route refuses an internal package that records one.
    let package =
        package_from(incomplete_report(), &internal_options()).expect("internal may build");
    let err = release(package).expect_err("the client release refuses an incomplete package");
    let ReleaseError::Blocked(failure) = err else {
        panic!("expected a gate refusal, got {err}");
    };
    assert!(
        failure
            .report
            .problems()
            .any(|(_, p)| p.message.contains("did not run to completion")),
        "{failure}"
    );

    // And `camerata release-gate <zip>` refuses the internal zip itself.
    let package =
        package_from(incomplete_report(), &internal_options()).expect("internal may build");
    let released = release_internal(package).expect("internal release");
    let audit = audit_zip(&released.bytes, None);
    assert!(
        !audit.is_clean(),
        "a zip that records an incomplete run must not pass the gate"
    );
}

#[test]
fn a_complete_run_exports_exactly_as_before() {
    let package = package_from(scan_report(), &client_options()).expect("complete run builds");
    assert!(package.report_json.cover.internal_incomplete.is_none());
    assert!(
        !package.data_json.contains("internal_incomplete"),
        "the PDF data is unchanged for a complete run"
    );
    let findings = String::from_utf8(package.findings_json.clone()).unwrap();
    assert!(
        !findings.contains("internal_incomplete"),
        "findings.json unchanged"
    );
    assert!(!package.readme.contains("INTERNAL INCOMPLETE"));
    assert!(package.provenance.incomplete.is_empty());
    assert!(
        package
            .provenance
            .inspection_json()
            .get("run_incomplete")
            .is_none(),
        "the manifest has no new keys"
    );
    // The same words never reach a complete run's client-facing text.
    for banned in ["pass failed", "not computed"] {
        assert!(!package.data_json.contains(banned), "{banned}");
        assert!(!findings.contains(banned), "{banned}");
        assert!(!package.readme.contains(banned), "{banned}");
    }
    release(package).expect("a complete run clears the gate");
}

#[test]
fn an_outage_stop_is_flagged_as_an_outage_in_the_refusal() {
    let mut report = scan_report();
    report.failed_passes.push(failed(FailureClass::OutageStop));
    report
        .ledger
        .record_outage(crate::run_health::OutageRecord {
            trigger: crate::run_health::OutageTrigger {
                class: FailureClass::Transport,
                detail: "3 model calls in a row could not reach the provider".to_string(),
                at: "2026-10-28T10:00:00Z".to_string(),
            },
            not_run_passes: Vec::new(),
            not_run_stages: Vec::new(),
            repos_not_finished: Vec::new(),
        });
    let sealed = report.seal();
    let refusal = crate::report_export::require_complete_run(&sealed, &client_options())
        .expect_err("refused");
    assert!(refusal.outage);
}
