#![allow(clippy::unwrap_used)]
//! The corpus's authored severity bounds hold in the LIVE scan, not only in the exports.
//!
//! # The defect this pins
//!
//! `max_severity` ceilings, `min_severity` floors and `deviation_ceiling` used to be applied
//! only at export time (`report_export::build_report_json` for the PDF,
//! `xlsx_export::partition_rows` for the workbook / CSV / JSON). The live scan copied each
//! producer's severity into the `Finding` verbatim, so the cockpit's triage view and the billed
//! PDF/xlsx showed DIFFERENT severities for the same finding on every one of the corpus rules
//! that declares a bound. `ui-core`'s triage ranking documents that it assumes the bounds were
//! "applied upstream"; nothing upstream applied them.
//!
//! # What these tests hold fixed
//!
//! Like `real_corpus_severity_ceiling_e2e.rs` and `real_corpus_severity_floor_e2e.rs`, every
//! bound asserted here is read from the REAL shipped corpus, never from a fixture, so the
//! properties cover a bound added later without editing this file. They are written against
//! the live `ScanReport` (`apply_corpus_severity_bounds` over `report.findings`, and the real
//! `audit_repos` entry point) and then CROSS-CHECKED against the export artifacts, because the
//! requirement is agreement between the two surfaces, not just that each is individually sane.
//!
//! Zero API spend: no model call anywhere in this file.

use std::collections::HashMap;
use std::path::Path;

use camerata_api_types::audit::{DeviationKind, FindingKind};
use camerata_server::ai_audit::ScanMode;
use camerata_server::onboard::{
    self, AuditedRef, Finding, LanguageVolume, ScanProvenance, ScanReport, SelectedRule,
};
use camerata_server::report_export::{build_report_json, ReportOptions};
use camerata_server::severity_bounds::apply_corpus_severity_bounds;
use camerata_server::xlsx_export::build_findings_export;

const VOCABULARY: [&str; 4] = ["critical", "high", "medium", "low"];

fn rank(sev: &str) -> u8 {
    match sev {
        "critical" => 0,
        "high" => 1,
        "medium" => 2,
        _ => 3,
    }
}

/// Load the real shipped corpus, or `None` when it isn't checked out (mirrors the
/// `path.exists()` skip guard every corpus-reading test uses).
async fn real_corpus() -> Option<camerata_rules::RuleSet> {
    let path = Path::new(camerata_rules::DEFAULT_CORPUS_PATH);
    if !path.exists() {
        return None;
    }
    Some(
        camerata_rules::load_corpus(path)
            .await
            .expect("the real corpus loads"),
    )
}

/// A located finding of `kind`: both properties are load-bearing, because the floor declines a
/// kindless row and an unlocated row (W3 / C4-R1), so a finding missing either would exercise
/// the decline path rather than the bound.
fn located(rule_id: &str, severity: &str, kind: FindingKind) -> Finding {
    Finding {
        repo: "owner/repo".to_string(),
        path: "src/a.ts".to_string(),
        line: 1,
        rule_id: rule_id.to_string(),
        severity: severity.to_string(),
        snippet: "snippet".to_string(),
        detail: format!("detail for {rule_id}"),
        kind: Some(kind),
        ..Finding::default()
    }
}

fn deviation() -> FindingKind {
    FindingKind::Deviation(DeviationKind {
        deviating_sites: 1,
        assessed_sites: Some(5),
        convention_option_id: None,
        convention_label: "the observed convention".to_string(),
    })
}

fn report_with(findings: Vec<Finding>) -> ScanReport {
    let audited_rule_ids = findings.iter().map(|f| f.rule_id.clone()).collect();
    ScanReport {
        repos: vec!["owner/repo".to_string()],
        stacks: Vec::new(),
        files_scanned: 10,
        test_file_count: 0,
        files_excluded: 2,
        code_chars: 5000,
        code_lines: 400,
        code_lines_by_language: vec![LanguageVolume {
            language: "TypeScript".to_string(),
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
                repo: "owner/repo".to_string(),
                sha: Some("abcdef1234567890".to_string()),
                branch: Some("main".to_string()),
                dirty: false,
            }],
            audit_model: Some("test-model".to_string()),
            calibration_model: Some("test-cal-model".to_string()),
            mode: "parallel".to_string(),
            thorough: false,
            deep: false,
            rules_fingerprint: "fp".to_string(),
            audited_rule_ids,
            camerata_version: "0.0.0-test".to_string(),
            osv_scanner_version: Some("1.9.0".to_string()),
            started_at: "2026-10-10T00:00:00Z".to_string(),
            finished_at: "2026-10-10T00:05:00Z".to_string(),
        },
        recommendations: HashMap::new(),
        failed_passes: Vec::new(),
        ledger: camerata_server::scan_ledger::ScanLedger::new(),
    }
}

/// The severity `findings.json` (the product export's row table, built by the same
/// `partition_rows` the workbook, CSV and JSON exports use) carries for a one-finding report.
fn exported_xlsx_severity(f: &Finding, corpus: &camerata_rules::RuleSet) -> String {
    let report = report_with(vec![f.clone()]).seal();
    let opts = ReportOptions::default();
    let json = build_report_json(&report, &HashMap::new(), Some(corpus), &opts);
    let export = build_findings_export(
        &report,
        &HashMap::new(),
        Some(corpus),
        &json,
        &opts.chosen_options,
    );
    let rows = serde_json::to_value(&export.findings).unwrap();
    let rows = rows.as_array().unwrap();
    assert_eq!(
        rows.len(),
        1,
        "{} must export exactly one findings.json row; a dropped row is not an agreement",
        f.rule_id
    );
    rows[0]["severity"].as_str().unwrap().to_string()
}

/// Every severity the PDF's JSON exports for a one-finding report, scanning both row sections
/// (a held-for-review group renders outside `curated_findings`).
fn exported_pdf_severities(f: &Finding, corpus: &camerata_rules::RuleSet) -> Vec<String> {
    let report = report_with(vec![f.clone()]).seal();
    let json = build_report_json(
        &report,
        &HashMap::new(),
        Some(corpus),
        &ReportOptions::default(),
    );
    json.curated_findings
        .iter()
        .chain(json.held_for_review_findings.iter())
        .flat_map(|g| g.sites.iter())
        .map(|s| s.severity.clone())
        .collect()
}

fn bounded(f: &Finding, corpus: &camerata_rules::RuleSet) -> Finding {
    let mut fs = vec![f.clone()];
    apply_corpus_severity_bounds(&mut fs, corpus);
    fs.remove(0)
}

/// Mirrors `req_e2_no_report_exceeds_any_ceiling_the_corpus_declares`, over the LIVE report:
/// propose `critical` for every rule that declares a ceiling and assert the live finding never
/// ranks above it.
///
/// FAILS BEFORE THE FIX: nothing applied a ceiling to `report.findings`, so every declaring
/// rule kept `critical`. The non-empty assertion turns a corpus with no ceilings into a
/// failure rather than a vacuous pass.
#[tokio::test]
async fn req_e2_no_live_scan_exceeds_any_ceiling_the_corpus_declares() {
    let Some(corpus) = real_corpus().await else {
        return;
    };
    let declaring: Vec<(String, String)> = corpus
        .iter()
        .filter_map(|r| r.max_severity.clone().map(|c| (r.id.0.clone(), c)))
        .collect();
    assert!(
        !declaring.is_empty(),
        "E2 is INERT: no corpus rule declares a ceiling, so this property is vacuous"
    );
    for (rule_id, ceiling) in &declaring {
        let mut report = report_with(vec![located(rule_id, "critical", FindingKind::Defect)]);
        apply_corpus_severity_bounds(&mut report.findings, &corpus);
        let live = &report.findings[0].severity;
        assert!(
            rank(live) >= rank(ceiling),
            "{rule_id} declares max_severity = {ceiling:?} but the LIVE scan carries {live:?}, \
             which ranks above it"
        );
    }
}

/// The floor counterpart, mirroring `req_w8_no_report_ranks_below_any_floor_the_corpus_declares`.
#[tokio::test]
async fn req_w8_no_live_scan_ranks_below_any_floor_the_corpus_declares() {
    let Some(corpus) = real_corpus().await else {
        return;
    };
    let declaring: Vec<(String, String)> = corpus
        .iter()
        .filter_map(|r| r.min_severity.clone().map(|f| (r.id.0.clone(), f)))
        .collect();
    assert!(
        !declaring.is_empty(),
        "W8 is INERT: no corpus rule declares a floor, so this property is vacuous"
    );
    for (rule_id, floor) in &declaring {
        let mut report = report_with(vec![located(rule_id, "low", FindingKind::Defect)]);
        apply_corpus_severity_bounds(&mut report.findings, &corpus);
        let live = &report.findings[0].severity;
        assert!(
            rank(live) <= rank(floor),
            "{rule_id} declares min_severity = {floor:?} but the LIVE scan carries {live:?}, \
             which ranks below it"
        );
    }
}

/// Attribution control: the bounds are per-rule, never global. A rule that declares no bound
/// keeps whatever severity its producer chose, and an unknown rule is untouched.
#[tokio::test]
async fn a_rule_that_declares_no_bound_is_untouched_live() {
    let Some(corpus) = real_corpus().await else {
        return;
    };
    // Taken from the corpus rather than hard-coded: a rule's bounds are authored data that
    // moves (`SUPABASE-RLS-DISABLED-NOT-RESTORED-1` carries a floor now), so a pinned id would
    // silently turn this control into a test of a different rule.
    let rule = corpus
        .iter()
        .find(|r| r.max_severity.is_none() && r.min_severity.is_none())
        .expect("the corpus has rules that declare no bound");
    let rule_id = rule.id.0.clone();
    for sev in VOCABULARY {
        assert_eq!(
            bounded(&located(&rule_id, sev, FindingKind::Defect), &corpus).severity,
            sev,
            "{rule_id} declares no bound, so its severity must be untouched"
        );
    }
    assert_eq!(
        bounded(
            &located("NOT-A-RULE-1", "critical", FindingKind::Defect),
            &corpus
        )
        .severity,
        "critical"
    );
}

/// Idempotence over the whole real corpus: bounding an already-bounded set changes nothing, so
/// the export-side clamps are harmless backstops.
#[tokio::test]
async fn bounding_the_real_corpus_twice_changes_nothing() {
    let Some(corpus) = real_corpus().await else {
        return;
    };
    let mut findings: Vec<Finding> = Vec::new();
    for rule in corpus.iter() {
        for sev in VOCABULARY {
            findings.push(located(&rule.id.0, sev, FindingKind::Defect));
            findings.push(located(&rule.id.0, sev, deviation()));
        }
    }
    apply_corpus_severity_bounds(&mut findings, &corpus);
    let once = findings.clone();
    apply_corpus_severity_bounds(&mut findings, &corpus);
    assert_eq!(findings, once);
}

/// THE requirement: for the same finding, the severity the live scan carries equals the
/// severity every export artifact bills. Checked against `findings.json` (the shared
/// `partition_rows` behind the workbook, CSV and JSON) for every rule that declares any bound,
/// every proposed severity and both accusing kinds, and against the PDF for Defects.
#[tokio::test]
async fn live_and_export_severities_agree_for_every_bounded_rule() {
    let Some(corpus) = real_corpus().await else {
        return;
    };
    let bounded_rules: Vec<String> = corpus
        .iter()
        .filter(|r| r.max_severity.is_some() || r.min_severity.is_some())
        .map(|r| r.id.0.clone())
        .collect();
    assert!(
        !bounded_rules.is_empty(),
        "no corpus rule declares a bound, so this agreement is vacuous"
    );
    let mut compared = 0usize;
    for rule_id in &bounded_rules {
        for sev in VOCABULARY {
            for kind in [FindingKind::Defect, deviation()] {
                let proposed = located(rule_id, sev, kind.clone());
                let live = bounded(&proposed, &corpus);
                assert_eq!(
                    exported_xlsx_severity(&proposed, &corpus),
                    live.severity,
                    "{rule_id} ({}) proposed {sev:?}: findings.json and the live scan disagree",
                    kind.tag()
                );
                // The export applied to an ALREADY-bounded finding is the backstop path; it
                // must be a fixed point.
                assert_eq!(
                    exported_xlsx_severity(&live, &corpus),
                    live.severity,
                    "{rule_id} ({}) proposed {sev:?}: the export backstop moved a bounded severity",
                    kind.tag()
                );
                compared += 1;
            }
            let defect = located(rule_id, sev, FindingKind::Defect);
            let live = bounded(&defect, &corpus);
            let pdf = exported_pdf_severities(&defect, &corpus);
            assert_eq!(
                pdf,
                vec![live.severity.clone()],
                "{rule_id} proposed {sev:?}: the PDF and the live scan disagree"
            );
        }
    }
    assert!(compared > 0);
}

// ── The real scan entry point ─────────────────────────────────────────────────────────────

fn stage_git_repo(dest: &Path, files: &[(&str, &str)]) {
    for (rel, body) in files {
        let path = dest.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }
    let g = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .current_dir(dest)
            .args(args)
            .output()
            .unwrap_or_else(|e| panic!("failed to spawn git {args:?}: {e}"));
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    g(&["init", "-q", "-b", "main"]);
    g(&["config", "user.email", "severity-bounds-e2e@camerata.local"]);
    g(&["config", "user.name", "Camerata Severity Bounds E2E Test"]);
    g(&["add", "."]);
    g(&["commit", "-q", "-m", "e2e fixture: one split import path"]);
}

async fn scan(repo_dir: &Path, corpus: Option<&camerata_rules::RuleSet>) -> onboard::ScanReport {
    let sources = vec![("e2e/split-import-path".to_string(), repo_dir.to_path_buf())];
    let selected = vec![SelectedRule {
        id: "ARCH-ONE-IMPORT-PATH-1".to_string(),
        directive: "live severity bounds e2e".to_string(),
        repos: Vec::new(),
    }];
    let (report, _manifest) = onboard::audit_repos(
        &sources,
        &selected,
        Vec::new(),
        None,
        None,
        ScanMode::Sequential,
        false,
        None,
        None,
        None,
        false,
        false,
        false, // run_ai_review: zero API spend
        true,  // run_deterministic: the architectural engine runs
        None,
        camerata_server::llm::BackendResolution::Api,
        corpus,
        &HashMap::new(),
    )
    .await;
    report
}

/// `ARCH-ONE-IMPORT-PATH-1`'s checker emits `medium` (its contract forbids emitting `low`)
/// and the corpus rule declares `max_severity = "low"`. Driven through the REAL `audit_repos`
/// with the REAL corpus, the live finding must already be `low`; with no corpus it stays
/// `medium`, which is the attribution control proving the clamp comes from the corpus bound.
///
/// FAILS BEFORE THE FIX: `arch_violation_to_finding` copied `medium` into the live finding and
/// only the exporters clamped it.
#[tokio::test]
async fn audit_repos_applies_the_corpus_ceiling_to_a_live_architectural_finding() {
    let Some(corpus) = real_corpus().await else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    stage_git_repo(
        dir.path(),
        &[
            ("lib/x.ts", "export function a() {}\n"),
            ("lib/index.ts", "export { a } from './x';\n"),
            ("app/one.ts", "import { a } from '../lib';\n"),
            ("app/two.ts", "import { a } from '../lib/x';\n"),
        ],
    );

    let with_corpus = scan(dir.path(), Some(&corpus)).await;
    let live: Vec<&Finding> = with_corpus
        .findings
        .iter()
        .filter(|f| f.rule_id == "ARCH-ONE-IMPORT-PATH-1")
        .collect();
    assert_eq!(live.len(), 1, "the split import path must fire: {live:?}");
    assert_eq!(
        corpus
            .iter()
            .find(|r| r.id.0 == "ARCH-ONE-IMPORT-PATH-1")
            .and_then(|r| r.max_severity.clone())
            .as_deref(),
        Some("low"),
        "the rule must declare the ceiling this test depends on"
    );
    assert_eq!(live[0].severity, "low", "the live finding must be bounded");
    assert_eq!(
        exported_xlsx_severity(live[0], &corpus),
        live[0].severity,
        "and the export must agree with it"
    );

    let without_corpus = scan(dir.path(), None).await;
    let raw: Vec<&Finding> = without_corpus
        .findings
        .iter()
        .filter(|f| f.rule_id == "ARCH-ONE-IMPORT-PATH-1")
        .collect();
    assert_eq!(raw.len(), 1);
    assert_eq!(
        raw[0].severity, "medium",
        "control: with no corpus there is no authored bound, so the checker's severity stands"
    );
}
