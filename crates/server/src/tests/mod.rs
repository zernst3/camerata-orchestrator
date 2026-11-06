//! Handler tests for `lib.rs`, grouped by area. Shared helpers live here so every group can use them.

use super::*;
use axum::body::Body;
use axum::http::Request;
use http_body_util::BodyExt;
use tower::ServiceExt;

async fn body_json(resp: Response) -> serde_json::Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

mod credentials_engagement;
mod exports_context;
mod runs_feedback;
mod scan_dedup_ci;
mod settings_projects;
mod uow_design_drift;
mod workitems_chat;

fn arm_rule(id: &str, scope: &str, repos: &[&str]) -> crate::arm::ArmRule {
    crate::arm::ArmRule {
        id: id.to_string(),
        title: format!("T {id}"),
        directive: format!("D {id}"),
        option: None,
        enforcement: "structured".to_string(),
        scope: scope.to_string(),
        conformance: None,
        repos: repos.iter().map(|s| s.to_string()).collect(),
    }
}

/// Helper: build a minimal ScanReport with one active finding.
fn make_scan_report(rule_id: &str) -> crate::onboard::ScanReport {
    crate::onboard::ScanReport {
        run_id: None,
        tree: None,
        discarded: Vec::new(),
        repos_not_inspected: Vec::new(),
        recommendations_by_repo: Vec::new(),
        patterns_across_services: Default::default(),
        volume_by_repo: Default::default(),
        repos: vec!["owner/repo".to_string()],
        stacks: Vec::new(),
        files_scanned: 1,
        test_file_count: 0,
        files_excluded: 0,
        excluded_noise_lines: None,
        code_chars: 100,
        code_lines: 10,
        code_lines_by_language: Vec::new(),
        excluded_mechanical_rules: Vec::new(),
        findings: vec![crate::onboard::Finding {
            could_be_intentional: false,
            site_key: String::new(),
            group_path: Vec::new(),
            group_role: crate::review_tree::GroupRole::Primary,
            review: Default::default(),
            flagged_by: Vec::new(),
            possible_duplicate_of: None,
            repo: "owner/repo".to_string(),
            kind: None,
            path: "src/main.rs".to_string(),
            line: 42,
            rule_id: rule_id.to_string(),
            severity: "high".to_string(),
            snippet: String::new(),
            detail: "test finding".to_string(),
            status: "active".to_string(),
            also_matches: Vec::new(),
            preview: false,
            preview_tool: None,
            in_test: false,
            needs_review: false,
            confidence: None,
            effort: None,
            category: None,
            located: true,
            captures: Default::default(),
            evaluated_option_id: None,
            also_locations: Vec::new(),
            fix_specific: None,
            calibration_rationale: None,
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
        provenance: crate::onboard::ScanProvenance::default(),
        recommendations: std::collections::HashMap::new(),
        failed_passes: Vec::new(),
        ledger: crate::scan_ledger::ScanLedger::new(),
    }
}
