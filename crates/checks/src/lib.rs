//! camerata-checks: layer-2 post-task gate.
//!
//! Implements [`camerata_core::CheckRunner`] for Rust worktrees. Phase 0 ships
//! two concrete runners:
//!
//! - [`FmtCheckRunner`]    — shells out to `cargo fmt --check`, maps failure to
//!                           `RUST-FMT`.
//! - [`ClippyCheckRunner`] — shells out to `cargo clippy`, maps warnings/errors
//!                           to `RUST-CLIPPY`.
//!
//! The subprocess invocation layer ([`subprocess`]) and the output-to-RuleId
//! mapping layer ([`parse`]) are kept separate so the mapping logic can be
//! unit-tested without spawning real subprocesses.

pub mod parse;
pub mod subprocess;

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

// ─── composite runner ────────────────────────────────────────────────────────

/// Runs both fmt and clippy checks; aggregates all violated rule ids.
/// The coordinator can use this as its default Rust gate.
pub struct RustCheckRunner {
    fmt: FmtCheckRunner,
    clippy: ClippyCheckRunner,
}

impl RustCheckRunner {
    pub fn new() -> Self {
        Self {
            fmt: FmtCheckRunner,
            clippy: ClippyCheckRunner,
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
        // Run sequentially; fmt is cheap and its errors often make clippy noisy.
        let mut violations = self.fmt.check(role, worktree).await?;
        let mut clippy_violations = self.clippy.check(role, worktree).await?;
        violations.append(&mut clippy_violations);
        // Deduplicate so the bounce-back message is clean.
        violations.dedup_by(|a, b| a.0 == b.0);
        Ok(violations)
    }
}
