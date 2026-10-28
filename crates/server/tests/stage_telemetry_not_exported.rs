#![allow(clippy::unwrap_used)]
//! Per-stage cost telemetry is operator-only. The export format (manifest, README, workbook,
//! findings.json) is a protected area, so recording telemetry on the scan ledger must not change
//! one byte of it. This pins that: the same scan, with and without telemetry on its ledger,
//! produces identical export content, and no cost term appears in it.

use std::collections::HashMap;

use camerata_server::onboard::{Finding, ScanProvenance, ScanReport};
use camerata_server::report_export::{build_report_json, ReportOptions};
use camerata_server::scan_ledger::ScanLedger;
use camerata_server::stage_telemetry::{Stage, StageMeter};
use camerata_server::xlsx_export::build_findings_export;

fn report(ledger: ScanLedger) -> ScanReport {
    ScanReport {
        repos: vec!["acme/portal".to_string()],
        stacks: Vec::new(),
        files_scanned: 10,
        test_file_count: 0,
        files_excluded: 0,
        code_chars: 5000,
        code_lines: 400,
        code_lines_by_language: Vec::new(),
        excluded_mechanical_rules: Vec::new(),
        findings: vec![Finding {
            repo: "acme/portal".to_string(),
            path: "src/db.rs".to_string(),
            line: 12,
            rule_id: "SEC-NO-RAW-SQL-CONCAT-1".to_string(),
            severity: "high".to_string(),
            snippet: "format!(\"select {}\", x)".to_string(),
            detail: "raw SQL concatenation".to_string(),
            ..Finding::default()
        }],
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
            audit_model: None,
            calibration_model: None,
            clone_judge_model: None,
            mode: "parallel".to_string(),
            camerata_version: "0.0.0-test".to_string(),
            started_at: "2026-10-21T00:00:00Z".to_string(),
            finished_at: "2026-10-21T00:05:00Z".to_string(),
            ..ScanProvenance::default()
        },
        recommendations: HashMap::new(),
        failed_passes: Vec::new(),
        ledger,
    }
}

fn exported(report: ScanReport) -> (String, String) {
    let sealed = report.seal();
    let json = build_report_json(&sealed, &HashMap::new(), None, &ReportOptions::default());
    let findings = build_findings_export(&sealed, &HashMap::new(), None, &json, &HashMap::new());
    (
        serde_json::to_string(&json).unwrap(),
        serde_json::to_string(&findings).unwrap(),
    )
}

#[test]
fn recording_stage_telemetry_changes_nothing_in_the_client_export() {
    let meter = StageMeter::default();
    meter.record(
        Stage::ScanPass,
        &camerata_server::llm::LlmResponse {
            text: String::new(),
            model: "m".to_string(),
            backend: "api".to_string(),
            cost_usd: Some(1.25),
            input_tokens: Some(9_000),
            output_tokens: Some(700),
            cache_read_input_tokens: 4_000,
            cache_creation_input_tokens: 2_000,
            or_cache_discount: None,
            truncated: false,
        },
    );
    let mut with = ScanLedger::new();
    with.record_stage_telemetry(meter.snapshot(400));
    with.record_ai_key(camerata_server::scan_cache::AiScanKey::new(
        "claude-sonnet-5",
        None,
        &[],
    ));
    assert!(with.stage_telemetry().is_some());

    let (report_with, findings_with) = exported(report(with));
    let (report_without, findings_without) = exported(report(ScanLedger::new()));

    assert_eq!(report_with, report_without, "report.json changed");
    assert_eq!(findings_with, findings_without, "findings.json changed");
    for term in [
        "stage_telemetry",
        "ai_key",
        "directives_fingerprint",
        "prompt_schema_version",
        "cache_creation",
        "wasted",
        "cost_per_1k",
        "1.25",
    ] {
        assert!(
            !report_with.contains(term) && !findings_with.contains(term),
            "`{term}` leaked into the client export"
        );
    }
}

#[test]
fn a_ledger_written_before_telemetry_existed_still_loads() {
    let legacy = serde_json::to_string(&ScanLedger::new()).unwrap();
    let mut value: serde_json::Value = serde_json::from_str(&legacy).unwrap();
    value.as_object_mut().unwrap().remove("stage_telemetry");
    let back: ScanLedger = serde_json::from_value(value).expect("a legacy ledger deserializes");
    assert!(back.stage_telemetry().is_none());
}

/// The efficiency set and the spend-cap record (dollar figures, NOT RUN lists) ride on the same
/// operator ledger as the telemetry, so they obey the same rule: the client export is unchanged.
#[test]
fn recording_the_efficiency_set_and_a_cost_cap_changes_nothing_in_the_client_export() {
    let mut capped = ScanLedger::new();
    let mut efficiency = camerata_server::efficiency::EfficiencyConfig::default();
    efficiency.set("warm_schedule", false).unwrap();
    capped.record_efficiency(efficiency);
    capped.record_cost_cap(camerata_server::cost_cap::CostCapRecord {
        cap_usd: 7.77,
        spent_usd: 8.88,
        not_run_passes: vec![camerata_server::cost_cap::NotRunPass {
            repo: "acme/portal".to_string(),
            group: "pass[all]".to_string(),
            chunk: 1,
            n_chunks: 1,
            rule_batch: 2,
            n_rule_batches: 2,
            rule_ids: vec!["ARCH-X-1".to_string()],
        }],
        not_run_stages: Vec::new(),
        notes: vec!["batch NOT submitted".to_string()],
    });
    assert!(capped.is_cost_capped());

    let (report_with, findings_with) = exported(report(capped));
    let (report_without, findings_without) = exported(report(ScanLedger::new()));
    assert_eq!(report_with, report_without, "report.json changed");
    assert_eq!(findings_with, findings_without, "findings.json changed");
    for term in [
        "7.77",
        "8.88",
        "cost_cap",
        "max_cost",
        "efficiency",
        "warm_schedule",
    ] {
        assert!(
            !report_with.contains(term) && !findings_with.contains(term),
            "`{term}` leaked into the client export"
        );
    }
}
