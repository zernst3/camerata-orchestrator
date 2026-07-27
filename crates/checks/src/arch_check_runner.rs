//! [`NativeArchCheckRunner`] — the Layer-2 executor for the deterministic
//! architectural-rule seam ([`crate::arch_checker`]).
//!
//! See `docs/design/2026-07-26_architectural-executor-feasibility.md` §2.3 ("Plug point B")
//! for the design rationale: architectural checks CANNOT run per-`gated_write` in Layer 1
//! (mid-edit trees don't parse — the 06-19 ADR's own argument). The write-time enforcement
//! surface for them is Layer 2, at the checkpoint, where the ASSEMBLED worktree exists — a
//! violation there bounces the agent's work back for revision before it ever becomes a
//! commit, exactly like a clippy failure.
//!
//! # Design
//!
//! - **Native sibling of [`crate::manifest_runner::ManifestCheckRunner`]**: same position in
//!   the loop (composed into [`crate::multilang::CombinedCheckRunner`] right beside it), but
//!   ZERO operator-authored shell — the checker ships inside Camerata. Where the manifest
//!   runner shells out to a command the operator wrote, this runner calls
//!   [`crate::arch_checker::ArchChecker::check`] directly, in-process.
//! - **"Armed" rule ids come from `role.rule_subset`** — the SAME field the Layer-1 gateway
//!   already enforces against (`camerata_gateway::evaluate_call(&driver.rule_subset, ...)`,
//!   `crates/server/src/api_agent_driver.rs`). There is exactly one per-session/per-work-item
//!   ruleset in this codebase; this runner reuses it rather than inventing a second binding.
//!   A checker only runs when at least one of its own `rule_ids()` is armed — mirroring
//!   exactly how the scan's `audit_architectural` (`crates/server/src/onboard/architectural.rs`,
//!   Pass 1) binds checkers to the repo's selected ruleset.
//! - **Cheap by construction**: only the UNION of every ARMED checker's `interest_globs` is
//!   read from the worktree. An un-armed architectural rule's checker never even sees its
//!   files, let alone runs — asserted directly in the tests below.
//! - **Never panics**: a missing/unreadable directory, an unreadable file, or a non-UTF8 file
//!   is skipped, never propagated as an error. The checkers underneath (the Supabase
//!   splitter/classifier) are already panic-free by construction (Pass 1's adversarial
//!   battery); this runner extends the same posture to the filesystem read itself, so a
//!   worktree with malformed/partial SQL degrades to fewer (or zero) violations, never a
//!   crash that takes the whole Layer-2 loop down with it.

use std::collections::HashSet;
use std::path::Path;

use async_trait::async_trait;
use camerata_core::{CheckOutcome, CheckRunner, Role, RuleId};

use crate::arch_checker::{all_checkers, checker_applies, matches_any_glob, ArchViolation, RepoView};

/// Layer-2 runner for the native architectural-checker registry
/// ([`crate::arch_checker::all_checkers`]).
///
/// Stateless — every `check` call rebuilds its file view fresh from the worktree, the same
/// way the manifest runner rebuilds its manifest per invocation. No fields today; kept as a
/// unit struct (rather than a free function) so it implements [`CheckRunner`] and composes
/// into [`crate::multilang::CombinedCheckRunner`] exactly like every other tier.
#[derive(Debug, Default, Clone, Copy)]
pub struct NativeArchCheckRunner;

impl NativeArchCheckRunner {
    /// Build a runner. No configuration today — the registry
    /// ([`crate::arch_checker::all_checkers`]) and the armed rule ids (`role.rule_subset`,
    /// supplied per-call) are the only inputs.
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl CheckRunner for NativeArchCheckRunner {
    async fn check(&self, role: &Role, worktree: &Path) -> anyhow::Result<CheckOutcome> {
        let armed: HashSet<&str> = role.rule_subset.iter().map(|r| r.0.as_str()).collect();
        let checkers = all_checkers();

        // Only checkers with at least one ARMED rule id get to read anything. An un-armed
        // architectural rule's checker never even sees its interest files.
        let armed_checkers: Vec<_> = checkers
            .iter()
            .filter(|c| c.rule_ids().iter().any(|id| armed.contains(id)))
            .collect();

        if armed_checkers.is_empty() {
            // No architectural rule armed for this role/work item — clean, without
            // touching the filesystem at all.
            return Ok(CheckOutcome::clean());
        }

        let mut globs: Vec<&str> = armed_checkers
            .iter()
            .flat_map(|c| c.interest_globs().iter().copied())
            .collect();
        globs.sort_unstable();
        globs.dedup();

        let files = collect_interest_files(worktree, &globs);
        let repo_label = worktree.display().to_string();
        let view = RepoView {
            spec: &repo_label,
            files: &files,
        };

        let mut outcome = CheckOutcome::clean();
        for checker in &armed_checkers {
            // Zero matching files -> skip entirely; never a false "clean" read as "this
            // checker ran and found nothing" — see `checker_applies`'s own doc.
            if !checker_applies(checker.as_ref(), &files) {
                continue;
            }
            for violation in checker.check(&view) {
                // Defensive re-check at the gate boundary: a checker's `check` must only
                // emit ids from its own `rule_ids()`, but this runner never trusts that
                // blindly — an un-armed id must never bounce the loop, even on a
                // hypothetical checker-side bug.
                if !armed.contains(violation.rule_id.as_str()) {
                    continue;
                }
                outcome.push_diagnostics(&format_violation(&violation));
                outcome.violated.push(RuleId(violation.rule_id.clone()));
            }
        }
        outcome.violated.dedup_by(|a, b| a.0 == b.0);
        Ok(outcome)
    }
}

/// Render one [`ArchViolation`] as a human-readable diagnostics line: `file:line [rule_id]
/// object — message`. This is what lands in [`CheckOutcome::diagnostics`] and is forwarded
/// straight into the Layer-2 bounce prompt — the "exact file/line" the design memo calls for
/// ("an agent that writes a migration adding a table without RLS gets bounced in-loop,
/// deterministically, with the exact file/line").
fn format_violation(v: &ArchViolation) -> String {
    match (&v.object, v.line) {
        (Some(obj), line) if line > 0 => {
            format!("{}:{} [{}] {} — {}", v.file, line, v.rule_id, obj, v.message)
        }
        (Some(obj), _) => format!("{} [{}] {} — {}", v.file, v.rule_id, obj, v.message),
        (None, line) if line > 0 => format!("{}:{} [{}] {}", v.file, line, v.rule_id, v.message),
        (None, _) => format!("{} [{}] {}", v.file, v.rule_id, v.message),
    }
}

/// Recursively enumerate every file under `worktree` (pruning the same
/// build-output/vendored/VCS directories [`crate::multilang::detect_languages`] prunes —
/// see [`crate::multilang::PRUNED_DIRS`]) whose repo-relative path matches at least one of
/// `globs`, reading CONTENT only for a matching file. This is the "interest_globs, cheap"
/// contract: directory traversal is bounded by the prune list, but no file's bytes are read
/// unless its path is something an armed checker actually declared interest in.
///
/// Never panics and never fails the gate: an unreadable directory, an unreadable file, or a
/// non-UTF8 file is skipped (a readable-but-non-UTF8 file is lossily decoded, not dropped, so
/// a checker still sees a best-effort text view) — consistent with "a malformed/adversarial
/// input file degrades, never crashes."
fn collect_interest_files(worktree: &Path, globs: &[&str]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if !globs.is_empty() {
        walk_collect(worktree, worktree, globs, &mut out);
    }
    out
}

fn walk_collect(root: &Path, dir: &Path, globs: &[&str], out: &mut Vec<(String, String)>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return, // missing/unreadable dir: skip silently, non-fatal
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            let name = entry.file_name();
            if crate::multilang::PRUNED_DIRS.contains(&name.to_string_lossy().as_ref()) {
                continue;
            }
            walk_collect(root, &path, globs, out);
            continue;
        }
        if !file_type.is_file() {
            continue; // symlinks / sockets / etc — never followed, defensive
        }
        let Ok(rel) = path.strip_prefix(root) else {
            continue;
        };
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        if !matches_any_glob(globs, &rel_str) {
            continue;
        }
        match std::fs::read(&path) {
            Ok(bytes) => out.push((rel_str, String::from_utf8_lossy(&bytes).into_owned())),
            Err(_) => continue, // unreadable file: skip, never panic
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmpdir() -> tempfile::TempDir {
        tempfile::tempdir().expect("failed to create tempdir")
    }

    fn role_armed_with(ids: &[&str]) -> Role {
        Role {
            name: "Test".to_string(),
            rule_subset: ids.iter().map(|id| RuleId(id.to_string())).collect(),
            allowed_paths: vec![],
        }
    }

    fn write_migration(dir: &Path, name: &str, sql: &str) {
        let migrations = dir.join("supabase").join("migrations");
        fs::create_dir_all(&migrations).unwrap();
        fs::write(migrations.join(name), sql).unwrap();
    }

    const RLS_ENABLED: &str = "SUPABASE-RLS-ENABLED-1";
    const RLS_NO_POLICY: &str = "SUPABASE-RLS-NO-POLICY-1";
    const RLS_POLICY_DISABLED: &str = "SUPABASE-RLS-POLICY-DISABLED-1";
    const FUNC_SEARCH_PATH: &str = "SUPABASE-FUNC-SEARCH-PATH-1";

    // ── maps ArchViolation -> CheckOutcome correctly ──────────────────────────

    #[tokio::test]
    async fn violation_maps_into_outcome_with_rule_id_and_location() {
        let dir = tmpdir();
        write_migration(
            dir.path(),
            "20240101000000_init.sql",
            "create table public.profiles (id uuid primary key);\n",
        );
        let role = role_armed_with(&[RLS_ENABLED, RLS_NO_POLICY, RLS_POLICY_DISABLED]);
        let runner = NativeArchCheckRunner::new();

        let outcome = runner.check(&role, dir.path()).await.expect("must not err");
        assert_eq!(
            outcome.violated,
            vec![RuleId(RLS_ENABLED.to_string())],
            "unexposed-RLS table with no RLS must bounce under the exact rule id"
        );
        assert!(
            outcome.diagnostics.contains("20240101000000_init.sql"),
            "diagnostics must carry the establishing file: {:?}",
            outcome.diagnostics
        );
        assert!(
            outcome.diagnostics.contains(":1"),
            "diagnostics must carry the establishing line: {:?}",
            outcome.diagnostics
        );
        assert!(
            outcome.diagnostics.contains(RLS_ENABLED),
            "diagnostics must carry the rule id: {:?}",
            outcome.diagnostics
        );
    }

    // ── respects armed rule ids: un-armed rule never fires ────────────────────

    #[tokio::test]
    async fn unarmed_rule_never_fires_even_though_checker_would_match() {
        let dir = tmpdir();
        write_migration(
            dir.path(),
            "20240101000000_init.sql",
            "create table public.profiles (id uuid primary key);\n",
        );
        // Arm only an unrelated rule id — NOT any of the RLS family.
        let role = role_armed_with(&[FUNC_SEARCH_PATH]);
        let runner = NativeArchCheckRunner::new();

        let outcome = runner.check(&role, dir.path()).await.expect("must not err");
        assert!(
            outcome.violated.is_empty(),
            "no RLS rule armed -> RLS checker must not fire, even though its files are \
             present and would otherwise match: {:?}",
            outcome.violated
        );
    }

    #[tokio::test]
    async fn nothing_armed_at_all_returns_clean_without_touching_disk() {
        let dir = tmpdir();
        write_migration(
            dir.path(),
            "20240101000000_init.sql",
            "create table public.profiles (id uuid primary key);\n",
        );
        let role = role_armed_with(&[]);
        let runner = NativeArchCheckRunner::new();

        let outcome = runner.check(&role, dir.path()).await.expect("must not err");
        assert_eq!(outcome, CheckOutcome::clean());
    }

    // ── clean when no interest-glob files exist ───────────────────────────────

    #[tokio::test]
    async fn clean_when_no_interest_glob_files_exist() {
        let dir = tmpdir(); // no supabase/ directory at all
        fs::write(dir.path().join("README.md"), "hello").unwrap();
        let role = role_armed_with(&[RLS_ENABLED, RLS_NO_POLICY, RLS_POLICY_DISABLED]);
        let runner = NativeArchCheckRunner::new();

        let outcome = runner.check(&role, dir.path()).await.expect("must not err");
        assert_eq!(
            outcome,
            CheckOutcome::clean(),
            "zero matching files -> clean, never a spurious finding"
        );
    }

    // ── a compliant worktree (RLS enabled + policy) passes clean ──────────────

    #[tokio::test]
    async fn compliant_rls_worktree_passes_clean() {
        let dir = tmpdir();
        write_migration(
            dir.path(),
            "20240101000000_init.sql",
            "create table public.orders (id uuid primary key, user_id uuid not null);\n\
             alter table public.orders enable row level security;\n\
             create policy \"orders_select_own\" on public.orders for select using (auth.uid() = user_id);\n",
        );
        let role = role_armed_with(&[RLS_ENABLED, RLS_NO_POLICY, RLS_POLICY_DISABLED]);
        let runner = NativeArchCheckRunner::new();

        let outcome = runner.check(&role, dir.path()).await.expect("must not err");
        assert_eq!(
            outcome,
            CheckOutcome::clean(),
            "RLS enabled + a real policy must pass clean: {:?}",
            outcome
        );
    }

    // ── never panics on malformed/partial SQL (adversarial) ───────────────────

    #[tokio::test]
    async fn malformed_sql_degrades_gracefully_never_panics() {
        let dir = tmpdir();
        // Unterminated dollar-quote, unterminated string, unterminated block comment, and
        // some binary-ish bytes thrown in — the splitter's own adversarial battery (Pass 1)
        // already proves the splitter is panic-free; this proves the runner's file-walk and
        // wiring around it hold up too.
        write_migration(
            dir.path(),
            "20240101000000_garbage.sql",
            "create table public.x (id uuid); \
             create function public.f() returns void as $$ begin -- never closed\n\
             insert into t values ('unterminated string;\n\
             /* unterminated comment\n\
             \u{0}\u{1}\u{2} garbage bytes",
        );
        let role = role_armed_with(&[RLS_ENABLED, RLS_NO_POLICY, RLS_POLICY_DISABLED, FUNC_SEARCH_PATH]);
        let runner = NativeArchCheckRunner::new();

        // The only contract under test: this must not panic and must return Ok. Whatever it
        // decides about violations is secondary (best-effort over unparseable input).
        let result = runner.check(&role, dir.path()).await;
        assert!(result.is_ok(), "malformed SQL must never propagate as Err: {result:?}");
    }

    #[tokio::test]
    async fn non_utf8_migration_file_does_not_panic() {
        let dir = tmpdir();
        let migrations = dir.path().join("supabase").join("migrations");
        fs::create_dir_all(&migrations).unwrap();
        // Invalid UTF-8 bytes — must be lossily decoded, never crash the walk.
        fs::write(migrations.join("20240101000000_binary.sql"), [0xff, 0xfe, 0x00, 0x80, 0x81]).unwrap();
        let role = role_armed_with(&[RLS_ENABLED]);
        let runner = NativeArchCheckRunner::new();

        let result = runner.check(&role, dir.path()).await;
        assert!(result.is_ok(), "non-UTF8 file must never propagate as Err: {result:?}");
    }

    // ── glob-scoping: reading is limited to armed checkers' interest globs ────

    #[tokio::test]
    async fn irrelevant_files_outside_interest_globs_are_ignored() {
        let dir = tmpdir();
        // A file that would trip up any naive "read everything" walk if it were parsed as
        // SQL, but sits outside every checker's interest_globs.
        fs::write(dir.path().join("notes.sql"), "DROP TABLE everything; -- not a migration").unwrap();
        write_migration(
            dir.path(),
            "20240101000000_init.sql",
            "create table public.orders (id uuid primary key, user_id uuid not null);\n\
             alter table public.orders enable row level security;\n\
             create policy \"p\" on public.orders for select using (true);\n",
        );
        let role = role_armed_with(&[RLS_ENABLED, RLS_NO_POLICY, RLS_POLICY_DISABLED]);
        let runner = NativeArchCheckRunner::new();

        let outcome = runner.check(&role, dir.path()).await.expect("must not err");
        assert_eq!(
            outcome,
            CheckOutcome::clean(),
            "the stray root-level notes.sql must never be treated as a migration: {:?}",
            outcome
        );
    }
}
