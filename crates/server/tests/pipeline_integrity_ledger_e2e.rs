//! END-TO-END: the W1 pipeline-integrity ledger (`camerata_server::scan_ledger::ScanLedger`),
//! driven through the REAL production entry points (`onboard::audit_repos` ->
//! `report_export::build_report_json`), mirroring the existing `*_executor_e2e.rs` two-suite
//! pattern (e.g. `query_grammar_injection_executor_e2e.rs`).
//!
//! Two defects this proves closed, end to end (not just in `scan_ledger`'s own unit tests or
//! `report_export`'s synthetic-ledger invariant tests):
//!
//! 1. A rule declaring `enforcement = "mechanical"` with NO wired detector (the corpus's own
//!    `RUBY-FROZEN-STRING-LITERAL-1` — a real, pre-existing gap `mechanical_gate` tracks,
//!    deliberately reused here rather than inventing a fake corpus entry) is selected for
//!    this scan and must land in "excluded from this audit" with the real reason, NEVER in
//!    "What's healthy" just because it was selected. W4 changed WHAT that real reason is:
//!    this scan runs with `run_ai_review: false` (zero API spend), and as of W4 this rule's
//!    ONLY possible route to evaluation is the semantic/AI pass (it has no deterministic
//!    detector) — so with AI review off, it is excluded for the ordinary, honestly-scoped
//!    reason "AI/semantic review not requested this run", the SAME reason any other
//!    AI-tier-only rule gets when AI review is off, rather than a "no wired detector"
//!    pipeline-integrity alarm (that alarm is now reserved for a rule that SHOULD have
//!    reached the semantic pass but somehow didn't — see `semantic_tier_covers_mechanical_no_detector_rule_e2e.rs`
//!    for the companion test proving the happy path: the SAME rule, with AI review ON and a
//!    stubbed model, genuinely gets evaluated and reaches `ran = true` via `RuleTier::Semantic`).
//! 2. Every pipeline stage this scan touches (the floor, the architectural engine, the
//!    cross-family merge) reconciles with ZERO unaccounted rows, and no rule id appears in
//!    both the fired set and the clean/excluded lists.
//!
//! FIXTURE: `tests/fixtures/pipeline_integrity_ledger_repo/`:
//!   - `src/lib.rs` — clean, fires nothing (the healthy case for `SEC-NO-HARDCODED-SECRETS-1`).
//!   - `src/db.rs` — a planted `SEC-NO-RAW-SQL-CONCAT-1` violation (the fired case).
//!
//! ZERO API SPEND: `run_ai_review: false` throughout.

use std::collections::HashMap;
use std::path::Path;

use camerata_server::ai_audit::ScanMode;
use camerata_server::onboard::{self, SelectedRule};
use camerata_server::report_export::{self, DispositionWire, ReportOptions};

const RULE_SECRETS: &str = "SEC-NO-HARDCODED-SECRETS-1";
const RULE_SQL_CONCAT: &str = "SEC-NO-RAW-SQL-CONCAT-1";
/// A real, pre-existing corpus gap `crate::mechanical_gate::mechanical_rules_missing_detector`
/// still finds: declares `enforcement = "mechanical"` but has no registered `ArchChecker`,
/// gateway rule arm, bundled Semgrep rule, or scan-preview linter source. Reused here (not a
/// fake/synthetic id) so this E2E test proves the ledger correctly tracks a REAL
/// no-deterministic-detector rule through the real pipeline — NOT a phantom since W4 (it has a
/// second, real route: the semantic/AI pass), just one this particular run (AI review off)
/// never takes.
const RULE_NO_DETECTOR: &str = "RUBY-FROZEN-STRING-LITERAL-1";

/// Mirrors every sibling `*_executor_e2e.rs`'s own fixture-staging helper (each e2e file owns
/// its own copy per this repo's existing convention).
fn stage_fixture_as_git_repo(dest: &Path) {
    let fixture_root =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pipeline_integrity_ledger_repo");
    copy_dir_recursive(&fixture_root, dest);

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
    g(&["config", "user.email", "pipeline-integrity-ledger-e2e@camerata.local"]);
    g(&["config", "user.name", "Camerata Pipeline-Integrity-Ledger E2E Test"]);
    g(&["add", "."]);
    g(&[
        "commit",
        "-q",
        "-m",
        "e2e fixture: one clean file + one planted raw-sql-concat violation",
    ]);
}

fn copy_dir_recursive(src: &Path, dest: &Path) {
    std::fs::create_dir_all(dest).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let ty = entry.file_type().unwrap();
        let from = entry.path();
        let to = dest.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_recursive(&from, &to);
        } else {
            std::fs::copy(&from, &to)
                .unwrap_or_else(|e| panic!("failed to copy fixture file {from:?} -> {to:?}: {e}"));
        }
    }
}

fn selected_rules() -> Vec<SelectedRule> {
    vec![
        SelectedRule {
            id: RULE_SECRETS.to_string(),
            directive: "no hardcoded secrets".to_string(),
            repos: Vec::new(),
        },
        SelectedRule {
            id: RULE_SQL_CONCAT.to_string(),
            directive: "no raw SQL built by string concatenation".to_string(),
            repos: Vec::new(),
        },
        SelectedRule {
            id: RULE_NO_DETECTOR.to_string(),
            directive: "ruby files declare frozen_string_literal: true".to_string(),
            repos: Vec::new(),
        },
    ]
}

async fn run_fixture_scan(
    repo_dir: &Path,
    repo_spec: &str,
    corpus: &camerata_rules::RuleSet,
) -> onboard::ScanReport {
    let sources = vec![(repo_spec.to_string(), repo_dir.to_path_buf())];
    let selected = selected_rules();
    let (report, _manifest) = onboard::audit_repos(
        &sources,
        &selected,
        Vec::new(), // extra_notes
        None,       // model — unused, run_ai_review is false
        None,       // calibration_model — unused
        ScanMode::Sequential,
        false, // thorough — unused (no AI review)
        None,  // feedback
        None,  // job
        None,  // incremental_prior — full scan
        false, // deep
        false, // soc2_enabled
        false, // run_ai_review — zero API spend
        true,  // run_deterministic — the floor AND the architectural engine both run
        None,  // usage ledger
        camerata_server::llm::BackendResolution::Api, // backend gate: not under test here
        Some(corpus),
        &std::collections::HashMap::new(), // chosen_options
    )
    .await;
    report
}

#[tokio::test]
async fn ledger_reconciles_and_the_no_detector_rule_is_excluded_never_healthy() {
    let corpus_path = camerata_rules::corpus_path();
    let (corpus, corpus_errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
    assert!(
        corpus_errors.is_empty(),
        "the bundled rule corpus must load cleanly: {corpus_errors:?}"
    );

    let repo_spec = "e2e/pipeline-integrity-ledger-fixture".to_string();
    let repo_dir = tempfile::tempdir().expect("create temp dir for the fixture git repo");
    stage_fixture_as_git_repo(repo_dir.path());

    let report = run_fixture_scan(repo_dir.path(), &repo_spec, &corpus).await;
    assert!(!report.gated, "a local-dir scan is never gated on a GitHub token");

    // ═══ The planted violation actually fired (sanity: the ledger assertions below are ═══
    // meaningless if the floor didn't really run). ═══
    assert!(
        report.findings.iter().any(|f| f.rule_id == RULE_SQL_CONCAT),
        "the planted SEC-NO-RAW-SQL-CONCAT-1 violation must be present: {:?}",
        report.findings
    );
    assert!(
        !report.findings.iter().any(|f| f.rule_id == RULE_SECRETS),
        "the clean file must not fire SEC-NO-HARDCODED-SECRETS-1: {:?}",
        report.findings
    );

    // ═══ Ledger: per-rule facts, straight off `report.ledger` (the real scan-time ledger). ═══
    let healthy = report.ledger.healthy_rule_ids();
    let excluded = report.ledger.excluded_rules();
    let fired = report.ledger.fired_rule_ids();

    assert!(
        healthy.contains(&RULE_SECRETS.to_string()),
        "a rule that genuinely ran with zero findings must be healthy: {healthy:?}"
    );
    assert!(
        fired.contains(RULE_SQL_CONCAT),
        "a rule that fired must be in the ledger's fired set: {fired:?}"
    );
    assert!(
        !healthy.contains(&RULE_SQL_CONCAT.to_string()),
        "a fired rule must never be healthy: {healthy:?}"
    );
    let no_detector_entry = excluded
        .iter()
        .find(|(id, _)| *id == RULE_NO_DETECTOR)
        .unwrap_or_else(|| panic!("{RULE_NO_DETECTOR} must appear in excluded_rules: {excluded:?}"));
    // W4: this run has `run_ai_review: false`, and `RUBY-FROZEN-STRING-LITERAL-1` has no
    // deterministic detector — its only possible route (the semantic pass) was never taken
    // this run, which is an ORDINARY, honestly-scoped skip, not a pipeline-integrity alarm.
    assert!(
        no_detector_entry
            .1
            .contains("AI/semantic review not requested"),
        "the excluded entry must carry the REAL reason, not a placeholder: {:?}",
        no_detector_entry.1
    );
    assert!(
        !healthy.contains(&RULE_NO_DETECTOR.to_string()),
        "the no-detector rule must never be healthy just because it was selected: {healthy:?}"
    );

    // No rule id may EVER appear in more than one of {healthy, excluded, fired}.
    let healthy_set: std::collections::HashSet<&str> = healthy.iter().map(String::as_str).collect();
    let excluded_set: std::collections::HashSet<&str> =
        excluded.iter().map(|(id, _)| *id).collect();
    assert!(
        healthy_set.is_disjoint(&excluded_set),
        "healthy and excluded must never overlap: healthy={healthy_set:?} excluded={excluded_set:?}"
    );
    let fired_set: std::collections::HashSet<&str> = fired.iter().map(String::as_str).collect();
    assert!(
        healthy_set.is_disjoint(&fired_set),
        "healthy and fired must never overlap: healthy={healthy_set:?} fired={fired_set:?}"
    );

    // ═══ Every stage this scan touched reconciled with ZERO unaccounted rows. ═══
    assert_eq!(
        report.ledger.total_unaccounted(),
        0,
        "every pipeline stage must reconcile exactly: {:#?}",
        report.ledger.stages()
    );

    // ═══ The SAME facts, through the real report-build pipeline. ═══
    let dispositions: HashMap<String, DispositionWire> = HashMap::new();
    let opts = ReportOptions {
        client_name: "Acme Corp".to_string(),
        project_title: "Pipeline-Integrity-Ledger E2E Fixture Audit".to_string(),
        prepared_by: "Camerata (automated e2e test)".to_string(),
        executive_summary_override: None,
        ..Default::default()
    };
    let json = report_export::build_report_json(&report, &dispositions, Some(&corpus), &opts);

    let healthy_ids: std::collections::HashSet<&str> =
        json.whats_healthy.rules.iter().map(|r| r.rule_id.as_str()).collect();
    let excluded_ids: std::collections::HashSet<&str> =
        json.rules_not_run.iter().map(|r| r.rule_id.as_str()).collect();
    assert!(healthy_ids.contains(RULE_SECRETS));
    assert!(!healthy_ids.contains(RULE_SQL_CONCAT));
    assert!(excluded_ids.contains(RULE_NO_DETECTOR));
    assert!(!healthy_ids.contains(RULE_NO_DETECTOR));
    assert!(healthy_ids.is_disjoint(&excluded_ids));

    // A fully-reconciled scan must never carry a "pipeline integrity" disclosure.
    for note in json
        .methodology
        .failed_passes
        .iter()
        .chain(json.executive_summary.failed_passes.iter())
    {
        assert!(
            !note.to_ascii_lowercase().contains("pipeline integrity"),
            "a fully-reconciled scan must never disclose a pipeline-integrity gap: {note}"
        );
    }
    // W4: with AI review off, skipping the no-detector rule is an ORDINARY scope choice (its
    // only route was never attempted this run), not a pipeline-integrity defect — so it must
    // NOT ride the `rule_disclosure` alarm mechanism into methodology/executive-summary at all.
    // Contrast with `semantic_tier_covers_mechanical_no_detector_rule_e2e.rs`, where the SAME
    // rule, reviewed by a stubbed model, genuinely reaches `ran = true`.
    assert!(
        !json
            .methodology
            .failed_passes
            .iter()
            .any(|n| n.contains(RULE_NO_DETECTOR)),
        "an ordinary 'AI review not requested' skip must not produce a rule-level disclosure: {:?}",
        json.methodology.failed_passes
    );
}
