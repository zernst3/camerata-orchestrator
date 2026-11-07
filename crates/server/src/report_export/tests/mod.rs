//! Unit tests for `report_export`, grouped by area. The fixture helpers other test modules import
//! (`finding`, `report_with`, `empty_opts`, `which_typst`) live here.

use super::*;
use crate::onboard::{AuditedRef, CoverageNote, LanguageVolume, ScanProvenance};

mod brand_template_review;
mod citations_fixes;
mod headlines;
mod headlines_scorecard;
mod healthy_pdf_marking;
mod serial_commas;
mod severity_dispositions;
mod text_hygiene;
mod whole_file;

pub(super) fn finding(rule_id: &str, path: &str, line: usize, severity: &str) -> Finding {
    Finding {
        repo: "owner/repo".to_string(),
        path: path.to_string(),
        line,
        rule_id: rule_id.to_string(),
        severity: severity.to_string(),
        snippet: format!("snippet-{line}"),
        detail: format!("detail for {rule_id}"),
        ..Finding::default()
    }
}

pub(super) fn report_with(findings: Vec<Finding>, audited_rule_ids: Vec<&str>) -> ScanReport {
    ScanReport {
        run_id: None,
        tree: None,
        discarded: Vec::new(),
        repos_not_inspected: Vec::new(),
        recommendations_by_repo: Vec::new(),
        patterns_across_services: Default::default(),
        volume_by_repo: Default::default(),
        repos: vec!["owner/repo".to_string()],
        stacks: Vec::new(),
        files_scanned: 10,
        test_file_count: 0,
        files_excluded: 2,
        excluded_noise_lines: None,
        code_chars: 5000,
        code_lines: 400,
        code_lines_by_language: vec![
            LanguageVolume {
                language: "TypeScript".to_string(),
                lines: 300,
            },
            LanguageVolume {
                language: "Rust".to_string(),
                lines: 100,
            },
        ],
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
            group_review_model: None,
            audited_refs: vec![AuditedRef {
                repo: "owner/repo".to_string(),
                sha: Some("abcdef1234567890".to_string()),
                branch: Some("main".to_string()),
                dirty: false,
                ..AuditedRef::default()
            }],
            audit_model: Some("test-model".to_string()),
            calibration_model: Some("test-cal-model".to_string()),
            clone_judge_model: None,
            mode: "parallel".to_string(),
            thorough: false,
            deep: false,
            rules_fingerprint: "fp".to_string(),
            audited_rule_ids: audited_rule_ids.into_iter().map(String::from).collect(),
            camerata_version: "0.0.0-test".to_string(),
            osv_scanner_version: Some("1.9.0".to_string()),
            vulnerability_data: None,
            started_at: "2026-07-23T00:00:00Z".to_string(),
            finished_at: "2026-07-23T00:05:00Z".to_string(),
            model_sessions: Vec::new(),
        },
        recommendations: std::collections::HashMap::new(),
        failed_passes: Vec::new(),
        ledger: crate::scan_ledger::ScanLedger::new(),
    }
}

fn wire(state: &str, reason: &str, bucket: &str) -> DispositionWire {
    DispositionWire {
        state: state.to_string(),
        reason: reason.to_string(),
        bucket: bucket.to_string(),
        confirmed_by_client: false,
    }
}

pub(super) fn empty_opts() -> ReportOptions {
    ReportOptions::default()
}

/// Options for an internal export, the only kind that carries failure wording.
pub(super) fn internal_opts() -> ReportOptions {
    ReportOptions {
        internal: true,
        ..ReportOptions::default()
    }
}

/// Best-effort `typst` presence check for the compile test's skip gate (mirrors the
/// same PATH lookup `compile_pdf` itself relies on via `Command::new("typst")`, just
/// synchronous and side-effect-free here).
pub(super) fn which_typst() -> Option<()> {
    camerata_test_env::typst_on_path().then_some(())
}
