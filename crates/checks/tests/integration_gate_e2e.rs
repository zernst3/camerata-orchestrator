// Integration tests unwrap freely on setup I/O (test fixtures); the workspace
// convention is a file-level allow for `crates/*/tests/` (see root Cargo.toml).
#![allow(clippy::unwrap_used)]

//! End-to-end test for the cross-agent integration gate (GAP-6).
//!
//! Assembles two real worktrees on disk — a PRODUCER (an API repo) and a CONSUMER
//! (a UI repo) — and drives them through the full [`camerata_checks::run_gate`]
//! pipeline: language detection → per-stack extractor → generic reconciliation →
//! waiver + review-tier split → verdict. This is the assembled-tree path the server
//! runs before push/PR, exercised with no mocks.
//!
//! It proves the three e2e obligations from the GAP-6 spec:
//! 1. a MATCHING producer/consumer pair PASSES;
//! 2. a DRIFTING pair BOUNCES (to the responsible consumer agent);
//! 3. a stack with NO extractor for a seam is reported REVIEW-TIER, never green.

use std::path::Path;

use camerata_checks::{run_gate, GateRepo, GateWaiver};

/// Materialize a repo worktree in `dir` with a `package.json` (so the language
/// detector picks the JS extractors) plus the given source files.
fn scaffold_js_repo(dir: &Path, files: &[(&str, &str)]) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("package.json"), "{\"name\":\"e2e\",\"version\":\"1.0.0\"}\n").unwrap();
    for (name, content) in files {
        let p = dir.join(name);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(p, content).unwrap();
    }
}

#[test]
fn e2e_matching_producer_consumer_pair_passes() {
    let root = tempfile::tempdir().unwrap();
    let api = root.path().join("api");
    let ui = root.path().join("ui");

    // Producer serves the routes; consumer calls the SAME routes.
    scaffold_js_repo(
        &api,
        &[(
            "src/routes.js",
            "app.get('/members/:id', show)\napp.post('/members/export', exportCsv)\n",
        )],
    );
    scaffold_js_repo(
        &ui,
        &[(
            "src/api-client.js",
            "await axios.get('/members/42')\nawait axios.post('/members/export', body)\n",
        )],
    );

    let verdict = run_gate(
        &[
            GateRepo { repo: "acme/api".into(), dir: api },
            GateRepo { repo: "acme/ui".into(), dir: ui },
        ],
        &["INTEGRATION-API-CONTRACT-1".into()],
        &[],
    );

    assert!(
        verdict.passed(),
        "matching producer/consumer pair must pass; failures: {:?}",
        verdict.failures
    );
    assert!(verdict.review.is_empty(), "no review-tier seam expected: {:?}", verdict.review);
}

#[test]
fn e2e_drifting_pair_bounces_to_consumer() {
    let root = tempfile::tempdir().unwrap();
    let api = root.path().join("api");
    let ui = root.path().join("ui");

    // Producer serves POST /members/export; consumer calls POST /members/csv (drift).
    scaffold_js_repo(&api, &[("src/routes.js", "app.post('/members/export', exportCsv)\n")]);
    scaffold_js_repo(&ui, &[("src/client.js", "await axios.post('/members/csv', body)\n")]);

    let verdict = run_gate(
        &[
            GateRepo { repo: "acme/api".into(), dir: api },
            GateRepo { repo: "acme/ui".into(), dir: ui },
        ],
        &["INTEGRATION-API-CONTRACT-1".into()],
        &[],
    );

    assert!(!verdict.passed(), "drifting pair must fail");
    let targets = verdict.bounce_targets();
    assert!(
        targets.contains_key("acme/ui"),
        "the consumer (ui) is the bounce target: {targets:?}"
    );
    // The delta names the offending route.
    assert!(
        verdict.failures.iter().any(|f| f.detail.contains("/members/csv")),
        "delta must name the offending route: {:?}",
        verdict.failures
    );
}

#[test]
fn e2e_stack_with_no_extractor_is_review_tier_not_green() {
    let root = tempfile::tempdir().unwrap();
    let mystery = root.path().join("mystery");
    std::fs::create_dir_all(&mystery).unwrap();
    // No recognized manifest → no extractor for any seam.
    std::fs::write(mystery.join("main.cobol"), "DISPLAY 'HELLO'.\n").unwrap();

    let verdict = run_gate(
        &[GateRepo { repo: "acme/mystery".into(), dir: mystery }],
        &["INTEGRATION-API-CONTRACT-1".into()],
        &[],
    );

    // Honest: no mechanical failure, but the seam is REVIEW-TIER (human QA), not a pass.
    assert!(verdict.failures.is_empty(), "no mechanical failure on an unknown stack");
    assert!(
        verdict.review.iter().any(|r| r.repo == "acme/mystery"),
        "the uncovered stack must be reported review-tier: {:?}",
        verdict.review
    );
}

#[test]
fn e2e_auth_seam_per_seam_firing_and_waiver() {
    let root = tempfile::tempdir().unwrap();
    let api = root.path().join("api");
    let ui = root.path().join("ui");

    // One gated affordance on an UNGUARDED endpoint (fails), and one public call the
    // UI does NOT gate (out of scope — must NOT be flagged). Intra-project mix.
    scaffold_js_repo(
        &api,
        &[(
            "src/routes.js",
            "app.post('/members/:id/ban', banHandler)\napp.get('/health', healthHandler)\n",
        )],
    );
    scaffold_js_repo(
        &ui,
        &[(
            "src/ui.js",
            "if (org._can.ban) await axios.post('/members/7/ban') // camerata:ui-gated\n\
             await axios.get('/health')\n",
        )],
    );

    let repos = vec![
        GateRepo { repo: "acme/api".into(), dir: api },
        GateRepo { repo: "acme/ui".into(), dir: ui },
    ];

    // Without a waiver: the gated /ban affordance on an unguarded endpoint FAILS; the
    // ungated /health call is NOT flagged (per-seam firing).
    let no_waiver = run_gate(&repos, &["INTEGRATION-AUTH-SEAM-1".into()], &[]);
    assert!(!no_waiver.passed(), "gated affordance without a guarded endpoint must fail");
    assert_eq!(no_waiver.failures.len(), 1, "only the gated seam fires: {:?}", no_waiver.failures);
    assert!(
        no_waiver.failures[0].artifact.contains("/members/{}/ban"),
        "the ban endpoint is the finding, not health: {:?}",
        no_waiver.failures
    );

    // With an explicit per-endpoint waiver: the intentional-public /ban clears.
    let waiver = GateWaiver {
        rule_id: "INTEGRATION-AUTH-SEAM-1".into(),
        artifact: "endpoint POST /members/{}/ban".into(),
        reason: Some("intentionally public in this deployment".into()),
    };
    let waived = run_gate(&repos, &["INTEGRATION-AUTH-SEAM-1".into()], &[waiver]);
    assert!(waived.passed(), "explicit waiver clears the finding: {:?}", waived.failures);
    assert_eq!(waived.waived.len(), 1, "the waived finding is in the audit trail");
}
