//! Pass-1 W5 — E2 / A5.2 severity ceilings, proved against the REAL shipped corpus.
//!
//! > **E2 AGREED** — rules may declare a maximum severity; no report may exceed it.
//! >
//! > — `docs/plans/2026-10-09_audit-findings-tracker.md`
//!
//! # Why this file exists separately from `report_export.rs`'s own `req_e2_*` tests
//!
//! E2's mechanism shipped complete and fully unit-tested in a prior workstream:
//! `Rule::max_severity`, `report_export::clamp_to_rule_max_severity`, and four passing
//! `req_e2_*` tests. Every one of those tests builds its own SYNTHETIC single-rule corpus
//! (`ruleset_with_max_severity("CEILING-1", "high")`) and proves the clamp function behaves.
//!
//! None of them could fail if the real corpus declared no ceilings at all — and it didn't.
//! At commit `2799284`, `grep -rl '^max_severity' crates/rules/principles/` returned **0 of
//! 417**. The requirement was inert while its tests were green, which is the precise failure
//! mode the tracker's test discipline names:
//!
//! > "I don't want code that confidently thinks it's doing what is written here in the
//! > product requirements, but actually isn't."
//!
//! So these tests take the ceiling from the **real, shipped corpus TOMLs** — never from a
//! fixture — and drive a real report export through `build_report_json`. They are the tests
//! that would have failed yesterday.
//!
//! # No fixture repos, no benchmark repos
//!
//! Nothing here reads a fixture or benchmark repository. The only external input is the
//! bundled rule corpus (`camerata_rules::DEFAULT_CORPUS_PATH`), and the ceilings asserted are
//! the ones transcribed from each rule's own authored prose (finding C8). A rule id appears
//! below only because its TOML states its own bound in prose.

use std::collections::HashMap;

use camerata_server::onboard::{AuditedRef, Finding, LanguageVolume, ScanProvenance, ScanReport};
use camerata_server::report_export::{build_report_json, ReportOptions};

/// Load the real shipped corpus, or `None` when it isn't checked out (mirrors the
/// `path.exists()` skip guard every corpus-reading test in `camerata_rules` uses).
async fn real_corpus() -> Option<camerata_rules::RuleSet> {
    let path = std::path::Path::new(camerata_rules::DEFAULT_CORPUS_PATH);
    if !path.exists() {
        return None;
    }
    Some(
        camerata_rules::load_corpus(path)
            .await
            .expect("the real corpus loads"),
    )
}

fn finding(rule_id: &str, path: &str, line: usize, severity: &str) -> Finding {
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

fn report_with(findings: Vec<Finding>, audited_rule_ids: Vec<&str>) -> ScanReport {
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
            audited_rule_ids: audited_rule_ids.into_iter().map(String::from).collect(),
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

/// Every severity a one-finding report actually EXPORTS for `rule_id`, plus the report's
/// total critical count. Taken from the real export path (`build_report_json`) rather than by
/// calling the clamp helper directly — "no REPORT may exceed it" is a statement about the
/// artifact, not about a function.
///
/// Both top-level row sections are scanned, not just `curated_findings`. That is load-bearing:
/// a group whose every site is informational or held renders under `held_for_review_findings`
/// instead, and the `ARCH-OBSERVABILITY-*` class goes there unconditionally (`is_informational`
/// routes it out of every action bucket by rule identity). A helper that read
/// `curated_findings[0]` alone would panic on exactly the rules whose ceiling is most
/// emphatically authored — so this returns every site it finds, wherever it landed.
fn exported_severities(
    rule_id: &str,
    proposed: &str,
    corpus: Option<&camerata_rules::RuleSet>,
) -> (Vec<String>, usize) {
    let report = report_with(
        vec![finding(rule_id, "src/a.ts", 1, proposed)],
        vec![rule_id],
    );
    let json = build_report_json(
        &report.seal(),
        &HashMap::new(),
        corpus,
        &ReportOptions::default(),
    );
    let severities: Vec<String> = json
        .curated_findings
        .iter()
        .chain(json.held_for_review_findings.iter())
        .flat_map(|g| g.sites.iter())
        .map(|s| s.severity.clone())
        .collect();
    let criticals: usize = json.scorecard.rows.iter().map(|r| r.critical).sum();
    (severities, criticals)
}

/// [`exported_severities`] for the single-site case, asserting the row was exported exactly
/// once so a silently-dropped row can never read as a passing clamp.
fn exported_severity(
    rule_id: &str,
    proposed: &str,
    corpus: Option<&camerata_rules::RuleSet>,
) -> (String, usize) {
    let (severities, criticals) = exported_severities(rule_id, proposed, corpus);
    assert_eq!(
        severities.len(),
        1,
        "{rule_id} must export exactly one site; a dropped row is not a clamp"
    );
    (severities[0].clone(), criticals)
}

/// req_e2: a ceiling **authored in a real corpus rule** clamps a real report that would
/// otherwise exceed it. `SUPABASE-STORAGE-PUBLIC-BUCKET-1`'s own prose reads
/// "`needs_review`, not an automatic critical" (twice), and its rejected option is literally
/// "flag only suspicious-named buckets, at critical severity" — so a critical can never be
/// the right export for this rule, no matter what the model or the severity floor proposed.
///
/// FAILS BEFORE THE FIX: on commit `2799284` this rule's TOML carried no `max_severity`
/// (0 of 417 rules did), `clamp_to_rule_max_severity` took its `None` early-return, and the
/// export kept `critical` — so `assert_eq!(severity, "high")` failed with `left: "critical"`.
/// Verified by reverting the 18 corpus TOMLs to `2799284` and re-running.
#[tokio::test]
async fn req_e2_an_authored_ceiling_clamps_a_real_rule() {
    let Some(corpus) = real_corpus().await else {
        return;
    };
    const RULE: &str = "SUPABASE-STORAGE-PUBLIC-BUCKET-1";
    assert_eq!(
        corpus
            .iter()
            .find(|r| r.id.0 == RULE)
            .and_then(|r| r.max_severity.clone())
            .as_deref(),
        Some("high"),
        "{RULE} must declare the ceiling its own prose authors; without it this test is vacuous"
    );

    let (severity, criticals) = exported_severity(RULE, "critical", Some(&corpus));
    assert_eq!(
        severity, "high",
        "{RULE} authors \"not an automatic critical\"; no report may export critical for it"
    );
    assert_eq!(
        criticals, 0,
        "the clamp must reach the scorecard too, not just the row's own severity field"
    );
}

/// req_e2 (attribution control): the SAME critical finding under a real corpus rule that
/// authors NO ceiling still exports `critical`. Without this, the test above would also pass
/// if some unrelated part of the export pipeline happened to cap everything — it proves the
/// clamp is attributable to the authored ceiling specifically.
///
/// `SUPABASE-RLS-DISABLED-NOT-RESTORED-1` is the deliberate counterpart: its prose says the
/// finding "is always SEVERITY_CRITICAL, because a deliberate-but-unrestored disable" is its
/// own defect, so it is exactly the rule that must NOT have been swept into a ceiling.
#[tokio::test]
async fn req_e2_a_rule_that_authored_no_ceiling_still_exports_critical() {
    let Some(corpus) = real_corpus().await else {
        return;
    };
    const RULE: &str = "SUPABASE-RLS-DISABLED-NOT-RESTORED-1";
    assert!(
        corpus
            .iter()
            .find(|r| r.id.0 == RULE)
            .expect("the control rule exists in the corpus")
            .max_severity
            .is_none(),
        "{RULE}'s prose authors \"always SEVERITY_CRITICAL\" — it must carry NO ceiling. A \
         ceiling here would be the over-reach failure: silently capping a real finding."
    );

    let (severity, criticals) = exported_severity(RULE, "critical", Some(&corpus));
    assert_eq!(
        severity, "critical",
        "a ceiling-free rule must be untouched; the E2 clamp is per-rule, never global"
    );
    assert_eq!(criticals, 1, "and it must still count as a critical");
}

/// req_e2: the owner-directive `low` cap, from the real corpus. Three observability rules
/// state in their own prose that the pipeline "force[s] this rule's findings to severity Low
/// ... regardless of the model's proposed severity", and
/// `ARCH-OBSERVABILITY-SILENT-FAILURE-1` adds "deliberately CAPPED LOW (owner directive)".
/// That cap previously existed ONLY as a hard-coded rule-id class inside `ai_audit`/
/// `report_export`; E2 is the structured declaration of the same intent, and this proves the
/// declared form holds at the export boundary for a critical proposal.
///
/// FAILS BEFORE THE FIX: all three TOMLs carried no `max_severity` at `2799284`.
#[tokio::test]
async fn req_e2_the_owner_directive_low_cap_holds_for_every_observability_rule() {
    let Some(corpus) = real_corpus().await else {
        return;
    };
    for rule in [
        "ARCH-OBSERVABILITY-SILENT-FAILURE-1",
        "ARCH-OBSERVABILITY-LOG-CONSISTENCY-1",
        "ARCH-OBSERVABILITY-CORRELATION-ID-1",
    ] {
        assert_eq!(
            corpus
                .iter()
                .find(|r| r.id.0 == rule)
                .and_then(|r| r.max_severity.clone())
                .as_deref(),
            Some("low"),
            "{rule} authors a low cap in its own prose"
        );
        let (severity, criticals) = exported_severity(rule, "critical", Some(&corpus));
        assert_ne!(
            severity, "critical",
            "{rule} is capped low by owner directive; a critical export contradicts it"
        );
        assert_eq!(severity, "low", "{rule}'s authored ceiling is exactly low");
        assert_eq!(criticals, 0, "{rule} must contribute no critical count");
    }
}

/// req_e2: a `medium` ceiling clamps a `high` proposal — the middle of the vocabulary, so the
/// clamp is not accidentally a two-value special case. `SUPABASE-RLS-NO-POLICY-1`'s prose:
/// "Medium severity — this is an availability/architecture smell, not a direct exposure, so
/// it is a lower severity than the missing-RLS and policy-disabled findings."
#[tokio::test]
async fn req_e2_a_medium_ceiling_clamps_a_high_proposal() {
    let Some(corpus) = real_corpus().await else {
        return;
    };
    const RULE: &str = "SUPABASE-RLS-NO-POLICY-1";
    let (severity, _) = exported_severity(RULE, "high", Some(&corpus));
    assert_eq!(
        severity, "medium",
        "{RULE} authors a medium bound relative to its sibling RLS findings"
    );
    // And the ceiling is a ceiling, never a floor: a proposal already below it is untouched.
    let (low, _) = exported_severity(RULE, "low", Some(&corpus));
    assert_eq!(
        low, "low",
        "{RULE}'s ceiling must not RAISE a finding that already ranks below it"
    );
}

/// req_e2: every ceiling the real corpus declares is actually HONOURED at the export
/// boundary — stated as a property over whatever the corpus contains, so a ceiling added
/// later is covered without editing this file. For each rule declaring a ceiling, propose
/// `critical` and assert the export never ranks above the declared value.
///
/// FAILS BEFORE THE FIX: the loop body never executed at `2799284` (no rule declared a
/// ceiling), so the test was vacuous; the explicit non-empty assertion below is what turns
/// that vacuity into a failure.
#[tokio::test]
async fn req_e2_no_report_exceeds_any_ceiling_the_corpus_declares() {
    let Some(corpus) = real_corpus().await else {
        return;
    };
    fn rank(sev: &str) -> u8 {
        match sev {
            "critical" => 0,
            "high" => 1,
            "medium" => 2,
            _ => 3,
        }
    }
    let declaring: Vec<(String, String)> = corpus
        .iter()
        .filter_map(|r| r.max_severity.clone().map(|c| (r.id.0.clone(), c)))
        .collect();
    assert!(
        !declaring.is_empty(),
        "E2 is INERT: no corpus rule declares a ceiling, so this property is vacuous"
    );
    for (rule_id, ceiling) in &declaring {
        let (severities, _) = exported_severities(rule_id, "critical", Some(&corpus));
        assert!(
            !severities.is_empty(),
            "{rule_id} exported no row at all, so this iteration proves nothing"
        );
        for severity in &severities {
            assert!(
                rank(severity) >= rank(ceiling),
                "{rule_id} declares max_severity = {ceiling:?} but the report exported \
                 {severity:?}, which ranks above it"
            );
        }
    }
}
