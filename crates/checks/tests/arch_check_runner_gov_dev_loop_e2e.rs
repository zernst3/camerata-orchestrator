// Integration tests unwrap freely on setup I/O (test fixtures); the workspace
// convention is a file-level allow for `crates/*/tests/` (see root Cargo.toml).
#![allow(clippy::unwrap_used)]

//! Hermetic end-to-end integration tests for the Layer-2 Governed Development write-time
//! gate's architectural-checker tier (Pass 2 — see
//! `docs/design/2026-07-26_architectural-executor-feasibility.md` §2.3, "Plug point B").
//!
//! These drive the PUBLIC entry point a real gov-dev loop uses —
//! [`camerata_checks::runner_for_worktree`], which returns the SAME
//! [`camerata_checks::CombinedCheckRunner`] the coordinator/fleet compose against — over a
//! real on-disk worktree containing Supabase migrations. The scenario under test is exactly
//! the one the design memo motivates: "an agent that writes a migration adding a table
//! without RLS gets bounced in-loop, deterministically, with the exact file/line."
//!
//! Black-box and hermetic: no git, no network, no model calls — pure filesystem + the public
//! `CheckRunner` trait, mirroring the existing `vcs_action_gate_e2e.rs` / `integration_gate_e2e.rs`
//! convention of exercising the crate from outside its own modules.

use std::path::Path;

use camerata_checks::runner_for_worktree;
use camerata_core::{Role, RuleId};

const RULE_RLS_ENABLED: &str = "SUPABASE-RLS-ENABLED-1";
const RULE_RLS_NO_POLICY: &str = "SUPABASE-RLS-NO-POLICY-1";
const RULE_RLS_POLICY_DISABLED: &str = "SUPABASE-RLS-POLICY-DISABLED-1";

fn role_armed_with_rls_family() -> Role {
    Role {
        name: "Backend".to_string(),
        rule_subset: [RULE_RLS_ENABLED, RULE_RLS_NO_POLICY, RULE_RLS_POLICY_DISABLED]
            .into_iter()
            .map(|id| RuleId(id.to_string()))
            .collect(),
        allowed_paths: vec!["supabase/".to_string()],
    }
}

fn write_migration(worktree: &Path, name: &str, sql: &str) {
    let migrations = worktree.join("supabase").join("migrations");
    std::fs::create_dir_all(&migrations).unwrap();
    std::fs::write(migrations.join(name), sql).unwrap();
}

/// A worktree whose migration adds a public table and NEVER enables RLS on it must be
/// BOUNCED by the combined Layer-2 runner: the exact rule the write-time gate would enforce,
/// with the exact table + establishing file:line surfaced in the diagnostics, so the agent
/// can self-correct without a human in the loop.
#[tokio::test]
async fn worktree_with_public_table_missing_rls_is_bounced_with_exact_location() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_migration(
        dir.path(),
        "20240515000000_add_profiles.sql",
        "create table public.profiles (\n\
         \x20\x20id uuid primary key default gen_random_uuid(),\n\
         \x20\x20email text not null\n\
         );\n",
    );

    let runner = runner_for_worktree(dir.path());
    let role = role_armed_with_rls_family();
    let outcome = runner
        .check(&role, dir.path())
        .await
        .expect("gov-dev loop check must not error on a well-formed worktree");

    assert!(
        outcome.violated.contains(&RuleId(RULE_RLS_ENABLED.to_string())),
        "a public table with no RLS must bounce under SUPABASE-RLS-ENABLED-1, got: {:?}",
        outcome.violated
    );
    assert!(
        outcome.diagnostics.contains("20240515000000_add_profiles.sql"),
        "the bounce diagnostics must name the establishing migration file: {:?}",
        outcome.diagnostics
    );
    assert!(
        outcome.diagnostics.contains("profiles"),
        "the bounce diagnostics must name the offending table: {:?}",
        outcome.diagnostics
    );
}

/// The mirror-image control: a worktree whose migration adds a public table AND correctly
/// enables RLS with a real policy must PASS clean through the same combined runner — proving
/// the gate doesn't cry wolf on compliant work.
#[tokio::test]
async fn worktree_with_rls_correctly_enabled_passes_clean() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_migration(
        dir.path(),
        "20240515000000_add_orders.sql",
        "create table public.orders (\n\
         \x20\x20id uuid primary key default gen_random_uuid(),\n\
         \x20\x20user_id uuid not null\n\
         );\n\n\
         alter table public.orders enable row level security;\n\n\
         create policy \"orders_select_own\" on public.orders\n\
         \x20\x20for select\n\
         \x20\x20using (auth.uid() = user_id);\n",
    );

    let runner = runner_for_worktree(dir.path());
    let role = role_armed_with_rls_family();
    let outcome = runner
        .check(&role, dir.path())
        .await
        .expect("gov-dev loop check must not error on a well-formed worktree");

    assert!(
        !outcome.violated.contains(&RuleId(RULE_RLS_ENABLED.to_string())),
        "RLS-enabled + policy must not trip RLS-ENABLED-1: {:?}",
        outcome.violated
    );
    assert!(
        !outcome.violated.contains(&RuleId(RULE_RLS_NO_POLICY.to_string())),
        "RLS-enabled + policy must not trip RLS-NO-POLICY-1: {:?}",
        outcome.violated
    );
    assert!(
        !outcome.violated.contains(&RuleId(RULE_RLS_POLICY_DISABLED.to_string())),
        "RLS-enabled + policy must not trip RLS-POLICY-DISABLED-1: {:?}",
        outcome.violated
    );
}

/// An architectural rule that is NOT armed for this role/work item must never bounce the
/// loop, even though its checker's interest files are present and would otherwise match —
/// this is the "armed rule ids" contract the design memo calls out explicitly for the
/// write-time gate (an un-armed rule is not the agent's problem right now).
#[tokio::test]
async fn unarmed_rls_rule_does_not_bounce_the_combined_runner() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_migration(
        dir.path(),
        "20240515000000_add_profiles.sql",
        "create table public.profiles (id uuid primary key);\n",
    );

    let runner = runner_for_worktree(dir.path());
    let role = Role {
        name: "Backend".to_string(),
        // Nothing armed at all -- no rule this build's checkers know about.
        rule_subset: vec![],
        allowed_paths: vec![],
    };
    let outcome = runner
        .check(&role, dir.path())
        .await
        .expect("gov-dev loop check must not error");

    assert!(
        outcome.violated.is_empty(),
        "no architectural rule armed -> the combined runner must not bounce on it: {:?}",
        outcome.violated
    );
}

/// Composition sanity: the combined runner over an empty (no-language, no-manifest) worktree
/// with the RLS family armed but no `supabase/` directory at all must stay clean — proving
/// the new architectural tier doesn't regress the existing "no manifest, no language ->
/// NoopChecks reports clean" behavior the selector already guarantees.
#[tokio::test]
async fn empty_worktree_with_rls_armed_but_no_supabase_dir_stays_clean() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("README.md"), "nothing to see here").unwrap();

    let runner = runner_for_worktree(dir.path());
    let role = role_armed_with_rls_family();
    let outcome = runner
        .check(&role, dir.path())
        .await
        .expect("gov-dev loop check must not error on a manifest-less, language-less worktree");

    assert!(
        outcome.violated.is_empty(),
        "zero supabase files present -> clean, never a spurious finding: {:?}",
        outcome.violated
    );
}
