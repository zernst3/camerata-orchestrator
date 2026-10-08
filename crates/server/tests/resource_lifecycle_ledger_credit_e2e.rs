//! END-TO-END: `ARCH-RESOURCE-LIFECYCLE-1` (`crates/checks/src/resource_lifecycle_checker.rs`)
//! must be credited directly at `RuleTier::Architectural` by the pipeline-integrity ledger
//! (`camerata_server::scan_ledger::ScanLedger`), driven through the REAL production entry
//! point (`onboard::audit_repos`), mirroring `pipeline_integrity_ledger_e2e.rs`'s pattern.
//!
//! The defect this closes: `ResourceLifecycleChecker` opts into `ArchChecker::
//! advisory_coexisting` (it is ALWAYS LLM-advisory-eligible by the rule's own design — not
//! because config is missing, see that checker's module doc) so its rule id is deliberately
//! excluded from `checker_rule_ids_for_repo` (the LLM-prompt-subtraction set). The ledger used
//! to reuse THAT set as its own "did a detector run" signal, which misclassified this
//! unconditionally-real, always-executing checker as having "no wired detector" — even though
//! `camerata_checks::arch_checker::all_checkers()` plainly registers it and it genuinely runs,
//! armed + applicable, on every scan. The fix is `ledger_detector_rule_ids_for_repo`, a
//! sibling set scoped to the ledger's actual question ("did a real checker run for this
//! repo"), which does not exclude `advisory_coexisting` checkers.
//!
//! FIXTURE: `tests/fixtures/resource_lifecycle_ledger_repo/`:
//!   - `src/lib.rs` — clean, fires nothing.
//!   - `src/spawn.rs` — a planted `tokio::process::Command` spawn with no `kill_on_drop(true)`.
//!
//! ZERO API SPEND: `run_ai_review: false` throughout — this rule's deterministic credit must
//! not depend on the model running at all.

// `allow-unwrap-in-tests` (clippy.toml) exempts `#[test]`/`#[tokio::test]` fn bodies directly,
// but this file's fixture-staging helpers (`copy_dir_recursive`, the git plumbing in
// `stage_fixture_as_git_repo`) are plain fns clippy does not recognize as test-only — see the
// workspace `unwrap_used = "deny"` lint's doc comment in the root Cargo.toml.
#![allow(clippy::unwrap_used)]

use std::path::Path;

use camerata_server::ai_audit::ScanMode;
use camerata_server::onboard::{self, SelectedRule};
use camerata_server::scan_ledger::RuleTier;

const RULE_RESOURCE_LIFECYCLE: &str = "ARCH-RESOURCE-LIFECYCLE-1";

/// Mirrors every sibling `*_e2e.rs`'s own fixture-staging helper.
fn stage_fixture_as_git_repo(dest: &Path) {
    let fixture_root =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/resource_lifecycle_ledger_repo");
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
    g(&[
        "config",
        "user.email",
        "resource-lifecycle-ledger-e2e@camerata.local",
    ]);
    g(&[
        "config",
        "user.name",
        "Camerata Resource-Lifecycle-Ledger E2E Test",
    ]);
    g(&["add", "."]);
    g(&[
        "commit",
        "-q",
        "-m",
        "e2e fixture: one clean file + one planted unprotected spawn",
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

async fn run_fixture_scan(
    repo_dir: &Path,
    repo_spec: &str,
    corpus: &camerata_rules::RuleSet,
) -> onboard::ScanReport {
    let sources = vec![(repo_spec.to_string(), repo_dir.to_path_buf())];
    let selected = vec![SelectedRule {
        id: RULE_RESOURCE_LIFECYCLE.to_string(),
        directive: "spawned child processes carry a kill-on-drop disposition".to_string(),
        repos: Vec::new(),
    }];
    let (report, _manifest) = onboard::audit_repos(
        &sources,
        &selected,
        Vec::new(), // extra_notes
        None,       // model — unused, run_ai_review is false
        None,       // calibration_model — unused
        ScanMode::Sequential,
        false,                                        // thorough — unused (no AI review)
        None,                                         // feedback
        None,                                         // job
        None,                                         // incremental_prior — full scan
        false,                                        // deep
        false,                                        // soc2_enabled
        false, // run_ai_review — zero API spend; this rule's credit must not depend on it
        true,  // run_deterministic — the architectural engine runs
        None,  // usage ledger
        camerata_server::llm::BackendResolution::Api, // backend gate: not under test here
        Some(corpus),
        &std::collections::HashMap::new(), // chosen_options
    )
    .await;
    report
}

#[tokio::test]
async fn resource_lifecycle_rule_is_credited_as_architectural_never_no_wired_detector() {
    let corpus_path = camerata_rules::corpus_path();
    let (corpus, corpus_errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
    assert!(
        corpus_errors.is_empty(),
        "the bundled rule corpus must load cleanly: {corpus_errors:?}"
    );

    let repo_spec = "e2e/resource-lifecycle-ledger-fixture".to_string();
    let repo_dir = tempfile::tempdir().expect("create temp dir for the fixture git repo");
    stage_fixture_as_git_repo(repo_dir.path());

    let report = run_fixture_scan(repo_dir.path(), &repo_spec, &corpus).await;
    assert!(
        !report.gated,
        "a local-dir scan is never gated on a GitHub token"
    );

    // ═══ The planted violation actually fired (sanity). ═══
    assert!(
        report
            .findings
            .iter()
            .any(|f| f.rule_id == RULE_RESOURCE_LIFECYCLE),
        "the planted unprotected spawn must fire {RULE_RESOURCE_LIFECYCLE}: {:?}",
        report.findings
    );

    // ═══ Ledger: this rule ran, is credited at the Architectural tier, fired, and carries NO ═══
    // config-gated disclosure — there is nothing to disclose, the checker genuinely ran. ═══
    let entry = report
        .ledger
        .rule(RULE_RESOURCE_LIFECYCLE)
        .unwrap_or_else(|| panic!("{RULE_RESOURCE_LIFECYCLE} must be recorded in the ledger"));
    assert!(entry.ran, "the checker genuinely ran this scan");
    assert_eq!(
        entry.tier,
        RuleTier::Architectural,
        "a real, unconditionally-executing checker must be credited at the Architectural tier, \
         never left to the semantic phase to (re)classify"
    );
    assert!(
        entry.findings_emitted >= 1,
        "the real planted finding must be counted: {entry:?}"
    );
    assert!(
        entry.coverage_note.is_none(),
        "nothing to disclose — the checker genuinely ran deterministically for this repo: \
         {entry:?}"
    );
    if let Some(reason) = &entry.skip_reason {
        assert!(
            !reason.contains(camerata_server::scan_ledger::NO_WIRED_DETECTOR_REASON),
            "must never be classified as having no wired detector: {reason}"
        );
    }

    let fired = report.ledger.fired_rule_ids();
    let healthy = report.ledger.healthy_rule_ids();
    let excluded = report.ledger.excluded_rules();
    assert!(fired.contains(RULE_RESOURCE_LIFECYCLE), "{fired:?}");
    assert!(
        !healthy.contains(&RULE_RESOURCE_LIFECYCLE.to_string()),
        "{healthy:?}"
    );
    assert!(
        !excluded
            .iter()
            .any(|(id, _)| *id == RULE_RESOURCE_LIFECYCLE),
        "a rule that genuinely ran and fired must never appear as excluded/not-run: {excluded:?}"
    );

    assert_eq!(
        report.ledger.total_unaccounted(),
        0,
        "every pipeline stage must reconcile exactly: {:#?}",
        report.ledger.stages()
    );
}
