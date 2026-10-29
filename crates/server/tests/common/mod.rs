//! Shared fixtures for the integration tests that read a multi-repo project's export.

#![allow(dead_code)]

use std::collections::HashMap;

use camerata_api_types::audit::{DeviationKind, FindingKind};
use camerata_server::onboard::{
    AuditedRef, Finding, LanguageVolume, MergedLocation, RepoVolume, ScanProvenance, ScanReport,
};
use camerata_server::scan_ledger::{RuleTier, ScanLedger};

pub const WEB: &str = "acme/web";
pub const API: &str = "acme/api";

/// Violated in both repos, at the SAME path and line.
pub const SQL_RULE: &str = "SEC-NO-RAW-SQL-CONCAT-1";
/// Violated in `acme/web` (a deviation), checked and clean in `acme/api`: one pattern row.
pub const PAGINATION_RULE: &str = "ARCH-PAGINATION-TEST-1";
/// Violated in `acme/web`, not applicable in `acme/api`: no pattern row.
pub const LOGGING_RULE: &str = "ARCH-LOGGING-TEST-1";
/// Clean in `acme/web`, not applicable in `acme/api`: healthy where it applies, and the list
/// must say so.
pub const HEALTHY_RULE: &str = "ARCH-HEALTHY-TEST-1";

const NO_TS_FILES: &str = "no typescript files matched this scan's file set";

fn finding(repo: &str, rule: &str, path: &str, line: usize, severity: &str) -> Finding {
    Finding {
        repo: repo.to_string(),
        path: path.to_string(),
        line,
        rule_id: rule.to_string(),
        severity: severity.to_string(),
        snippet: format!("snippet-{repo}-{line}"),
        detail: format!("detail for {rule} in {repo}"),
        kind: Some(FindingKind::Defect),
        ..Finding::default()
    }
}

fn ran(ledger: &mut ScanLedger, repo: &str, rule: &str, emitted: usize) {
    ledger.record_rule(repo, rule, RuleTier::Semantic, true, None, 5, emitted);
}

fn not_applicable(ledger: &mut ScanLedger, repo: &str, rule: &str) {
    ledger.record_rule(
        repo,
        rule,
        RuleTier::Semantic,
        false,
        Some(NO_TS_FILES.to_string()),
        0,
        0,
    );
}

fn provenance(repos: &[&str]) -> ScanProvenance {
    ScanProvenance {
        audited_refs: repos
            .iter()
            .map(|r| AuditedRef {
                repo: (*r).to_string(),
                sha: Some("abcdef1234567890".to_string()),
                branch: Some("main".to_string()),
                ..AuditedRef::default()
            })
            .collect(),
        audit_model: None,
        calibration_model: None,
        clone_judge_model: None,
        mode: "parallel".to_string(),
        thorough: false,
        deep: false,
        rules_fingerprint: "fp".to_string(),
        audited_rule_ids: [SQL_RULE, PAGINATION_RULE, LOGGING_RULE, HEALTHY_RULE]
            .iter()
            .map(ToString::to_string)
            .collect(),
        camerata_version: "0.0.0-test".to_string(),
        osv_scanner_version: None,
        vulnerability_data: None,
        started_at: "2026-10-21T00:00:00Z".to_string(),
        finished_at: "2026-10-21T00:05:00Z".to_string(),
        model_sessions: Vec::new(),
    }
}

/// The two-repo export fixture: two TypeScript services, `acme/web` and `acme/api`, whose files
/// share the path `src/index.ts`.
///
/// | rule | acme/web | acme/api |
/// |---|---|---|
/// | `SEC-NO-RAW-SQL-CONCAT-1` | violated at `src/index.ts:12` (+ one absorbed site) | violated at `src/index.ts:12` |
/// | `ARCH-PAGINATION-TEST-1` | violated (a deviation) | checked, clean |
/// | `ARCH-LOGGING-TEST-1` | violated | not applicable (no TypeScript files) |
/// | `ARCH-HEALTHY-TEST-1` | checked, clean | not applicable |
pub fn two_repo_report() -> ScanReport {
    let mut sql_web = finding(WEB, SQL_RULE, "src/index.ts", 12, "high");
    sql_web.also_locations = vec![MergedLocation::site(
        WEB,
        "src/other.ts",
        7,
        SQL_RULE,
        "format!(...)",
        false,
    )];
    let sql_api = finding(API, SQL_RULE, "src/index.ts", 12, "high");
    let mut pagination = finding(WEB, PAGINATION_RULE, "src/list.ts", 3, "medium");
    pagination.kind = Some(FindingKind::Deviation(DeviationKind {
        deviating_sites: 1,
        assessed_sites: Some(4),
        convention_option_id: Some("cursor".to_string()),
        convention_label: "cursor pagination".to_string(),
    }));
    let logging = finding(WEB, LOGGING_RULE, "src/log.ts", 1, "medium");

    let mut ledger = ScanLedger::new();
    ran(&mut ledger, WEB, SQL_RULE, 2);
    ran(&mut ledger, API, SQL_RULE, 1);
    ran(&mut ledger, WEB, PAGINATION_RULE, 1);
    ran(&mut ledger, API, PAGINATION_RULE, 0);
    ran(&mut ledger, WEB, LOGGING_RULE, 1);
    not_applicable(&mut ledger, API, LOGGING_RULE);
    ran(&mut ledger, WEB, HEALTHY_RULE, 0);
    not_applicable(&mut ledger, API, HEALTHY_RULE);

    ScanReport {
        run_id: None,
        repos_not_inspected: Vec::new(),
        recommendations_by_repo: Vec::new(),
        patterns_across_services: Default::default(),
        volume_by_repo: vec![
            RepoVolume {
                repo: WEB.to_string(),
                files: 6,
                lines: 300,
            },
            RepoVolume {
                repo: API.to_string(),
                files: 4,
                lines: 100,
            },
        ],
        repos: vec![WEB.to_string(), API.to_string()],
        stacks: Vec::new(),
        files_scanned: 10,
        test_file_count: 0,
        files_excluded: 0,
        excluded_noise_lines: None,
        code_chars: 5000,
        code_lines: 400,
        code_lines_by_language: vec![LanguageVolume {
            language: "TypeScript".to_string(),
            lines: 400,
        }],
        excluded_mechanical_rules: Vec::new(),
        findings: vec![sql_web, sql_api, pagination, logging],
        proposed_rules: Vec::new(),
        gated: false,
        blocked: false,
        ai_blocked_reason: None,
        ai_error: None,
        message: None,
        actual_usage: None,
        deep: None,
        coverage_notes: Vec::new(),
        provenance: provenance(&[WEB, API]),
        recommendations: HashMap::new(),
        failed_passes: Vec::new(),
        ledger,
    }
}

/// The same project as one repo: only `acme/web`, with the rows and ledger entries it had.
pub fn one_repo_report() -> ScanReport {
    let mut r = two_repo_report();
    r.repos = vec![WEB.to_string()];
    r.findings.retain(|f| f.repo == WEB);
    r.volume_by_repo.retain(|v| v.repo == WEB);
    r.provenance = provenance(&[WEB]);
    let mut ledger = ScanLedger::new();
    ran(&mut ledger, WEB, SQL_RULE, 2);
    ran(&mut ledger, WEB, PAGINATION_RULE, 1);
    ran(&mut ledger, WEB, LOGGING_RULE, 1);
    ran(&mut ledger, WEB, HEALTHY_RULE, 0);
    r.ledger = ledger;
    r
}
