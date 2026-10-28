//! Tests written against the mutants `cargo mutants -p camerata-server --file
//! crates/server/src/report_export.rs` left alive on 2026-10-28 (docs/MUTATION_TESTING.md).
//! Each pins client-facing report text or a severity decision that no existing test held
//! exactly, so a change to it could ship in a client PDF unnoticed.

use super::*;

mod built_report;
mod routing;
mod text;

// ---- shared fixtures ---------------------------------------------------------------------

async fn bundled_corpus() -> camerata_rules::RuleSet {
    camerata_rules::load_corpus(std::path::Path::new(camerata_rules::DEFAULT_CORPUS_PATH))
        .await
        .expect("bundled corpus loads")
}

/// A minimal sealed-report fixture (this module's own copy, per the crate's convention that
/// test modules never import a sibling module's private helpers).
fn report_of(findings: Vec<Finding>) -> crate::onboard::ScanReport {
    use crate::onboard::{AuditedRef, ScanProvenance};
    crate::onboard::ScanReport {
        repos: vec!["owner/repo".to_string()],
        stacks: Vec::new(),
        files_scanned: 10,
        test_file_count: 0,
        files_excluded: 0,
        code_chars: 5000,
        code_lines: 400,
        code_lines_by_language: Vec::new(),
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
                repo: "owner/repo".to_string(),
                sha: Some("abcdef1234567890".to_string()),
                branch: Some("main".to_string()),
                dirty: false,
                ..AuditedRef::default()
            }],
            audit_model: Some("test-model".to_string()),
            calibration_model: None,
            clone_judge_model: None,
            mode: "parallel".to_string(),
            thorough: false,
            deep: false,
            rules_fingerprint: "fp".to_string(),
            audited_rule_ids: Vec::new(),
            camerata_version: "0.0.0-test".to_string(),
            osv_scanner_version: None,
            started_at: "2026-07-23T00:00:00Z".to_string(),
            finished_at: "2026-07-23T00:05:00Z".to_string(),
            model_sessions: Vec::new(),
        },
        recommendations: std::collections::HashMap::new(),
        failed_passes: Vec::new(),
        ledger: crate::scan_ledger::ScanLedger::new(),
    }
}

/// A rule with an authored fix and effort, so its findings stay curated (an action row with no
/// fix is held instead).
fn fixable_rule(rule_id: &str) -> camerata_rules::Rule {
    fixable_rule_in(rule_id, "test-fixture")
}

fn fixable_rule_in(rule_id: &str, domain: &str) -> camerata_rules::Rule {
    let mut rule = camerata_rules::bare_rule(rule_id, domain);
    rule.default_option = Some("default".to_string());
    rule.options = vec![camerata_rules::RuleOption {
        id: "default".to_string(),
        label: "Default".to_string(),
        directive: "do the right thing".to_string(),
        why: "because".to_string(),
        remediation: Some("Fix the thing.".to_string()),
        finding_headline: None,
        finding_detail: None,
        escalation: None,
        effort: Some("low".to_string()),
        violates: false,
    }];
    rule
}

fn curated_corpus() -> camerata_rules::RuleSet {
    let mut corpus = camerata_rules::RuleSet::default();
    for id in ["PART-NOW-1", "PART-PLAN-1", "PART-HELD-1", "PART-INFO-1"] {
        corpus.push(fixable_rule(id));
    }
    corpus
}

fn partition_report() -> crate::onboard::ScanReport {
    let finding = |rule_id: &str, line: usize, severity: &str, needs_review: bool| Finding {
        repo: "owner/repo".to_string(),
        path: "src/a.rs".to_string(),
        line,
        rule_id: rule_id.to_string(),
        severity: severity.to_string(),
        snippet: format!("snippet-{line}"),
        detail: format!("Detail for {rule_id}."),
        needs_review,
        ..Finding::default()
    };
    report_of(vec![
        finding("PART-NOW-1", 1, "critical", false),
        finding("PART-PLAN-1", 2, "medium", false),
        finding("PART-HELD-1", 3, "high", true),
        finding("PART-INFO-1", 4, "info", false),
    ])
}
