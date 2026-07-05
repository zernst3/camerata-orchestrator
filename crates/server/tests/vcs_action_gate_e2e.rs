//! Hermetic end-to-end regression net for the VCS-action gate (GAP-2).
//!
//! THE CLAIM under test: for every server-side commit / PR chokepoint, a configured
//! process rule actually BLOCKS non-compliant actions and ALLOWS compliant ones. Before
//! GAP-2 landed (2026-07-05), the gate logic existed in `camerata_checks::vcs_action` but
//! was only reachable through the manual bypass endpoint — no real commit or PR chokepoint
//! called it. A project could configure a conventional-commit rule and watch the server
//! generate non-compliant commits silently.
//!
//! Coverage:
//!   - `gap2_violating_commit_is_blocked`: a commit message that violates a configured
//!     process rule HARD-BLOCKS (`ChokeError::Blocked`); nothing is staged.
//!   - `gap2_compliant_commit_passes`: a commit message that satisfies all configured
//!     rules returns `Ok(())`.
//!   - `gap2_bypass_with_reason_allows_violating_commit`: the `gated_commit_or_bypass`
//!     entry point allows an auditable bypass for machine-generated commits that cannot
//!     satisfy the rule, and returns a non-empty summary naming the suppressed rule(s).
//!   - `gap2_bypass_without_reason_is_rejected`: an empty bypass reason is itself
//!     rejected (`ChokeError::BypassReasonRequired`), closing the "silent bypass" hole.
//!   - `gap2_pr_gate_blocks_and_passes`: the PR gate mirrors the commit gate at its own
//!     chokepoint.
//!   - `gap2_compliant_commit_consumes_no_bypass`: a compliant action passed through the
//!     `_or_bypass` path returns `Ok(None)` (no bypass record created or consumed).
//!
//! HERMETIC: NO real `git` spawn, NO network, NO model call. The gate is driven via the
//! public `camerata_server::vcs_choke` API over an in-memory `ProcessRuleConfig` that
//! mirrors what a real project would configure. This tests the SAME code paths the server
//! invokes at `POST /api/git/commit` and `POST /api/pr/open`.

#![allow(clippy::unwrap_used)]

use camerata_checks::vcs_action::{CommitDocConfig, ConventionalCommitConfig, ProcessRuleConfig};
use camerata_server::vcs_choke::{
    gated_commit, gated_commit_or_bypass, gated_pr, gated_pr_or_bypass, ChokeError,
};

// ════════════════════════════════════════════════════════════════════════════════════
// Shared fixtures
// ════════════════════════════════════════════════════════════════════════════════════

/// A config that enforces the conventional-commit shape only, with no story-id requirement.
/// Mirrors a minimal "clean commit messages" project configuration.
fn conventional_commit_only() -> ProcessRuleConfig {
    ProcessRuleConfig {
        commit_doc: CommitDocConfig {
            enabled: false,
            ..CommitDocConfig::default()
        },
        conventional_commit: ConventionalCommitConfig {
            enabled: true,
            ..ConventionalCommitConfig::default()
        },
        ..ProcessRuleConfig::default()
    }
}

/// A config that enforces both conventional-commit shape AND a story-id in the body.
/// Mirrors a "ticket-linked commits" project configuration (the common enterprise setup).
fn full_doc_config() -> ProcessRuleConfig {
    ProcessRuleConfig::default()
}

// ════════════════════════════════════════════════════════════════════════════════════
// GAP-2 commit gate
// ════════════════════════════════════════════════════════════════════════════════════

/// THE KEY GAP-2 REGRESSION: before this fix no commit chokepoint called the gate.
/// Now `gated_commit` hard-blocks any commit whose message violates a configured
/// process rule. A message like "just did some stuff" with no type-prefix is a
/// conventional-commit violation and must produce `ChokeError::Blocked`.
#[test]
fn gap2_violating_commit_is_blocked() {
    let cfg = conventional_commit_only();

    // A bare, unstructured commit message with no conventional-commit type prefix.
    let err = gated_commit(&cfg, "just did some stuff").expect_err(
        "GAP-2 regression: a violating commit MUST be hard-blocked by the gate; \
         before the fix it was silently allowed",
    );

    match err {
        ChokeError::Blocked(violations) => {
            assert!(
                violations
                    .iter()
                    .any(|v| v.rule_id == "PROCESS-CONVENTIONAL-COMMIT-1"),
                "the conventional-commit rule must fire and name itself in the violation \
                 detail so the caller can surface it to the user: {violations:?}"
            );
        }
        other => panic!("expected ChokeError::Blocked, got {other:?}"),
    }
}

/// A message that satisfies the conventional-commit shape passes the gate cleanly.
#[test]
fn gap2_compliant_commit_passes() {
    let cfg = conventional_commit_only();
    gated_commit(&cfg, "feat: add the export endpoint")
        .expect("a compliant commit message must pass the gate without error");
}

/// Verify both shape + story-id rules together. A conventional-commit subject with no
/// body is blocked by the commit-doc rule; adding a substantive body + story id passes.
#[test]
fn gap2_full_config_blocks_subject_only_and_passes_with_body_and_story_id() {
    let cfg = full_doc_config();

    // Subject-only commit: no body, no story id -> blocked.
    assert!(
        gated_commit(&cfg, "feat: add export").is_err(),
        "a subject-only commit must be blocked when the doc rule is enabled"
    );

    // Conventional subject + substantive body + story id -> passes.
    let ok_msg = "feat: add export\n\nImplements the CSV export flow end to end. Refs #42.";
    gated_commit(&cfg, ok_msg).expect("a fully-compliant commit must pass the gate");
}

// ════════════════════════════════════════════════════════════════════════════════════
// GAP-2 bypass path
// ════════════════════════════════════════════════════════════════════════════════════

/// Machine-generated commits (snapshot commits, merge commits) legitimately cannot
/// always satisfy the conventional-commit format. The `gated_commit_or_bypass` entry
/// point allows an AUDITABLE bypass: the action is allowed, and the returned record
/// names the suppressed rules so the evidence trail is complete.
#[test]
fn gap2_bypass_with_reason_allows_violating_commit() {
    let cfg = conventional_commit_only();

    let record = gated_commit_or_bypass(
        &cfg,
        "Merge branch 'main' into feature/foo",
        Some("machine-generated merge commit from the rebase pipeline"),
    )
    .expect("bypass with a non-empty reason must succeed even on a violating message");

    let summary = record.expect(
        "a bypass record must be produced when the action would otherwise have been blocked",
    );
    assert!(
        summary.contains("PROCESS-CONVENTIONAL-COMMIT-1"),
        "the bypass record must name the suppressed rule so it appears in the audit trail: \
         {summary}"
    );
}

/// An empty bypass reason is itself rejected. This closes the "silent bypass" hole:
/// callers cannot call `gated_commit_or_bypass` with `Some("")` to trivially avoid the
/// gate without providing a traceable justification.
#[test]
fn gap2_bypass_without_reason_is_rejected() {
    let cfg = conventional_commit_only();

    let err = gated_commit_or_bypass(&cfg, "bad subject with no type", Some(""))
        .expect_err("an empty bypass reason must be rejected");

    assert!(
        matches!(err, ChokeError::BypassReasonRequired),
        "the error must be BypassReasonRequired, not Blocked: {err:?}"
    );
}

/// When a commit message is already compliant, passing it through the `_or_bypass`
/// path returns `Ok(None)`: no bypass record is created and no bypass "token" is
/// consumed. The gate only logs a bypass when it actually had to suppress a violation.
#[test]
fn gap2_compliant_commit_consumes_no_bypass() {
    let cfg = conventional_commit_only();

    let record = gated_commit_or_bypass(&cfg, "feat: compliant message", None)
        .expect("a compliant commit must pass even through the bypass path");

    assert!(
        record.is_none(),
        "no bypass record must be produced for a commit that satisfies the rules: {record:?}"
    );
}

// ════════════════════════════════════════════════════════════════════════════════════
// GAP-2 PR gate
// ════════════════════════════════════════════════════════════════════════════════════

/// The PR gate mirrors the commit gate at the `POST /api/pr/open` chokepoint.
/// An empty PR body (which the commit-doc rule treats as lacking a story id and
/// substantive context) must be blocked; a body with a story id must pass.
#[test]
fn gap2_pr_gate_blocks_and_passes() {
    let cfg = full_doc_config();

    // Empty body -> blocked (no story id, no substantive context).
    assert!(
        gated_pr(&cfg, "Add export endpoint", "").is_err(),
        "a PR with an empty body must be blocked when the doc rule is enabled"
    );

    // Body with substantive content + story id -> passes.
    gated_pr(
        &cfg,
        "Add export endpoint",
        "Implements the CSV export feature end to end. Closes #99.",
    )
    .expect("a PR with a substantive body and story id must pass the gate");
}

/// Machine-generated PRs (governance PR, onboarding PR) use the `_or_bypass` path.
/// A reasoned bypass is allowed and produces an audit record naming the suppressed rule.
#[test]
fn gap2_pr_bypass_with_reason_produces_audit_record() {
    let cfg = full_doc_config();

    let record = gated_pr_or_bypass(
        &cfg,
        "chore: apply governance files",
        "",
        Some("machine-generated onboarding governance PR — title and body are Camerata-authored"),
    )
    .expect("bypass with a non-empty reason must succeed");

    // The PR body is empty and would have been blocked; the bypass record must exist.
    let summary = record.expect("a bypass record must be produced for a blocked action with a reason");
    assert!(
        !summary.is_empty(),
        "the bypass record must contain the suppressed rule name(s): {summary}"
    );
}
