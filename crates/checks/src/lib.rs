//! camerata-checks: layer-2 post-task gate.
//!
//! Implements [`camerata_core::CheckRunner`] for Rust worktrees. Three concrete
//! runners, composed by [`RustCheckRunner`]:
//!
//! - [`FmtCheckRunner`] -- shells out to `cargo fmt --check`, maps failure to
//!   `RUST-FMT`.
//! - [`ClippyCheckRunner`] -- shells out to `cargo clippy`, maps warnings/errors
//!   to `RUST-CLIPPY`.
//! - [`TestCheckRunner`] -- shells out to `cargo test --no-fail-fast`, maps a
//!   failed test or a compile failure to `RUST-TEST`.
//!
//! The subprocess invocation layer ([`subprocess`]) and the output-to-RuleId
//! mapping layer ([`parse`]) are kept separate so the mapping logic can be
//! unit-tested without spawning real subprocesses.

/// Architectural (AST-tier) governance checks: deterministic structural rules
/// that no regex can express and no LLM is needed to judge (e.g. "a handler does
/// not touch the DB directly"). Ships a self-contained PROOF checker today; the
/// `syn`-backed production design is routed in
/// `docs/decisions/2026-06-19_ast_architectural_rule_tier.md`.
pub mod architectural;

/// Per-language layer-2 [`camerata_core::CheckRunner`]s (JS/TS, Python, Go) plus
/// the worktree language-detect selector ([`multilang::runner_for_worktree`])
/// that injects the right one. Closes the cross-language layer-2 gap where the
/// coordinator was hardcoded to the Rust-only [`RustCheckRunner`].
/// See `docs/decisions/2026-06-21_multilang_layer2_checkrunner.md`.
pub mod multilang;
pub use multilang::{
    detect_language, runner_for_worktree, GoCheckRunner, JsCheckRunner, PythonCheckRunner,
    WorktreeLanguage,
};

pub mod parse;
pub mod subprocess;

/// The VCS-action gate: deterministic process rules (`PROCESS-*`) over commit /
/// PR / branch METADATA — the fourth enforcement point. Distinct from the
/// content-layer `CheckRunner` in this crate: it gates the metadata of the
/// commit/PR Camerata is about to perform, the one place no code gate can see.
/// See `docs/decisions/2026-06-15_process_rules_and_vcs_action_gate.md`.
pub mod vcs_action;

/// The verification-mechanics gates over the rule corpus: the deny-gate that
/// keeps `verification = "verified"` human-only (agents may set at most
/// `grounded`), and the staleness pass that demotes a drifted `verified` rule to
/// `needs_recheck`. The dogfood of the grounding ladder in `camerata-rules`.
/// See `docs/decisions/2026-06-20_verification_mechanics.md`.
pub mod verification_gate;

use anyhow::Context as _;
use async_trait::async_trait;
use camerata_core::{CheckRunner, Role, RuleId};
use std::path::Path;
use thiserror::Error;

// ─── crate-local error type (RUST-DOMAIN-4/6) ────────────────────────────────

#[derive(Debug, Error)]
pub enum CheckError {
    #[error("subprocess failed to spawn: {0}")]
    SpawnFailed(#[from] std::io::Error),

    #[error("subprocess produced non-UTF-8 output")]
    NonUtf8Output,

    #[error("check tool exited with unexpected status {code}: {stderr}")]
    ToolError { code: i32, stderr: String },
}

// ─── newtype marker for the rule IDs this crate owns ─────────────────────────
//
// RuleId itself is defined in camerata-core; we expose named constructors so
// the rest of the stack never hard-codes the strings.

pub fn fmt_rule() -> RuleId {
    RuleId("RUST-FMT".to_string())
}

pub fn clippy_rule() -> RuleId {
    RuleId("RUST-CLIPPY".to_string())
}

pub fn test_rule() -> RuleId {
    RuleId("RUST-TEST".to_string())
}

// ─── fmt runner ──────────────────────────────────────────────────────────────

/// Runs `cargo fmt --check` and returns `[RUST-FMT]` if the worktree has
/// unformatted files.
pub struct FmtCheckRunner;

#[async_trait]
impl CheckRunner for FmtCheckRunner {
    async fn check(&self, _role: &Role, worktree: &Path) -> anyhow::Result<Vec<RuleId>> {
        let output = subprocess::run_fmt_check(worktree)
            .await
            .context("running cargo fmt --check")?;

        Ok(parse::map_fmt_output(&output))
    }
}

// ─── clippy runner ───────────────────────────────────────────────────────────

/// Runs `cargo clippy -- -D warnings` and returns `[RUST-CLIPPY]` if any lint
/// fires.
pub struct ClippyCheckRunner;

#[async_trait]
impl CheckRunner for ClippyCheckRunner {
    async fn check(&self, _role: &Role, worktree: &Path) -> anyhow::Result<Vec<RuleId>> {
        let output = subprocess::run_clippy(worktree)
            .await
            .context("running cargo clippy")?;

        Ok(parse::map_clippy_output(&output))
    }
}

// ─── test runner ───────────────────────────────────────────────────────────

/// Runs `cargo test --no-fail-fast` and returns `[RUST-TEST]` if any test fails
/// or the crate does not compile.
pub struct TestCheckRunner;

#[async_trait]
impl CheckRunner for TestCheckRunner {
    async fn check(&self, _role: &Role, worktree: &Path) -> anyhow::Result<Vec<RuleId>> {
        let output = subprocess::run_test(worktree)
            .await
            .context("running cargo test")?;

        Ok(parse::map_test_output(&output))
    }
}

// ─── composite runner ────────────────────────────────────────────────────────

/// Runs fmt, clippy, AND test checks; aggregates all violated rule ids. The
/// coordinator uses this as its default Rust gate, so generated code that
/// compiles and lints clean but fails its own tests is still bounced back for
/// revision (the failure mode that otherwise becomes silent debt).
pub struct RustCheckRunner {
    fmt: FmtCheckRunner,
    clippy: ClippyCheckRunner,
    test: TestCheckRunner,
}

impl RustCheckRunner {
    pub fn new() -> Self {
        Self {
            fmt: FmtCheckRunner,
            clippy: ClippyCheckRunner,
            test: TestCheckRunner,
        }
    }
}

impl Default for RustCheckRunner {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl CheckRunner for RustCheckRunner {
    async fn check(&self, role: &Role, worktree: &Path) -> anyhow::Result<Vec<RuleId>> {
        // Run sequentially, cheapest-first: fmt errors often make clippy noisy,
        // and a clippy/compile failure makes the test run redundant. Ordering the
        // gate this way surfaces the cheapest fix first in the bounce-back.
        let mut violations = self.fmt.check(role, worktree).await?;
        let mut clippy_violations = self.clippy.check(role, worktree).await?;
        violations.append(&mut clippy_violations);
        let mut test_violations = self.test.check(role, worktree).await?;
        violations.append(&mut test_violations);
        // Deduplicate so the bounce-back message is clean.
        violations.dedup_by(|a, b| a.0 == b.0);
        Ok(violations)
    }
}
