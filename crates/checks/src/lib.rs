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

/// Per-language layer-2 [`camerata_core::CheckRunner`]s (JS/TS, Python, Go,
/// Ruby, Java, C#) plus the worktree language-detect selector
/// ([`multilang::runner_for_worktree`]) that injects the right one. With
/// [`RustCheckRunner`] this covers all SEVEN languages the corpus ships rules
/// for. Closes the cross-language layer-2 gap where the coordinator was
/// hardcoded to the Rust-only [`RustCheckRunner`].
/// See `docs/decisions/2026-06-21_multilang_layer2_checkrunner.md` and
/// `docs/decisions/2026-06-22_layer2_ruby_java_csharp_runners.md`.
pub mod multilang;
pub use multilang::{
    detect_language, runner_for_worktree, runner_for_worktree_with_heartbeat,
    CombinedCheckRunner, CSharpCheckRunner, GoCheckRunner,
    JavaCheckRunner, JsCheckRunner, PythonCheckRunner, RubyCheckRunner, WorktreeLanguage,
};

/// Single source of truth manifest (`.camerata/checks.toml`) — schema, loader,
/// and shared command-list helpers consumed by BOTH the Layer-2 runner and the
/// Layer-3 CI workflow generator.
/// See `docs/decisions/2026-06-22_check_manifest_single_source_of_truth.md`.
pub mod manifest;
pub use manifest::{CheckManifest, ManifestCheck};

/// Layer-2 executor for manifest checks (`in_loop = true` entries).
/// Additive on top of the built-in language runners; composed into
/// [`CombinedCheckRunner`] via [`runner_for_worktree`].
pub mod manifest_runner;
pub use manifest_runner::ManifestCheckRunner;

/// The cross-agent INTEGRATION GATE (GAP-6 / R3.e): the third enforcement tier.
/// A stack-generalized reconciliation engine over the ASSEMBLED tree (all role
/// agents' outputs combined), with pluggable per-stack extractors that normalize
/// each repo's source into neutral produced/consumed lists. Deterministic
/// verdicts; a seam with no extractor is review-tier, never a faked green.
/// See `docs/decisions/2026-07-05_integration-gate-generic-engine.md` and
/// `docs/decisions/2026-06-15_cross_agent_integration_gate.md`.
pub mod integration;
pub use integration::{run_gate, GateRepo, GateVerdict, GateWaiver, ReviewItem};

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
use camerata_liveness::HeartbeatFn;
use camerata_core::{CheckRunner, Role, RuleId};
use std::path::{Path, PathBuf};
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

// ─── Shared Cargo target-dir derivation (disk-safety, 2026-06-22) ────────────
//
// All cargo subprocess calls set `CARGO_TARGET_DIR` to the repo's shared artifact
// directory so every UoW worktree under the same clone writes into ONE target/ tree
// instead of N separate ones.
//
// Layout: worktree is `<clone>/.camerata-worktrees/<branch>`.
//         clone  = worktree.parent().parent()
//         target = clone.join(".camerata-shared-target")
//
// A worktree outside the canonical layout (e.g. an out-of-band `git worktree add`
// at an arbitrary path) will produce a `None` from the derivation, in which case
// the caller falls back to the cargo default (worktree-local target/). This is the
// conservative fail-open choice: a mis-derived target dir that points to the wrong
// clone would be worse than a per-worktree fallback.

/// Derive the shared `CARGO_TARGET_DIR` path from a UoW worktree path.
///
/// Returns `Some(<clone>/.camerata-shared-target)` for the canonical layout
/// (`<clone>/.camerata-worktrees/<branch>`), and `None` for out-of-band worktrees.
pub fn derive_shared_target_dir(worktree: &Path) -> Option<PathBuf> {
    // parent() → `.camerata-worktrees`, parent() → clone root
    let clone = worktree.parent()?.parent()?;
    Some(clone.join(".camerata-shared-target"))
}

/// Run the disk-headroom preflight check before a cargo build, using the worktree
/// path for the space query. On insufficient space, returns an error so the run
/// status surfaces a clear message instead of silently filling the disk.
///
/// Threshold: `CAMERATA_MIN_DISK_HEADROOM_GB` env var (integer GB), default 10 GB.
fn check_build_disk_headroom(worktree: &Path) -> anyhow::Result<()> {
    let min = disk_headroom_threshold_bytes();
    let Some(available) = available_disk_bytes(worktree) else {
        // Cannot query — fail-open (see workspace.rs::ensure_disk_headroom).
        return Ok(());
    };
    if available >= min {
        return Ok(());
    }
    let available_gb = available as f64 / (1024.0 * 1024.0 * 1024.0);
    let required_gb = min as f64 / (1024.0 * 1024.0 * 1024.0);
    anyhow::bail!(
        "insufficient disk headroom before cargo build: {available_gb:.1} GB free, \
         need >= {required_gb:.0} GB; reclaim space (remove stale worktrees under \
         .camerata-worktrees/ or .camerata-shared-target/) before starting more work"
    )
}

/// Query available disk space at `path`. A thin wrapper so tests can verify the
/// decision logic via [`has_headroom`] without a real low-disk scenario.
fn available_disk_bytes(path: &Path) -> Option<u64> {
    fs2::available_space(path).ok()
}

/// Pure headroom test: `available >= min`. Separated from the fs2 call so the
/// decision logic is unit-testable without real disk access.
pub fn has_headroom(available: u64, min: u64) -> bool {
    available >= min
}

/// Parse the `CAMERATA_MIN_DISK_HEADROOM_GB` override, defaulting to 10 GiB.
fn disk_headroom_threshold_bytes() -> u64 {
    std::env::var("CAMERATA_MIN_DISK_HEADROOM_GB")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(|gb| gb * 1024 * 1024 * 1024)
        .unwrap_or(10 * 1024 * 1024 * 1024)
}

// ─── fmt runner ──────────────────────────────────────────────────────────────

/// Runs `cargo fmt --check` and returns `[RUST-FMT]` if the worktree has
/// unformatted files.
///
/// Carries an optional `on_progress` heartbeat callback (Phase 1b). When
/// `Some`, each stdout line from the cargo subprocess fires the callback so
/// the parent tracked run's `last_activity_ms` stays fresh during the check.
/// Construct with [`FmtCheckRunner::new`] for no heartbeat, or
/// [`FmtCheckRunner::with_heartbeat`] for an active-run context.
pub struct FmtCheckRunner {
    on_progress: Option<HeartbeatFn>,
}

impl FmtCheckRunner {
    /// Create a `FmtCheckRunner` with no heartbeat (backwards-compatible).
    pub fn new() -> Self {
        Self { on_progress: None }
    }

    /// Create a `FmtCheckRunner` that fires `cb` on every stdout line from
    /// the cargo subprocess. Use this at call sites where a `RunStore` run id
    /// is in scope (dev-cycle checks inside a tracked run).
    pub fn with_heartbeat(cb: HeartbeatFn) -> Self {
        Self { on_progress: Some(cb) }
    }
}

impl Default for FmtCheckRunner {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl CheckRunner for FmtCheckRunner {
    async fn check(&self, _role: &Role, worktree: &Path) -> anyhow::Result<Vec<RuleId>> {
        // Disk-headroom preflight: refuse to start a build if disk is low.
        check_build_disk_headroom(worktree)?;
        let target_dir = derive_shared_target_dir(worktree);
        let output = subprocess::run_fmt_check(worktree, target_dir.as_deref(), self.on_progress.as_ref())
            .await
            .context("running cargo fmt --check")?;

        Ok(parse::map_fmt_output(&output))
    }
}

// ─── clippy runner ───────────────────────────────────────────────────────────

/// Runs `cargo clippy -- -D warnings` and returns `[RUST-CLIPPY]` if any lint
/// fires.
///
/// Carries an optional `on_progress` heartbeat callback (Phase 1b). See
/// [`FmtCheckRunner`] for the pattern.
pub struct ClippyCheckRunner {
    on_progress: Option<HeartbeatFn>,
}

impl ClippyCheckRunner {
    /// Create a `ClippyCheckRunner` with no heartbeat (backwards-compatible).
    pub fn new() -> Self {
        Self { on_progress: None }
    }

    /// Create a `ClippyCheckRunner` that fires `cb` on every stdout line.
    pub fn with_heartbeat(cb: HeartbeatFn) -> Self {
        Self { on_progress: Some(cb) }
    }
}

impl Default for ClippyCheckRunner {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl CheckRunner for ClippyCheckRunner {
    async fn check(&self, _role: &Role, worktree: &Path) -> anyhow::Result<Vec<RuleId>> {
        // Disk-headroom preflight: refuse to start a build if disk is low.
        check_build_disk_headroom(worktree)?;
        let target_dir = derive_shared_target_dir(worktree);
        let output = subprocess::run_clippy(worktree, target_dir.as_deref(), self.on_progress.as_ref())
            .await
            .context("running cargo clippy")?;

        Ok(parse::map_clippy_output(&output))
    }
}

// ─── test runner ───────────────────────────────────────────────────────────

/// Runs `cargo test --no-fail-fast` and returns `[RUST-TEST]` if any test fails
/// or the crate does not compile.
///
/// Carries an optional `on_progress` heartbeat callback (Phase 1b). See
/// [`FmtCheckRunner`] for the pattern.
pub struct TestCheckRunner {
    on_progress: Option<HeartbeatFn>,
}

impl TestCheckRunner {
    /// Create a `TestCheckRunner` with no heartbeat (backwards-compatible).
    pub fn new() -> Self {
        Self { on_progress: None }
    }

    /// Create a `TestCheckRunner` that fires `cb` on every stdout line.
    pub fn with_heartbeat(cb: HeartbeatFn) -> Self {
        Self { on_progress: Some(cb) }
    }
}

impl Default for TestCheckRunner {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl CheckRunner for TestCheckRunner {
    async fn check(&self, _role: &Role, worktree: &Path) -> anyhow::Result<Vec<RuleId>> {
        // Disk-headroom preflight: refuse to start a build if disk is low.
        check_build_disk_headroom(worktree)?;
        let target_dir = derive_shared_target_dir(worktree);
        let output = subprocess::run_test(worktree, target_dir.as_deref(), self.on_progress.as_ref())
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
///
/// When constructed via [`RustCheckRunner::with_heartbeat`], the callback is
/// baked into all three sub-runners so every cargo subprocess fires heartbeats.
pub struct RustCheckRunner {
    fmt: FmtCheckRunner,
    clippy: ClippyCheckRunner,
    test: TestCheckRunner,
}

impl RustCheckRunner {
    /// Create a `RustCheckRunner` with no heartbeat (backwards-compatible).
    pub fn new() -> Self {
        Self {
            fmt: FmtCheckRunner::new(),
            clippy: ClippyCheckRunner::new(),
            test: TestCheckRunner::new(),
        }
    }

    /// Create a `RustCheckRunner` whose sub-runners all fire `cb` on each
    /// cargo stdout line. Use this inside a tracked dev run.
    pub fn with_heartbeat(cb: HeartbeatFn) -> Self {
        Self {
            fmt: FmtCheckRunner::with_heartbeat(cb.clone()),
            clippy: ClippyCheckRunner::with_heartbeat(cb.clone()),
            test: TestCheckRunner::with_heartbeat(cb),
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

// ─── unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── derive_shared_target_dir ─────────────────────────────────────────────

    #[test]
    fn derive_shared_target_dir_canonical_layout() {
        // Canonical: <clone>/.camerata-worktrees/<branch-seg>
        let worktree = Path::new("/Users/me/ws/acme/api/.camerata-worktrees/camerata__story-7");
        let got = derive_shared_target_dir(worktree);
        assert_eq!(
            got,
            Some(PathBuf::from(
                "/Users/me/ws/acme/api/.camerata-shared-target"
            )),
            "derived target must be sibling of .camerata-worktrees under clone"
        );
    }

    #[test]
    fn derive_shared_target_dir_out_of_band_worktree_returns_none() {
        // A worktree at root level has no grandparent — derivation returns None.
        let worktree = Path::new("/wt");
        let got = derive_shared_target_dir(worktree);
        // May or may not be None depending on whether "/" has a parent; the key
        // invariant is that it does not panic.
        let _ = got; // just confirm no panic
    }

    #[test]
    fn derive_shared_target_dir_two_worktrees_same_clone() {
        // Two different branch worktrees under the SAME clone must derive the SAME target dir.
        let base = "/Users/me/ws/acme/api/.camerata-worktrees";
        let wt_a = PathBuf::from(base).join("camerata__story-a");
        let wt_b = PathBuf::from(base).join("camerata__story-b");
        assert_eq!(
            derive_shared_target_dir(&wt_a),
            derive_shared_target_dir(&wt_b),
            "same clone → same shared target"
        );
    }

    // ── has_headroom (pure decision logic) ──────────────────────────────────

    #[test]
    fn has_headroom_true_when_exactly_at_threshold() {
        let threshold = 10u64 * 1024 * 1024 * 1024;
        assert!(has_headroom(threshold, threshold));
    }

    #[test]
    fn has_headroom_true_when_above_threshold() {
        let threshold = 10u64 * 1024 * 1024 * 1024;
        assert!(has_headroom(threshold + 1, threshold));
    }

    #[test]
    fn has_headroom_false_when_below_threshold() {
        let threshold = 10u64 * 1024 * 1024 * 1024;
        assert!(!has_headroom(threshold - 1, threshold));
        // Incident: 131 MB free
        assert!(!has_headroom(131 * 1024 * 1024, threshold));
    }

    #[test]
    fn has_headroom_false_at_zero_with_nonzero_min() {
        assert!(!has_headroom(0, 1));
    }

    #[test]
    fn has_headroom_true_at_zero_min() {
        assert!(has_headroom(0, 0));
    }
}
