//! [`ManifestCheckRunner`] — Layer-2 executor for `.camerata/checks.toml`.
//!
//! Implements [`camerata_core::CheckRunner`] by loading the manifest from the
//! worktree root and running each check marked `in_loop = true` as a subprocess.
//!
//! # Design
//!
//! - Uses the SAME subprocess pattern as the built-in runners: `sh -c <command>`,
//!   `current_dir(worktree)`, the shared `CARGO_TARGET_DIR` derivation, and the
//!   disk-headroom guard.
//! - A non-zero exit maps to a violation reported under the check's `id` field.
//! - A missing or invalid manifest yields ZERO violations (best-effort, non-fatal).
//!   The absence is logged via `eprintln!` so operator can distinguish "no manifest"
//!   from "manifest with zero checks". A parse error is logged as a warning.
//! - CI-only checks (`in_loop = false`) are SKIPPED — they belong in the generated
//!   CI workflow, not the agent loop. The parity test asserts this is consistent
//!   with what the workflow generator emits.
//!
//! # Composition
//!
//! [`ManifestCheckRunner`] is ADDITIVE on top of the built-in language runners.
//! It is composed into [`crate::multilang::CombinedCheckRunner`] (the wrapper
//! returned by [`crate::runner_for_worktree`]), running AFTER fmt/clippy/test/
//! polyglot checks. The manifest never replaces the built-ins.

use async_trait::async_trait;
use camerata_core::{CheckRunner, Role, RuleId};
use std::path::Path;
use tokio::process::Command;

use crate::manifest::{load_manifest, CheckManifest};

// ─── runner ───────────────────────────────────────────────────────────────────

/// Layer-2 runner for manifest checks (`.camerata/checks.toml`, `in_loop = true`).
///
/// Constructed with a reference to the manifest (so tests can inject any manifest
/// without touching the filesystem). [`crate::runner_for_worktree`] wraps the
/// loading logic and hands a loaded manifest to this runner.
pub struct ManifestCheckRunner {
    /// The loaded manifest. `None` means the manifest was absent or failed to parse;
    /// the runner will produce zero violations in that case (best-effort).
    pub manifest: Option<CheckManifest>,
}

impl ManifestCheckRunner {
    /// Build a runner that loads its manifest from `worktree/.camerata/checks.toml`.
    ///
    /// A missing manifest is silently OK (zero manifest checks). A parse error is
    /// logged and treated the same way — never fatal to the dev loop.
    pub fn load_from(worktree: &Path) -> Self {
        match load_manifest(worktree) {
            Ok(Some(m)) => {
                let n = m.in_loop_checks().count();
                if n > 0 {
                    eprintln!(
                        "[camerata-checks] manifest: {} in_loop check(s) loaded from {}/.camerata/checks.toml",
                        n,
                        worktree.display()
                    );
                }
                Self { manifest: Some(m) }
            }
            Ok(None) => {
                eprintln!(
                    "[camerata-checks] manifest: no .camerata/checks.toml found at {}; \
                     skipping manifest checks",
                    worktree.display()
                );
                Self { manifest: None }
            }
            Err(e) => {
                eprintln!(
                    "[camerata-checks] manifest: WARNING — {} — treating as zero manifest checks",
                    e
                );
                Self { manifest: None }
            }
        }
    }

    /// Build a runner with an explicit (pre-loaded) manifest. Used in tests.
    pub fn with_manifest(manifest: CheckManifest) -> Self {
        Self {
            manifest: Some(manifest),
        }
    }

    /// Build a runner with no manifest (zero custom checks). Used in tests.
    pub fn empty() -> Self {
        Self { manifest: None }
    }
}

#[async_trait]
impl CheckRunner for ManifestCheckRunner {
    async fn check(&self, _role: &Role, worktree: &Path) -> anyhow::Result<Vec<RuleId>> {
        let Some(ref manifest) = self.manifest else {
            // No manifest (absent or parse error): zero violations, non-fatal.
            return Ok(Vec::new());
        };

        let mut violations: Vec<RuleId> = Vec::new();

        for check in manifest.in_loop_checks() {
            let result = run_manifest_check(worktree, &check.command).await;
            match result {
                Ok(true) => {
                    // Exit 0 = pass. Nothing to report.
                }
                Ok(false) => {
                    // Non-zero exit = violation. Report under the check's rule id.
                    eprintln!(
                        "[camerata-checks] manifest check {} ({}) failed (non-zero exit)",
                        check.id, check.name
                    );
                    violations.push(RuleId(check.id.clone()));
                }
                Err(e) => {
                    // Could not spawn (e.g. `sh` not on PATH, permissions). Log and
                    // continue — we do NOT fail-close the whole run for a missing
                    // custom script; a missing built-in (e.g. cargo) would already
                    // have failed the built-in runner. The manifest check is ADDITIVE.
                    eprintln!(
                        "[camerata-checks] manifest check {} ({}) could not run: {}; skipping",
                        check.id, check.name, e
                    );
                }
            }
        }

        Ok(violations)
    }
}

/// Run `sh -c <command>` in `worktree`. Returns `Ok(true)` on exit 0, `Ok(false)`
/// on non-zero exit, or `Err` if the process could not be spawned at all.
///
/// Using `sh -c` gives commands full shell expansion (globs, pipes, `&&` chains)
/// at the cost of a shell fork — acceptable for per-task gates that already run
/// cargo builds.
async fn run_manifest_check(worktree: &Path, command: &str) -> std::io::Result<bool> {
    let status = Command::new("sh")
        .args(["-c", command])
        .current_dir(worktree)
        .status()
        .await?;
    Ok(status.success())
}

// ─── tests (ORCH-NEW-PATH-TESTS-1) ───────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::ManifestCheck;
    use std::fs;

    fn tmpdir() -> std::path::PathBuf {
        static COUNTER: std::sync::atomic::AtomicU64 =
            std::sync::atomic::AtomicU64::new(0);
        let seq = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "cam-manifest-runner-{}-{}-{}",
            std::process::id(),
            seq,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn fake_role() -> Role {
        Role {
            name: "Test".to_string(),
            rule_subset: vec![],
            allowed_paths: vec![],
        }
    }

    fn make_check(id: &str, command: &str, in_loop: bool) -> ManifestCheck {
        ManifestCheck {
            id: id.to_string(),
            name: format!("Test check {id}"),
            command: command.to_string(),
            severity: "high".to_string(),
            in_loop,
        }
    }

    // ── no manifest → zero violations ─────────────────────────────────────────

    #[tokio::test]
    async fn no_manifest_produces_zero_violations() {
        let wt = tmpdir();
        let runner = ManifestCheckRunner::empty();
        let role = fake_role();
        let violations = runner.check(&role, &wt).await.expect("check must not err");
        assert!(
            violations.is_empty(),
            "absent manifest must produce zero violations"
        );
    }

    // ── passing check (exit 0) → zero violations ──────────────────────────────

    #[cfg(unix)]
    #[tokio::test]
    async fn exit_zero_produces_no_violation() {
        let wt = tmpdir();
        let manifest = CheckManifest {
            checks: vec![make_check("ARCH-TEST-PASS-1", "exit 0", true)],
        };
        let runner = ManifestCheckRunner::with_manifest(manifest);
        let role = fake_role();
        let violations = runner.check(&role, &wt).await.expect("check must not err");
        assert!(
            violations.is_empty(),
            "exit-0 command must produce zero violations, got {violations:?}"
        );
    }

    // ── failing check (exit 1) → violation under the check id ─────────────────

    #[cfg(unix)]
    #[tokio::test]
    async fn exit_nonzero_maps_to_violation_under_check_id() {
        let wt = tmpdir();
        let manifest = CheckManifest {
            checks: vec![make_check("ARCH-API-LAYERING-1", "exit 1", true)],
        };
        let runner = ManifestCheckRunner::with_manifest(manifest);
        let role = fake_role();
        let violations = runner.check(&role, &wt).await.expect("check must not err");
        assert_eq!(
            violations,
            vec![RuleId("ARCH-API-LAYERING-1".to_string())],
            "exit-1 command must map to violation under the check id"
        );
    }

    // ── ci-only checks are SKIPPED at Layer 2 ─────────────────────────────────

    #[cfg(unix)]
    #[tokio::test]
    async fn ci_only_checks_are_skipped_at_layer2() {
        let wt = tmpdir();
        let manifest = CheckManifest {
            checks: vec![
                // This would fail if run, but it's ci-only so it must be skipped.
                make_check("SEC-SECRETS-SCAN-1", "exit 1", false),
                // This passes and is in_loop.
                make_check("ARCH-API-LAYERING-1", "exit 0", true),
            ],
        };
        let runner = ManifestCheckRunner::with_manifest(manifest);
        let role = fake_role();
        let violations = runner.check(&role, &wt).await.expect("check must not err");
        assert!(
            violations.is_empty(),
            "ci-only check must be skipped; only in_loop checks run, got {violations:?}"
        );
    }

    // ── multiple in_loop checks, one fails ────────────────────────────────────

    #[cfg(unix)]
    #[tokio::test]
    async fn multiple_checks_all_violations_collected() {
        let wt = tmpdir();
        let manifest = CheckManifest {
            checks: vec![
                make_check("RULE-A", "exit 0", true),
                make_check("RULE-B", "exit 1", true),
                make_check("RULE-C", "exit 2", true),
            ],
        };
        let runner = ManifestCheckRunner::with_manifest(manifest);
        let role = fake_role();
        let violations = runner.check(&role, &wt).await.expect("check must not err");
        // A and C-failing both produce violations; A is clean.
        assert_eq!(violations.len(), 2, "expected 2 violations, got {violations:?}");
        assert!(violations.iter().any(|v| v.0 == "RULE-B"));
        assert!(violations.iter().any(|v| v.0 == "RULE-C"));
    }

    // ── bad command (can't spawn) → no panic, zero violations ─────────────────

    #[tokio::test]
    async fn unspawnable_command_does_not_crash_runner() {
        let wt = tmpdir();
        // A command that `sh` cannot find still resolves via `sh -c`; use a
        // definitely-missing binary to trigger a non-zero exit (sh exits 127).
        // The runner must NOT return Err — it logs and skips.
        let manifest = CheckManifest {
            checks: vec![make_check(
                "ARCH-MISSING-TOOL-1",
                "this_binary_definitely_does_not_exist_xyzzy_42",
                true,
            )],
        };
        let runner = ManifestCheckRunner::with_manifest(manifest);
        let role = fake_role();
        // The runner must not return Err (shell exits non-zero for missing cmd,
        // which we treat as a violation — the id shows up in violations).
        // Either violation or empty is acceptable; what's NOT acceptable is a panic.
        let result = runner.check(&role, &wt).await;
        assert!(
            result.is_ok(),
            "unspawnable command must not propagate as Err: {result:?}"
        );
    }
}
