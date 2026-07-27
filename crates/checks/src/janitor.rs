//! The build-artifact janitor — the disk-buildup guardrail.
//!
//! See `docs/design/2026-07-27_build-artifact-janitor.md` for the full design. This
//! module is the SAFETY CORE: the artifact registry, the five hard deletion-safety
//! invariants, and the reclaim primitives every trigger (T1-T4, `crates/server/src`)
//! is built on. A bug here deletes the wrong files, so every public function here is
//! written to FAIL CLOSED (skip, never delete) on anything ambiguous.
//!
//! # The two-zone model
//!
//! - **Zone A** (Camerata-owned scratch: `.camerata-shared-target/`, orphaned entries
//!   under `.camerata-worktrees/`): fully automatic reclaim. Camerata created these
//!   directories; deleting them can never lose user state.
//! - **Zone B** (the host repo's own `target/`, `node_modules/`, `.venv/`, ...):
//!   MEASURE + report only from this module. [`scan_zone_b`] never deletes; a caller
//!   that wants to act on a Zone-B candidate must call [`reclaim_dir`] itself, which
//!   still runs every safety invariant, and does so ONLY on explicit user opt-in.
//!   Nothing in this module auto-deletes Zone B.
//!
//! # The five hard deletion-safety invariants
//!
//! Every deletion this module performs (Zone A or, if a caller opts in, Zone B) goes
//! through [`evaluate_reclaim`], which enforces, in order:
//!
//! 1. **Registry match** ([`classify_artifact`]) — the path's final segment (or, for
//!    Camerata-owned containers, its PARENT's final segment) must match a
//!    [`builtin_registry`] entry or a repo-declared `extra_artifact_dirs` name.
//! 2. **Gitignore-verified** ([`check_gitignore_status`]) — for anything NOT
//!    Camerata-owned infra, `git check-ignore` must report the path as ignored. A
//!    tracked or plain-untracked-but-not-ignored path is refused.
//! 3. **No symlink following** ([`is_safe_to_reclaim`]) — the path itself must not be
//!    a symlink, and its canonicalized form must resolve INSIDE the canonicalized
//!    root the caller supplies. Either failure refuses the deletion outright.
//! 4. **Logged before deletion** ([`reclaim_dir`]) — every deletion decision is handed
//!    to the caller's `on_log` callback (and emitted via `tracing`) BEFORE
//!    `remove_dir_all` runs, never after.
//! 5. **Rebuild-reversible by construction** — this is what registry membership
//!    (invariant 1) IS: every entry in [`builtin_registry`] names a regenerable build
//!    artifact (a rebuild command recreates it) or a Camerata-managed scratch
//!    directory (a worktree recreates via `git worktree add`; a shared target
//!    recreates on the next `cargo build`). Nothing outside the registry is ever a
//!    reclaim candidate, so nothing that isn't reversible can reach the delete call.
//!
//! A path that fails ANY of 1-3 is skipped and logged as a refusal, never deleted.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::WorktreeLanguage;

// ─── artifact registry ───────────────────────────────────────────────────────

/// Rebuild-cost tier for an artifact directory. Informational (surfaced in the Zone-B
/// report and used to gate "opt-in without re-confirm" per the design's §3), not a
/// safety gate itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactTier {
    /// Rebuild is one command (`cargo build`, `npm run build`, `pytest` re-collecting
    /// `__pycache__`, ...).
    Cheap,
    /// Rebuild is minutes-and-network (`npm install`, a fresh `.venv` + pip resolve).
    Expensive,
}

/// How a registry entry's `dir_name` is matched against a candidate path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpecKind {
    /// The candidate path's OWN final segment must equal `dir_name` (a language
    /// build-artifact directory: `target/`, `node_modules/`, ...).
    ArtifactDir,
    /// The candidate path's PARENT's final segment must equal `dir_name`. Used for
    /// `.camerata-worktrees`, whose CHILDREN are per-branch and unpredictably named
    /// (`camerata__story-7`), so the registry can't name the child directly. A
    /// container match is a NECESSARY but not SUFFICIENT condition for reclaim: the
    /// caller ([`crate::janitor`] Zone-A wiring in `camerata-server`) additionally
    /// verifies the child is not a currently-registered `git worktree` before ever
    /// calling [`reclaim_dir`] on it.
    OrphanContainer,
}

/// One entry in the static, per-language artifact registry.
#[derive(Debug, Clone, Copy)]
pub struct ArtifactSpec {
    /// The directory name matched per `kind`.
    pub dir_name: &'static str,
    /// The language this artifact belongs to (`None` for Camerata-owned infra, which
    /// isn't tied to any one language).
    pub language: Option<WorktreeLanguage>,
    /// How `dir_name` is matched against a candidate path.
    pub kind: SpecKind,
    /// When non-empty, at least one of these filenames must exist as a SIBLING of the
    /// matched directory (i.e. in its parent) for the match to count. Guards against,
    /// e.g., a source directory legitimately named `build/` with no `build.gradle`
    /// beside it — matched only when the JVM marker is actually present.
    pub requires_marker_any: &'static [&'static str],
    /// Rebuild-cost tier.
    pub tier: ArtifactTier,
}

/// The static per-language build-artifact registry.
///
/// Extended two ways (§1 of the design doc):
/// (a) add a row here (compile-time, reviewed like any other rule);
/// (b) a repo adds names via `.camerata/checks.toml`'s `[janitor] extra_artifact_dirs`
///     (see [`crate::manifest::JanitorConfig`]), matched as plain [`SpecKind::ArtifactDir`]
///     names with no marker gate.
///
/// Go, Ruby, and C# are intentionally out of scope for v1 (Go's caches are global, not
/// in-repo; Ruby/C# are noted as v1.1 in the design doc) — a repo in one of those
/// languages can still opt in via `extra_artifact_dirs`.
pub fn builtin_registry() -> &'static [ArtifactSpec] {
    const REGISTRY: &[ArtifactSpec] = &[
        // ── Rust ──────────────────────────────────────────────────────────────
        ArtifactSpec {
            dir_name: "target",
            language: Some(WorktreeLanguage::Rust),
            kind: SpecKind::ArtifactDir,
            requires_marker_any: &[],
            tier: ArtifactTier::Cheap,
        },
        // ── Camerata-owned infra (Zone A) ───────────────────────────────────────
        ArtifactSpec {
            dir_name: ".camerata-shared-target",
            language: None,
            kind: SpecKind::ArtifactDir,
            requires_marker_any: &[],
            tier: ArtifactTier::Cheap,
        },
        ArtifactSpec {
            dir_name: ".camerata-worktrees",
            language: None,
            kind: SpecKind::OrphanContainer,
            requires_marker_any: &[],
            tier: ArtifactTier::Cheap,
        },
        // ── JS/TS ────────────────────────────────────────────────────────────
        ArtifactSpec {
            dir_name: "node_modules",
            language: Some(WorktreeLanguage::JavaScript),
            kind: SpecKind::ArtifactDir,
            requires_marker_any: &[],
            tier: ArtifactTier::Expensive,
        },
        ArtifactSpec {
            dir_name: "dist",
            language: Some(WorktreeLanguage::JavaScript),
            kind: SpecKind::ArtifactDir,
            requires_marker_any: &["package.json"],
            tier: ArtifactTier::Cheap,
        },
        ArtifactSpec {
            dir_name: ".next",
            language: Some(WorktreeLanguage::JavaScript),
            kind: SpecKind::ArtifactDir,
            requires_marker_any: &["package.json"],
            tier: ArtifactTier::Cheap,
        },
        ArtifactSpec {
            dir_name: ".turbo",
            language: Some(WorktreeLanguage::JavaScript),
            kind: SpecKind::ArtifactDir,
            requires_marker_any: &["package.json"],
            tier: ArtifactTier::Cheap,
        },
        ArtifactSpec {
            dir_name: ".vite",
            language: Some(WorktreeLanguage::JavaScript),
            kind: SpecKind::ArtifactDir,
            requires_marker_any: &["package.json"],
            tier: ArtifactTier::Cheap,
        },
        ArtifactSpec {
            dir_name: "coverage",
            language: Some(WorktreeLanguage::JavaScript),
            kind: SpecKind::ArtifactDir,
            requires_marker_any: &["package.json"],
            tier: ArtifactTier::Cheap,
        },
        // `build/` is ALSO a JS artifact dir (create-react-app et al) — always
        // marker-gated (JS's own `dist`/`.next` marker set is loose enough that an
        // unmarked `build/` is too easily a plain source directory).
        ArtifactSpec {
            dir_name: "build",
            language: Some(WorktreeLanguage::JavaScript),
            kind: SpecKind::ArtifactDir,
            requires_marker_any: &["package.json"],
            tier: ArtifactTier::Cheap,
        },
        // ── Python ───────────────────────────────────────────────────────────
        ArtifactSpec {
            dir_name: "__pycache__",
            language: Some(WorktreeLanguage::Python),
            kind: SpecKind::ArtifactDir,
            requires_marker_any: &[],
            tier: ArtifactTier::Cheap,
        },
        ArtifactSpec {
            dir_name: ".pytest_cache",
            language: Some(WorktreeLanguage::Python),
            kind: SpecKind::ArtifactDir,
            requires_marker_any: &[],
            tier: ArtifactTier::Cheap,
        },
        ArtifactSpec {
            dir_name: ".mypy_cache",
            language: Some(WorktreeLanguage::Python),
            kind: SpecKind::ArtifactDir,
            requires_marker_any: &[],
            tier: ArtifactTier::Cheap,
        },
        ArtifactSpec {
            dir_name: ".ruff_cache",
            language: Some(WorktreeLanguage::Python),
            kind: SpecKind::ArtifactDir,
            requires_marker_any: &[],
            tier: ArtifactTier::Cheap,
        },
        ArtifactSpec {
            dir_name: ".tox",
            language: Some(WorktreeLanguage::Python),
            kind: SpecKind::ArtifactDir,
            requires_marker_any: &[],
            tier: ArtifactTier::Cheap,
        },
        ArtifactSpec {
            dir_name: ".venv",
            language: Some(WorktreeLanguage::Python),
            kind: SpecKind::ArtifactDir,
            requires_marker_any: &[],
            tier: ArtifactTier::Expensive,
        },
        // ── JVM ──────────────────────────────────────────────────────────────
        // Gradle's `build/` — marker-gated so a source dir named `build/` is never
        // misclassified.
        ArtifactSpec {
            dir_name: "build",
            language: Some(WorktreeLanguage::Java),
            kind: SpecKind::ArtifactDir,
            requires_marker_any: &["build.gradle", "build.gradle.kts"],
            tier: ArtifactTier::Cheap,
        },
        ArtifactSpec {
            dir_name: ".gradle",
            language: Some(WorktreeLanguage::Java),
            kind: SpecKind::ArtifactDir,
            requires_marker_any: &["build.gradle", "build.gradle.kts"],
            tier: ArtifactTier::Cheap,
        },
        // Maven's `target/` — marker-gated on `pom.xml` (distinct entry from Rust's
        // unmarked `target/`; a repo can match either or both depending on which
        // marker is present beside it).
        ArtifactSpec {
            dir_name: "target",
            language: Some(WorktreeLanguage::Java),
            kind: SpecKind::ArtifactDir,
            requires_marker_any: &["pom.xml"],
            tier: ArtifactTier::Cheap,
        },
    ];
    REGISTRY
}

/// A registry (or repo-declared extra) match for a candidate path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactMatch {
    pub dir_name: String,
    pub tier: ArtifactTier,
}

/// Classify `path` against the [`builtin_registry`] plus any repo-declared
/// `extra_dirs`. Returns `None` when nothing matches — the path is NOT a reclaim
/// candidate (invariant 1 fails).
///
/// `extra_dirs` entries match as plain [`SpecKind::ArtifactDir`] names (the path's own
/// final segment), unconditionally `Cheap` tier, no marker gate — a deliberately
/// simple v1 extension surface (see `.camerata/checks.toml`'s `[janitor]` table).
pub fn classify_artifact(path: &Path, extra_dirs: &[String]) -> Option<ArtifactMatch> {
    let file_name = path.file_name()?.to_string_lossy().to_string();

    for spec in builtin_registry() {
        match spec.kind {
            SpecKind::ArtifactDir => {
                if file_name != spec.dir_name {
                    continue;
                }
                if !spec.requires_marker_any.is_empty() {
                    let parent = path.parent()?;
                    let satisfied = spec
                        .requires_marker_any
                        .iter()
                        .any(|marker| parent.join(marker).is_file());
                    if !satisfied {
                        continue;
                    }
                }
                return Some(ArtifactMatch {
                    dir_name: spec.dir_name.to_string(),
                    tier: spec.tier,
                });
            }
            SpecKind::OrphanContainer => {
                let Some(parent) = path.parent() else {
                    continue;
                };
                let Some(parent_name) = parent.file_name() else {
                    continue;
                };
                if parent_name.to_string_lossy() == spec.dir_name {
                    return Some(ArtifactMatch {
                        dir_name: spec.dir_name.to_string(),
                        tier: spec.tier,
                    });
                }
            }
        }
    }

    if extra_dirs.iter().any(|d| d == &file_name) {
        return Some(ArtifactMatch {
            dir_name: file_name,
            tier: ArtifactTier::Cheap,
        });
    }

    None
}

// ─── safety invariants ───────────────────────────────────────────────────────

/// Why a path was refused reclamation. Every variant means "skipped, never deleted".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefusalReason {
    /// Invariant 1: the path matched no registry entry / `extra_artifact_dirs` name.
    NoRegistryMatch,
    /// Invariant 3: the path itself is a symlink. Never followed, never deleted —
    /// even if its target would otherwise be a valid candidate.
    IsSymlink,
    /// Invariant 3: the path's canonicalized form resolves OUTSIDE the canonicalized
    /// root the caller supplied (a symlinked ancestor escaping the intended root, or a
    /// plain `..`-style escape).
    ResolvesOutsideRoot,
    /// Invariant 2: `git check-ignore` reports the path as tracked / not ignored.
    NotGitignored,
    /// Invariant 2: gitignore status could not be verified (not inside a git repo, and
    /// not a recognized Camerata-owned infra path) — fail closed rather than guess.
    CannotVerifyGitignore(String),
    /// A filesystem race or I/O error made the path impossible to evaluate safely
    /// (e.g. it vanished mid-walk). Never treated as "safe by default".
    IoError(String),
}

impl std::fmt::Display for RefusalReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RefusalReason::NoRegistryMatch => {
                write!(f, "path does not match the build-artifact registry")
            }
            RefusalReason::IsSymlink => write!(f, "path is a symlink — never followed or deleted"),
            RefusalReason::ResolvesOutsideRoot => {
                write!(f, "path resolves outside its expected root (possible symlink escape)")
            }
            RefusalReason::NotGitignored => {
                write!(f, "path is tracked or not gitignored — refusing to delete")
            }
            RefusalReason::CannotVerifyGitignore(detail) => {
                write!(f, "could not verify gitignore status: {detail}")
            }
            RefusalReason::IoError(detail) => write!(f, "I/O error evaluating path: {detail}"),
        }
    }
}

/// Invariant 3 (no symlink following): `path` itself must not be a symlink, and its
/// canonicalized form must resolve INSIDE the canonicalized `root`. Either failure is
/// a hard refusal — this is checked BEFORE registry matching so a symlink escape can
/// never be masked by an otherwise-valid-looking name.
///
/// Never panics: an unreadable/vanished path is treated as unsafe (`Err`), not skipped
/// silently as safe.
pub fn is_safe_to_reclaim(path: &Path, root: &Path) -> Result<(), RefusalReason> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => return Err(RefusalReason::IsSymlink),
        Ok(_) => {}
        Err(e) => return Err(RefusalReason::IoError(format!("{}: {e}", path.display()))),
    }

    let canonical_path = std::fs::canonicalize(path)
        .map_err(|e| RefusalReason::IoError(format!("{}: {e}", path.display())))?;
    let canonical_root = std::fs::canonicalize(root)
        .map_err(|e| RefusalReason::IoError(format!("{}: {e}", root.display())))?;

    if !canonical_path.starts_with(&canonical_root) {
        return Err(RefusalReason::ResolvesOutsideRoot);
    }
    Ok(())
}

/// The outcome of checking whether `git` considers a path ignored.
#[derive(Debug, Clone, PartialEq, Eq)]
enum GitIgnoreStatus {
    Ignored,
    NotIgnored,
    /// `git check-ignore` failed for a reason OTHER than "not ignored" (not a git
    /// repo, git not installed, etc.) — carries a human detail string.
    Unverifiable(String),
}

/// Run `git check-ignore --quiet <path>` with CWD set to `path`'s parent (so git can
/// discover the repo upward from there). A blocking, synchronous call — this only
/// runs on the rare reclaim path (headroom shortfall / cap breach), never on the hot
/// request path, matching this crate's existing `fs2`-in-a-sync-fn convention for the
/// disk-headroom guard.
fn check_gitignore_status(path: &Path) -> GitIgnoreStatus {
    let Some(parent) = path.parent() else {
        return GitIgnoreStatus::Unverifiable("path has no parent directory".to_string());
    };
    let Some(file_name) = path.file_name() else {
        return GitIgnoreStatus::Unverifiable("path has no file name".to_string());
    };

    let output = std::process::Command::new("git")
        .current_dir(parent)
        .args(["check-ignore", "--quiet", &file_name.to_string_lossy()])
        .output();

    match output {
        Ok(out) => match out.status.code() {
            Some(0) => GitIgnoreStatus::Ignored,
            Some(1) => GitIgnoreStatus::NotIgnored,
            other => {
                let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
                GitIgnoreStatus::Unverifiable(if stderr.is_empty() {
                    format!("git check-ignore exited {other:?}")
                } else {
                    stderr
                })
            }
        },
        Err(e) => GitIgnoreStatus::Unverifiable(format!("couldn't run git: {e}")),
    }
}

/// Run every safety invariant (1-3) for `path` and, on success, return the registry
/// match. This is the single seam [`reclaim_dir`] uses before ever calling
/// `remove_dir_all` — nothing in this crate deletes a path that hasn't passed through
/// here.
///
/// `root` bounds where `path` is allowed to resolve to (invariant 3). `extra_dirs` are
/// the repo-declared `extra_artifact_dirs` (invariant 1). `is_camerata_infra` is `true`
/// ONLY for well-known Camerata-owned scratch paths (`.camerata-shared-target`,
/// orphaned entries under `.camerata-worktrees`) — these skip the gitignore check
/// (invariant 2) because they are structurally outside every per-UoW worktree's git
/// view (see the module doc), NOT because we trust the caller's say-so on an
/// arbitrary path. Every OTHER invariant still applies unconditionally.
pub fn evaluate_reclaim(
    path: &Path,
    root: &Path,
    extra_dirs: &[String],
    is_camerata_infra: bool,
) -> Result<ArtifactMatch, RefusalReason> {
    // Invariant 3 first: a symlink escape must never be masked by a plausible name.
    is_safe_to_reclaim(path, root)?;

    // Invariant 1: must match the registry (or a repo-declared extra).
    let matched = classify_artifact(path, extra_dirs).ok_or(RefusalReason::NoRegistryMatch)?;

    // Invariant 2: gitignore-verified, unless this is a recognized Camerata-owned
    // infra path (structurally invisible to git; see module doc).
    if !is_camerata_infra {
        match check_gitignore_status(path) {
            GitIgnoreStatus::Ignored => {}
            GitIgnoreStatus::NotIgnored => return Err(RefusalReason::NotGitignored),
            GitIgnoreStatus::Unverifiable(detail) => {
                return Err(RefusalReason::CannotVerifyGitignore(detail))
            }
        }
    }

    Ok(matched)
}

// ─── logged, invariant-checked deletion ──────────────────────────────────────

/// One reclaim decision, always constructed and handed to the caller's `on_log`
/// callback BEFORE any deletion happens (invariant 4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReclaimLogEntry {
    pub path: PathBuf,
    pub dir_name: String,
    pub tier: ArtifactTier,
    pub bytes: u64,
    pub trigger: String,
    pub timestamp: SystemTime,
    pub dry_run: bool,
}

/// The result of one [`reclaim_dir`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReclaimOutcome {
    /// Reclaimed (or, in dry-run mode, WOULD have been reclaimed — see
    /// [`ReclaimLogEntry::dry_run`]). Carries the log entry that was emitted before
    /// deletion.
    Reclaimed(ReclaimLogEntry),
    /// Skipped — one of the safety invariants refused this path. NEVER deleted.
    Skipped { path: PathBuf, reason: RefusalReason },
}

/// Recursively compute the total size of everything under `path`, in bytes.
///
/// Best-effort and NEVER panics: an unreadable subtree (permission denied) or a file
/// that vanishes mid-walk (race with another process) is simply skipped (contributes
/// 0), not treated as an error. This mirrors the existing manifest-walk convention in
/// [`crate::multilang::walk_for_manifests`].
pub fn dir_size(path: &Path) -> u64 {
    let entries = match std::fs::read_dir(path) {
        Ok(e) => e,
        Err(_) => return 0,
    };
    let mut total = 0u64;
    for entry in entries.flatten() {
        let file_type = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };
        if file_type.is_symlink() {
            // Never follow a symlink even for measurement — a symlinked entry inside
            // an artifact dir must not cause us to walk (or count) something outside
            // the artifact tree.
            continue;
        }
        if file_type.is_dir() {
            total = total.saturating_add(dir_size(&entry.path()));
        } else {
            total = total.saturating_add(entry.metadata().map(|m| m.len()).unwrap_or(0));
        }
    }
    total
}

/// Evaluate every safety invariant for `path` and, if it passes, delete it — LOGGING
/// the decision (via `on_log` and `tracing`) BEFORE the deletion happens (invariant
/// 4). A path that fails any invariant is returned as [`ReclaimOutcome::Skipped`] and
/// is NEVER deleted.
///
/// `dry_run` still computes size and calls `on_log` (so a dry-run inventory is fully
/// populated) but skips the actual `remove_dir_all`.
///
/// Deletion itself is best-effort: a `remove_dir_all` failure (permission denied, or
/// the path vanished between the invariant check and the delete) is swallowed rather
/// than panicking or propagating — the caller already has the log entry recording
/// what was ATTEMPTED, and a failed delete leaves the filesystem exactly as it was
/// (never a partial, inconsistent state worse than doing nothing).
pub fn reclaim_dir(
    path: &Path,
    root: &Path,
    extra_dirs: &[String],
    is_camerata_infra: bool,
    trigger: &str,
    dry_run: bool,
    mut on_log: impl FnMut(&ReclaimLogEntry),
) -> ReclaimOutcome {
    let matched = match evaluate_reclaim(path, root, extra_dirs, is_camerata_infra) {
        Ok(m) => m,
        Err(reason) => {
            tracing::debug!(
                path = %path.display(),
                reason = %reason,
                "janitor: skipped (not reclaimed)"
            );
            return ReclaimOutcome::Skipped {
                path: path.to_path_buf(),
                reason,
            };
        }
    };

    let bytes = dir_size(path);
    let entry = ReclaimLogEntry {
        path: path.to_path_buf(),
        dir_name: matched.dir_name,
        tier: matched.tier,
        bytes,
        trigger: trigger.to_string(),
        timestamp: SystemTime::now(),
        dry_run,
    };

    // Invariant 4: logged BEFORE deletion, unconditionally (including dry-run).
    on_log(&entry);
    tracing::info!(
        path = %entry.path.display(),
        bytes = entry.bytes,
        trigger = %entry.trigger,
        dry_run = entry.dry_run,
        "janitor: reclaiming build artifact"
    );

    if !dry_run {
        // Best-effort: a failed delete (permission, or vanished mid-race) leaves the
        // filesystem unchanged; never escalate to a panic or propagate an error that
        // would abort an otherwise-successful sweep of other candidates.
        let _ = std::fs::remove_dir_all(path);
    }

    ReclaimOutcome::Reclaimed(entry)
}

// ─── Zone A: pure prune-policy decisions (shared-target cap/age) ────────────

/// T2 policy: the shared target exceeds its size cap.
pub fn shared_target_over_cap(bytes: u64, max_bytes: u64) -> bool {
    bytes > max_bytes
}

/// T2 policy: no worktree is currently using this clone's shared target — safe to
/// reclaim regardless of size (v1 simplification per the design doc: "whole-dir
/// removal ... when no worktrees remain for that clone").
pub fn shared_target_orphaned(has_live_worktrees: bool) -> bool {
    !has_live_worktrees
}

/// T2 policy: the shared target is stale (untouched past `max_age`) AND has no live
/// worktrees. Kept as its own named predicate (even though
/// [`shared_target_orphaned`] alone already implies reclaim) so the "14-day default"
/// config knob has a directly-testable decision function of its own.
pub fn shared_target_stale(age: Duration, max_age: Duration, has_live_worktrees: bool) -> bool {
    !has_live_worktrees && age > max_age
}

/// Combined T2 decision: prune the shared target when ANY policy fires.
pub fn should_prune_shared_target(
    bytes: u64,
    max_bytes: u64,
    age: Duration,
    max_age: Duration,
    has_live_worktrees: bool,
) -> bool {
    shared_target_over_cap(bytes, max_bytes)
        || shared_target_orphaned(has_live_worktrees)
        || shared_target_stale(age, max_age, has_live_worktrees)
}

// ─── Zone A: orphan-worktree detection ───────────────────────────────────────

/// List the ABSOLUTE, canonicalized worktree paths `git worktree list --porcelain`
/// currently reports for `clone`. Best-effort: any failure (not a repo, `git` not on
/// PATH) yields an empty list, which callers must treat conservatively — see
/// [`list_orphan_worktree_dirs`], which only ever REMOVES a candidate when this list
/// query itself succeeded and is non-suspect (empty-because-error is indistinguishable
/// from empty-because-no-worktrees at this layer, so the caller additionally requires
/// `clone` to be a valid git repo before treating anything as orphaned).
fn registered_worktree_paths(clone: &Path) -> Vec<PathBuf> {
    let output = std::process::Command::new("git")
        .current_dir(clone)
        .args(["worktree", "list", "--porcelain"])
        .output();
    let Ok(out) = output else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines()
        .filter_map(|line| line.strip_prefix("worktree "))
        .map(|p| {
            let raw = PathBuf::from(p.trim());
            std::fs::canonicalize(&raw).unwrap_or(raw)
        })
        .collect()
}

/// Find subdirectories of `<clone>/.camerata-worktrees` that are NOT currently
/// registered as a `git worktree` — the T3 orphan-sweep candidate set (crashed runs,
/// leftovers from before per-stage teardown existed, etc.).
///
/// Conservative by construction: returns an empty list unless `clone` is confirmed to
/// be a git repo (so a `git worktree list` failure never gets misread as "zero
/// worktrees, everything is an orphan"). Each returned path still has to pass
/// [`evaluate_reclaim`] before anything deletes it — this function only narrows the
/// CANDIDATE set, it is not itself a safety invariant.
pub fn list_orphan_worktree_dirs(clone: &Path) -> Vec<PathBuf> {
    if !clone.join(".git").exists() {
        return Vec::new();
    }
    let worktrees_dir = clone.join(".camerata-worktrees");
    let Ok(entries) = std::fs::read_dir(&worktrees_dir) else {
        return Vec::new();
    };
    let registered: HashSet<PathBuf> = registered_worktree_paths(clone).into_iter().collect();

    entries
        .flatten()
        .filter(|entry| entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false))
        .map(|entry| entry.path())
        .filter(|path| {
            let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.clone());
            !registered.contains(&canonical)
        })
        .collect()
}

/// Whether `clone` currently has at least one registered worktree under
/// `.camerata-worktrees` (used by the T2 "no live worktrees" policy). Best-effort:
/// a `git worktree list` failure is treated as "has live worktrees" (fail CLOSED —
/// the conservative direction here is to NOT prune, not to prune eagerly).
pub fn has_live_worktrees(clone: &Path) -> bool {
    let output = std::process::Command::new("git")
        .current_dir(clone)
        .args(["worktree", "list", "--porcelain"])
        .output();
    let Ok(out) = output else {
        return true; // fail closed: assume live worktrees exist, don't prune
    };
    if !out.status.success() {
        return true;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let worktrees_root = clone.join(".camerata-worktrees");
    let canonical_root = std::fs::canonicalize(&worktrees_root).unwrap_or(worktrees_root);
    text.lines()
        .filter_map(|line| line.strip_prefix("worktree "))
        .map(|p| {
            let raw = PathBuf::from(p.trim());
            std::fs::canonicalize(&raw).unwrap_or(raw)
        })
        .any(|p| p.starts_with(&canonical_root))
}

// ─── Zone A: one-clone reclaim (used by T2/T3/T4 wiring in camerata-server) ──

/// Reclaim everything currently eligible in `clone`'s Zone A scratch: orphaned
/// `.camerata-worktrees/*` entries, and — when `prune_shared_target` is `true` (the
/// caller has already decided via [`should_prune_shared_target`] or an emergency
/// headroom shortfall) — the whole `.camerata-shared-target` directory.
///
/// Returns the total bytes reclaimed (0 in dry-run mode, since nothing is actually
/// deleted, or if nothing was eligible). Every individual decision still goes through
/// [`reclaim_dir`] (so `on_log` sees every attempt, reclaimed or skipped) before
/// anything is deleted.
pub fn reclaim_zone_a_for_clone(
    clone: &Path,
    prune_shared_target: bool,
    trigger: &str,
    dry_run: bool,
    mut on_log: impl FnMut(&ReclaimLogEntry),
) -> u64 {
    let mut total = 0u64;

    let worktrees_root = clone.join(".camerata-worktrees");
    for orphan in list_orphan_worktree_dirs(clone) {
        if let ReclaimOutcome::Reclaimed(entry) =
            reclaim_dir(&orphan, &worktrees_root, &[], true, trigger, dry_run, &mut on_log)
        {
            total = total.saturating_add(entry.bytes);
        }
    }

    if prune_shared_target {
        let shared = clone.join(".camerata-shared-target");
        if shared.is_dir() {
            if let ReclaimOutcome::Reclaimed(entry) =
                reclaim_dir(&shared, clone, &[], true, trigger, dry_run, &mut on_log)
            {
                total = total.saturating_add(entry.bytes);
            }
        }
    }

    total
}

// ─── Zone B: measure + report only, NEVER auto-delete ────────────────────────

/// One Zone-B (host repo) candidate: a registry-matched, gitignore-verified,
/// symlink-safe artifact directory that COULD be reclaimed, sized so a warn message
/// can name it. Nothing in this module deletes a [`ZoneBCandidate`] automatically —
/// see the module doc.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZoneBCandidate {
    pub path: PathBuf,
    pub dir_name: String,
    pub tier: ArtifactTier,
    pub bytes: u64,
}

/// Directory names never descended into while scanning for Zone-B candidates,
/// regardless of registry match (VCS metadata; scanning inside would be slow and
/// pointless).
const ZONE_B_ALWAYS_SKIP: &[&str] = &[".git"];

/// Scan `root` (a repo or worktree directory) for Zone-B reclaim candidates:
/// registry-matched artifact directories that are ALSO gitignore-verified and
/// symlink-safe (i.e. would pass [`evaluate_reclaim`] with `is_camerata_infra =
/// false`). This is measurement + reporting ONLY — nothing here deletes anything. A
/// caller (the headroom-guard message, a UI surface, a one-click reclaim endpoint)
/// decides whether/when to act on a returned candidate by calling [`reclaim_dir`]
/// itself, which re-runs the full invariant chain.
///
/// Symlinked directories are never descended into (mirrors [`dir_size`]). A matched
/// artifact directory is not recursed into further (it's a leaf candidate, not a
/// subtree to keep scanning inside). Results are sorted largest-first so a warn
/// message can lead with the biggest offender.
pub fn scan_zone_b(root: &Path, extra_dirs: &[String]) -> Vec<ZoneBCandidate> {
    let mut out = Vec::new();
    scan_zone_b_walk(root, root, extra_dirs, &mut out);
    out.sort_by(|a, b| b.bytes.cmp(&a.bytes));
    out
}

fn scan_zone_b_walk(dir: &Path, root: &Path, extra_dirs: &[String], out: &mut Vec<ZoneBCandidate>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        // Never follow symlinks, even for measurement/reporting.
        if file_type.is_symlink() {
            continue;
        }
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if ZONE_B_ALWAYS_SKIP.contains(&name_str.as_ref()) {
            continue;
        }
        if classify_artifact(&path, extra_dirs).is_some() {
            // Only report it as a real candidate if it would actually pass every
            // safety invariant (gitignore-verified, resolves inside root, etc.) —
            // otherwise it's not a genuine remedy to suggest.
            if evaluate_reclaim(&path, root, extra_dirs, false).is_ok() {
                if let Some(matched) = classify_artifact(&path, extra_dirs) {
                    let bytes = dir_size(&path);
                    out.push(ZoneBCandidate {
                        path,
                        dir_name: matched.dir_name,
                        tier: matched.tier,
                        bytes,
                    });
                }
            }
            // Either way, don't recurse further into a matched artifact directory.
            continue;
        }
        scan_zone_b_walk(&path, root, extra_dirs, out);
    }
}

/// Render a short, human-readable inventory line for a headroom-guard message: the
/// top `limit` candidates by size, with a named remedy. Empty when there are no
/// candidates.
pub fn format_zone_b_inventory(candidates: &[ZoneBCandidate], limit: usize) -> String {
    if candidates.is_empty() {
        return String::new();
    }
    let mut lines = vec!["your repo's own build artifacts (never auto-deleted):".to_string()];
    for c in candidates.iter().take(limit) {
        let gb = c.bytes as f64 / (1024.0 * 1024.0 * 1024.0);
        let tier = match c.tier {
            ArtifactTier::Cheap => "cheap rebuild",
            ArtifactTier::Expensive => "expensive rebuild",
        };
        lines.push(format!(
            "  - {} ({gb:.1} GB, {tier}) — remove with `rm -rf {}`",
            c.path.display(),
            c.path.display()
        ));
    }
    lines.join("\n")
}

// ─── Zone A: reclaim-then-recheck-then-block orchestration (T4) ─────────────

/// The outcome of [`check_headroom_with_reclaim`] — the pure, injectable-seam version
/// of the T4 "reclaim → re-check → block" guard upgrade.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeadroomOutcome {
    /// The disk-free query itself failed — fail-open (unchanged from the pre-janitor
    /// guard: better to attempt the operation than block spuriously).
    CannotQuery,
    /// Headroom was already sufficient; no reclaim was attempted.
    Sufficient { available: u64 },
    /// Headroom was short, Zone-A reclaim ran, and it's now sufficient.
    ReclaimedSufficient { available: u64, reclaimed_bytes: u64 },
    /// Headroom was short, Zone-A reclaim ran, and it's STILL short — the caller
    /// should bail with an actionable message (Zone-B inventory).
    StillInsufficient { available: u64, reclaimed_bytes: u64 },
}

/// Pure orchestration of the T4 guard upgrade: query headroom, and ONLY if it's short,
/// run `reclaim` once and re-query. Both queries and the reclaim step are injected
/// closures so this is fully unit-testable without a real disk or filesystem — tests
/// drive it with canned `available` sequences and a counting `reclaim` stub, the same
/// "has_headroom-style seam" the pre-existing pure `has_headroom` function used.
///
/// Real callers wrap this with `available_disk_bytes` (the real `fs2` query) and a
/// `reclaim` closure that calls [`reclaim_zone_a_for_clone`].
pub fn check_headroom_with_reclaim(
    min: u64,
    mut available_disk_bytes: impl FnMut() -> Option<u64>,
    mut reclaim: impl FnMut() -> u64,
) -> HeadroomOutcome {
    let Some(before) = available_disk_bytes() else {
        return HeadroomOutcome::CannotQuery;
    };
    if before >= min {
        return HeadroomOutcome::Sufficient { available: before };
    }

    let reclaimed_bytes = reclaim();

    let Some(after) = available_disk_bytes() else {
        return HeadroomOutcome::CannotQuery;
    };
    if after >= min {
        HeadroomOutcome::ReclaimedSufficient {
            available: after,
            reclaimed_bytes,
        }
    } else {
        HeadroomOutcome::StillInsufficient {
            available: after,
            reclaimed_bytes,
        }
    }
}

// ─── config parsing (extends the CAMERATA_MIN_DISK_HEADROOM_GB family) ──────

/// The janitor's on/off/dry-run kill switch (`CAMERATA_JANITOR`). Zone A only — Zone B
/// is never automatic regardless of this setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JanitorMode {
    On,
    DryRun,
    Off,
}

/// Parse the `CAMERATA_JANITOR` env var (`"on"` / `"dry-run"` / `"off"`), defaulting to
/// `On`. Unrecognized values also default to `On` (fail-open on the KILL SWITCH means
/// "keep the existing reclaim behavior", which is the conservative choice for a
/// disk-safety feature — silently going to `Off` on a typo would reintroduce the
/// incident this module exists to prevent).
pub fn parse_janitor_mode(raw: Option<&str>) -> JanitorMode {
    match raw.map(|s| s.trim().to_ascii_lowercase()) {
        Some(ref s) if s == "off" => JanitorMode::Off,
        Some(ref s) if s == "dry-run" || s == "dry_run" || s == "dryrun" => JanitorMode::DryRun,
        _ => JanitorMode::On,
    }
}

/// Read `CAMERATA_JANITOR` from the environment.
pub fn janitor_mode() -> JanitorMode {
    parse_janitor_mode(std::env::var("CAMERATA_JANITOR").ok().as_deref())
}

/// Parse a GB-valued env var into bytes, defaulting to `default_gb` GiB when absent or
/// invalid. Mirrors [`crate::parse_disk_headroom_gb`]'s existing pattern exactly (same
/// family, not a parallel config system).
pub fn parse_gb_env(raw: Option<&str>, default_gb: u64) -> u64 {
    raw.and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(default_gb)
        * 1024
        * 1024
        * 1024
}

/// `CAMERATA_SHARED_TARGET_MAX_GB` (default 30 GB) — the T2 shared-target size cap.
pub fn shared_target_max_bytes() -> u64 {
    parse_gb_env(std::env::var("CAMERATA_SHARED_TARGET_MAX_GB").ok().as_deref(), 30)
}

/// Parse a day-valued env var into a [`Duration`], defaulting to `default_days` when
/// absent or invalid.
pub fn parse_days_env(raw: Option<&str>, default_days: u64) -> Duration {
    let days = raw
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(default_days);
    Duration::from_secs(days * 24 * 60 * 60)
}

/// `CAMERATA_ARTIFACT_MAX_AGE_DAYS` (default 14 days) — the T2/T3 staleness cutoff.
pub fn artifact_max_age() -> Duration {
    parse_days_env(std::env::var("CAMERATA_ARTIFACT_MAX_AGE_DAYS").ok().as_deref(), 14)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::symlink;

    fn tmpdir(label: &str) -> PathBuf {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let seq = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "cam-janitor-test-{label}-{}-{}-{}",
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

    fn init_git_repo(dir: &Path) {
        let status = std::process::Command::new("git")
            .current_dir(dir)
            .args(["init", "-q"])
            .status()
            .expect("git init");
        assert!(status.success());
        // A repo-scoped identity so tests never depend on global git config.
        std::process::Command::new("git")
            .current_dir(dir)
            .args(["config", "user.email", "test@example.com"])
            .status()
            .unwrap();
        std::process::Command::new("git")
            .current_dir(dir)
            .args(["config", "user.name", "Test"])
            .status()
            .unwrap();
    }

    fn git(dir: &Path, args: &[&str]) -> std::process::Output {
        std::process::Command::new("git")
            .current_dir(dir)
            .args(args)
            .output()
            .expect("git command")
    }

    // ── classify_artifact (registry matching, invariant 1) ──────────────────────

    #[test]
    fn classify_rust_target_matches_no_marker_needed() {
        let root = tmpdir("rust-target");
        let target = root.join("target");
        fs::create_dir_all(&target).unwrap();
        let m = classify_artifact(&target, &[]).expect("target/ must match Rust spec");
        assert_eq!(m.dir_name, "target");
        assert_eq!(m.tier, ArtifactTier::Cheap);
    }

    #[test]
    fn classify_node_modules_is_expensive_tier() {
        let root = tmpdir("node-modules");
        let nm = root.join("node_modules");
        fs::create_dir_all(&nm).unwrap();
        let m = classify_artifact(&nm, &[]).expect("node_modules must match");
        assert_eq!(m.tier, ArtifactTier::Expensive);
    }

    #[test]
    fn classify_gradle_build_requires_marker() {
        let root = tmpdir("gradle-build");
        let build = root.join("build");
        fs::create_dir_all(&build).unwrap();
        // No build.gradle marker present — a source dir literally named `build/`
        // must NOT be classified as an artifact.
        assert!(
            classify_artifact(&build, &[]).is_none(),
            "unmarked build/ must not match any spec"
        );

        fs::write(root.join("build.gradle"), "// gradle").unwrap();
        let m = classify_artifact(&build, &[]).expect("build/ with build.gradle sibling matches");
        assert_eq!(m.dir_name, "build");
    }

    #[test]
    fn classify_maven_target_requires_pom_marker() {
        let root = tmpdir("maven-target");
        let target = root.join("target");
        fs::create_dir_all(&target).unwrap();
        // Rust's unmarked target spec ALSO matches unconditionally, so this case
        // alone can't prove marker-gating; the meaningful assertion is that the
        // match still succeeds when ONLY the pom.xml marker exists (no Cargo.toml),
        // proving the Maven spec's own marker gate is satisfied independently.
        fs::write(root.join("pom.xml"), "<project/>").unwrap();
        let m = classify_artifact(&target, &[]).expect("target/ with pom.xml matches Maven spec");
        assert_eq!(m.dir_name, "target");
    }

    #[test]
    fn classify_extra_artifact_dirs_matches_repo_declared_name() {
        let root = tmpdir("extra-dirs");
        let custom = root.join("my-custom-cache");
        fs::create_dir_all(&custom).unwrap();
        assert!(classify_artifact(&custom, &[]).is_none());
        let extra = vec!["my-custom-cache".to_string()];
        let m = classify_artifact(&custom, &extra).expect("extra_dirs name must match");
        assert_eq!(m.dir_name, "my-custom-cache");
        assert_eq!(m.tier, ArtifactTier::Cheap);
    }

    #[test]
    fn classify_unrelated_directory_never_matches() {
        let root = tmpdir("unrelated");
        let src = root.join("src");
        fs::create_dir_all(&src).unwrap();
        assert!(
            classify_artifact(&src, &[]).is_none(),
            "an ordinary source directory must never match the registry"
        );
    }

    #[test]
    fn classify_camerata_shared_target_matches_infra_spec() {
        let root = tmpdir("shared-target-classify");
        let shared = root.join(".camerata-shared-target");
        fs::create_dir_all(&shared).unwrap();
        let m = classify_artifact(&shared, &[]).expect(".camerata-shared-target must match");
        assert_eq!(m.dir_name, ".camerata-shared-target");
    }

    #[test]
    fn classify_orphan_container_matches_any_child_name() {
        let root = tmpdir("orphan-container-classify");
        let worktrees = root.join(".camerata-worktrees");
        let child = worktrees.join("camerata__story-7");
        fs::create_dir_all(&child).unwrap();
        let m = classify_artifact(&child, &[])
            .expect("any child of .camerata-worktrees matches the OrphanContainer spec");
        assert_eq!(m.dir_name, ".camerata-worktrees");
    }

    // ── is_safe_to_reclaim (invariant 3: symlinks + root-escape) ─────────────────

    #[test]
    fn safe_to_reclaim_true_for_plain_dir_inside_root() {
        let root = tmpdir("safe-plain");
        let target = root.join("target");
        fs::create_dir_all(&target).unwrap();
        assert!(is_safe_to_reclaim(&target, &root).is_ok());
    }

    #[test]
    fn safe_to_reclaim_refuses_symlinked_artifact_dir() {
        let root = tmpdir("safe-symlink-leaf");
        let real_elsewhere = tmpdir("safe-symlink-leaf-elsewhere");
        let link = root.join("target");
        symlink(&real_elsewhere, &link).unwrap();

        let result = is_safe_to_reclaim(&link, &root);
        assert_eq!(result, Err(RefusalReason::IsSymlink));
        assert!(real_elsewhere.exists(), "the symlink target must be untouched");
    }

    #[test]
    fn safe_to_reclaim_refuses_ancestor_symlink_escaping_root() {
        // Adversarial layout: root/subdir is a SYMLINK to a directory OUTSIDE root
        // that contains a "target" dir. Even though the leaf `target` component
        // itself isn't a symlink, its resolved (canonicalized) path escapes `root`.
        let root = tmpdir("safe-ancestor-escape-root");
        let outside = tmpdir("safe-ancestor-escape-outside");
        let outside_target = outside.join("target");
        fs::create_dir_all(&outside_target).unwrap();
        // A "source" marker file to prove this directory would matter if reached.
        fs::write(outside.join("source.txt"), "do not touch").unwrap();

        let escape_link = root.join("subdir");
        symlink(&outside, &escape_link).unwrap();
        let candidate = escape_link.join("target");

        let result = is_safe_to_reclaim(&candidate, &root);
        assert_eq!(result, Err(RefusalReason::ResolvesOutsideRoot));
        assert!(outside_target.exists(), "the escaped target must be untouched");
    }

    #[test]
    fn evaluate_reclaim_never_deletes_through_symlink_escape_end_to_end() {
        // Full end-to-end adversarial test: reclaim_dir must refuse and must NOT
        // delete anything when handed a path that escapes root via a symlink.
        let root = tmpdir("e2e-symlink-escape-root");
        let outside = tmpdir("e2e-symlink-escape-outside");
        let victim = outside.join("target");
        fs::create_dir_all(&victim).unwrap();
        fs::write(victim.join("important.rs"), "fn main() {}").unwrap();

        let escape_link = root.join("evil");
        symlink(&outside, &escape_link).unwrap();
        let candidate = escape_link.join("target");

        let mut logged = Vec::new();
        let outcome = reclaim_dir(&candidate, &root, &[], true, "test", false, |e| {
            logged.push(e.clone())
        });

        assert!(matches!(outcome, ReclaimOutcome::Skipped { .. }));
        assert!(logged.is_empty(), "a refused path must never be logged as reclaimed");
        assert!(victim.exists(), "the escaped directory must survive untouched");
        assert!(victim.join("important.rs").exists());
    }

    // ── evaluate_reclaim (invariant 2: gitignore verification) ───────────────────

    #[test]
    fn evaluate_reclaim_allows_gitignored_target_in_real_repo() {
        let root = tmpdir("gitignore-allow");
        init_git_repo(&root);
        fs::write(root.join(".gitignore"), "/target\n").unwrap();
        let target = root.join("target");
        fs::create_dir_all(&target).unwrap();

        let result = evaluate_reclaim(&target, &root, &[], false);
        assert!(result.is_ok(), "gitignored target/ must be reclaimable: {result:?}");
    }

    #[test]
    fn evaluate_reclaim_refuses_tracked_directory_even_with_matching_name() {
        // Adversarial layout: a directory literally named "target" that is actually
        // TRACKED SOURCE (committed to git, not gitignored). Must be refused even
        // though the name matches the Rust registry entry.
        let root = tmpdir("gitignore-refuse-tracked");
        init_git_repo(&root);
        let target = root.join("target");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("real_source.rs"), "fn main() {}").unwrap();
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "-q", "-m", "commit tracked target/ dir"]);

        let result = evaluate_reclaim(&target, &root, &[], false);
        assert_eq!(result, Err(RefusalReason::NotGitignored));
        assert!(target.join("real_source.rs").exists());
    }

    #[test]
    fn evaluate_reclaim_refuses_untracked_but_not_gitignored_directory() {
        // Untracked (never added/committed) but ALSO not covered by any .gitignore
        // rule — the honest "not ignored" case, distinct from tracked. Must still
        // be refused: only EXPLICITLY gitignored artifact dirs are safe.
        let root = tmpdir("gitignore-refuse-untracked");
        init_git_repo(&root);
        let target = root.join("target");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("could_be_anything.txt"), "hmm").unwrap();

        let result = evaluate_reclaim(&target, &root, &[], false);
        assert_eq!(result, Err(RefusalReason::NotGitignored));
    }

    #[test]
    fn evaluate_reclaim_refuses_when_not_inside_a_git_repo_and_not_infra() {
        let root = tmpdir("gitignore-no-repo");
        let target = root.join("target");
        fs::create_dir_all(&target).unwrap();
        // Deliberately NOT a git repo.
        let result = evaluate_reclaim(&target, &root, &[], false);
        assert!(matches!(result, Err(RefusalReason::CannotVerifyGitignore(_))));
    }

    #[test]
    fn evaluate_reclaim_bypasses_gitignore_for_camerata_infra() {
        let root = tmpdir("infra-bypass");
        let shared = root.join(".camerata-shared-target");
        fs::create_dir_all(&shared).unwrap();
        // No git repo at all here — infra paths must still be reclaimable.
        let result = evaluate_reclaim(&shared, &root, &[], true);
        assert!(result.is_ok(), "camerata infra bypasses gitignore verification: {result:?}");
    }

    #[test]
    fn evaluate_reclaim_refuses_no_registry_match_even_when_gitignored() {
        // A gitignored directory whose name matches NOTHING in the registry (e.g.
        // a gitignored `secrets/` or `local-data/` dir) must still be refused — the
        // registry match (invariant 1) is independent of gitignore status.
        let root = tmpdir("gitignore-but-no-registry-match");
        init_git_repo(&root);
        fs::write(root.join(".gitignore"), "/local-data\n").unwrap();
        let dir = root.join("local-data");
        fs::create_dir_all(&dir).unwrap();

        let result = evaluate_reclaim(&dir, &root, &[], false);
        assert_eq!(result, Err(RefusalReason::NoRegistryMatch));
    }

    // ── adversarial layouts (combined) ────────────────────────────────────────────

    #[test]
    fn adversarial_git_inside_artifact_dir_is_still_gitignore_verified_by_outer_repo() {
        // A ".git" directory living INSIDE an artifact dir (e.g. a vendored/nested
        // repo checked out under target/) must never itself be treated as a reclaim
        // candidate by the ZONE-B SCANNER (which explicitly skips ".git" by name),
        // and the OUTER artifact dir's own gitignore status must still be evaluated
        // against the OUTER repo, unaffected by the nested one.
        let root = tmpdir("adversarial-nested-git");
        init_git_repo(&root);
        fs::write(root.join(".gitignore"), "/target\n").unwrap();
        let target = root.join("target");
        let nested_git = target.join(".git");
        fs::create_dir_all(&nested_git).unwrap();
        fs::write(nested_git.join("HEAD"), "ref: refs/heads/main\n").unwrap();

        // The outer target/ must still be a valid, gitignored candidate.
        let result = evaluate_reclaim(&target, &root, &[], false);
        assert!(result.is_ok(), "outer target/ must reclaim normally: {result:?}");

        // The Zone-B scanner must never surface the NESTED .git as its own candidate.
        let candidates = scan_zone_b(&root, &[]);
        assert!(
            candidates.iter().all(|c| c.path != nested_git),
            "a nested .git must never be reported as a Zone-B candidate"
        );
    }

    #[test]
    fn adversarial_symlinked_node_modules_is_never_reclaimed() {
        let root = tmpdir("adversarial-symlink-node-modules");
        init_git_repo(&root);
        fs::write(root.join(".gitignore"), "/node_modules\n").unwrap();
        let real_cache = tmpdir("adversarial-symlink-node-modules-real");
        fs::write(real_cache.join("do_not_delete.txt"), "important").unwrap();
        let link = root.join("node_modules");
        symlink(&real_cache, &link).unwrap();

        let mut logged = Vec::new();
        let outcome = reclaim_dir(&link, &root, &[], false, "test", false, |e| logged.push(e.clone()));
        assert!(matches!(outcome, ReclaimOutcome::Skipped { .. }));
        assert!(logged.is_empty());
        assert!(real_cache.join("do_not_delete.txt").exists());
    }

    // ── dir_size (no panics on fs weirdness) ──────────────────────────────────────

    #[test]
    fn dir_size_sums_nested_files() {
        let root = tmpdir("dir-size-nested");
        fs::write(root.join("a.txt"), "12345").unwrap();
        let sub = root.join("sub");
        fs::create_dir_all(&sub).unwrap();
        fs::write(sub.join("b.txt"), "1234567890").unwrap();
        assert_eq!(dir_size(&root), 5 + 10);
    }

    #[test]
    fn dir_size_never_panics_on_missing_dir() {
        let missing = std::env::temp_dir().join("cam-janitor-definitely-does-not-exist-xyz");
        assert_eq!(dir_size(&missing), 0);
    }

    #[test]
    fn dir_size_skips_symlinked_children_without_following() {
        let root = tmpdir("dir-size-symlink-child");
        fs::write(root.join("real.txt"), "abc").unwrap();
        let elsewhere = tmpdir("dir-size-symlink-child-elsewhere");
        fs::write(elsewhere.join("big.txt"), "x".repeat(1000)).unwrap();
        symlink(&elsewhere, root.join("link")).unwrap();
        // Only "real.txt" (3 bytes) should count; the symlinked subtree must not be
        // followed/counted.
        assert_eq!(dir_size(&root), 3);
    }

    // ── reclaim_dir: logging happens BEFORE deletion (invariant 4) ────────────────

    #[test]
    fn reclaim_dir_logs_before_deleting_camerata_infra() {
        let root = tmpdir("log-before-delete-root");
        let shared = root.join(".camerata-shared-target");
        fs::create_dir_all(&shared).unwrap();
        fs::write(shared.join("artifact.bin"), "deadbeef").unwrap();

        let mut saw_path_exist_at_log_time = false;
        let outcome = reclaim_dir(&shared, &root, &[], true, "test-trigger", false, |_entry| {
            saw_path_exist_at_log_time = shared.exists();
        });

        assert!(saw_path_exist_at_log_time, "on_log must fire while the path still exists");
        assert!(matches!(outcome, ReclaimOutcome::Reclaimed(_)));
        assert!(!shared.exists(), "the path must be gone after reclaim_dir returns");
    }

    #[test]
    fn reclaim_dir_dry_run_logs_but_never_deletes() {
        let root = tmpdir("dry-run-root");
        let shared = root.join(".camerata-shared-target");
        fs::create_dir_all(&shared).unwrap();
        fs::write(shared.join("artifact.bin"), "deadbeef").unwrap();

        let mut logged = Vec::new();
        let outcome = reclaim_dir(&shared, &root, &[], true, "test", true, |e| logged.push(e.clone()));

        assert_eq!(logged.len(), 1);
        assert!(logged[0].dry_run);
        assert!(matches!(outcome, ReclaimOutcome::Reclaimed(_)));
        assert!(shared.exists(), "dry-run must never actually delete");
    }

    // ── Zone A: shared-target prune policy (pure decisions) ──────────────────────

    #[test]
    fn should_prune_shared_target_true_when_over_cap() {
        let max = 30u64 * 1024 * 1024 * 1024;
        assert!(should_prune_shared_target(
            max + 1,
            max,
            Duration::from_secs(0),
            Duration::from_secs(u64::MAX / 2),
            true
        ));
    }

    #[test]
    fn should_prune_shared_target_true_when_no_live_worktrees() {
        assert!(should_prune_shared_target(
            0,
            u64::MAX,
            Duration::from_secs(0),
            Duration::from_secs(u64::MAX / 2),
            false
        ));
    }

    #[test]
    fn should_prune_shared_target_true_when_stale_and_orphaned() {
        let max_age = Duration::from_secs(14 * 24 * 60 * 60);
        assert!(shared_target_stale(max_age + Duration::from_secs(1), max_age, false));
        assert!(!shared_target_stale(max_age - Duration::from_secs(1), max_age, false));
        assert!(!shared_target_stale(max_age + Duration::from_secs(1), max_age, true));
    }

    #[test]
    fn should_prune_shared_target_false_when_under_cap_fresh_and_live() {
        assert!(!should_prune_shared_target(
            1024,
            30 * 1024 * 1024 * 1024,
            Duration::from_secs(60),
            Duration::from_secs(14 * 24 * 60 * 60),
            true
        ));
    }

    // ── list_orphan_worktree_dirs / has_live_worktrees ────────────────────────────

    #[test]
    fn list_orphan_worktree_dirs_empty_for_non_git_dir() {
        let root = tmpdir("orphan-non-git");
        let worktrees = root.join(".camerata-worktrees");
        fs::create_dir_all(worktrees.join("stray")).unwrap();
        // root is NOT a git repo — conservative empty result, never guesses.
        assert!(list_orphan_worktree_dirs(&root).is_empty());
    }

    #[test]
    fn list_orphan_worktree_dirs_finds_unregistered_stray_directory() {
        let root = tmpdir("orphan-real-repo");
        init_git_repo(&root);
        fs::write(root.join("README.md"), "hi").unwrap();
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "-q", "-m", "init"]);

        let worktrees = root.join(".camerata-worktrees");
        fs::create_dir_all(&worktrees).unwrap();
        // A real registered worktree.
        let add = git(
            &root,
            &[
                "worktree",
                "add",
                "-b",
                "camerata-story-real",
                worktrees.join("camerata-story-real").to_str().unwrap(),
            ],
        );
        assert!(add.status.success(), "{}", String::from_utf8_lossy(&add.stderr));
        // A stray, unregistered directory left behind by a crash.
        fs::create_dir_all(worktrees.join("stray-crash-leftover")).unwrap();

        let orphans = list_orphan_worktree_dirs(&root);
        let orphan_names: Vec<String> = orphans
            .iter()
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
            .collect();
        assert!(orphan_names.contains(&"stray-crash-leftover".to_string()));
        assert!(
            !orphan_names.contains(&"camerata-story-real".to_string()),
            "a REGISTERED worktree must never be listed as an orphan"
        );
    }

    #[test]
    fn has_live_worktrees_false_when_none_registered() {
        let root = tmpdir("live-worktrees-none");
        init_git_repo(&root);
        fs::write(root.join("README.md"), "hi").unwrap();
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "-q", "-m", "init"]);
        assert!(!has_live_worktrees(&root));
    }

    #[test]
    fn has_live_worktrees_true_when_one_registered() {
        let root = tmpdir("live-worktrees-one");
        init_git_repo(&root);
        fs::write(root.join("README.md"), "hi").unwrap();
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "-q", "-m", "init"]);
        let worktrees = root.join(".camerata-worktrees");
        fs::create_dir_all(&worktrees).unwrap();
        let add = git(
            &root,
            &[
                "worktree",
                "add",
                "-b",
                "camerata-story-live",
                worktrees.join("camerata-story-live").to_str().unwrap(),
            ],
        );
        assert!(add.status.success());
        assert!(has_live_worktrees(&root));
    }

    #[test]
    fn has_live_worktrees_fails_closed_when_not_a_repo() {
        let root = tmpdir("live-worktrees-not-a-repo");
        // Not a git repo: the query fails, must default to "has live worktrees"
        // (never prune eagerly on an error).
        assert!(has_live_worktrees(&root));
    }

    // ── reclaim_zone_a_for_clone (end-to-end Zone A sweep) ────────────────────────

    #[test]
    fn reclaim_zone_a_for_clone_removes_orphan_and_prunes_shared_target_when_asked() {
        let root = tmpdir("zone-a-e2e");
        init_git_repo(&root);
        fs::write(root.join("README.md"), "hi").unwrap();
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "-q", "-m", "init"]);

        let worktrees = root.join(".camerata-worktrees");
        fs::create_dir_all(&worktrees).unwrap();
        fs::create_dir_all(worktrees.join("stray")).unwrap();
        fs::write(worktrees.join("stray").join("junk.txt"), "junk").unwrap();

        let shared = root.join(".camerata-shared-target");
        fs::create_dir_all(&shared).unwrap();
        fs::write(shared.join("artifact.bin"), "0123456789").unwrap();

        let mut logged = Vec::new();
        let total = reclaim_zone_a_for_clone(&root, true, "test", false, |e| logged.push(e.clone()));

        assert!(!worktrees.join("stray").exists(), "orphan worktree must be removed");
        assert!(!shared.exists(), "shared target must be pruned when prune_shared_target=true");
        assert!(total > 0);
        assert_eq!(logged.len(), 2);
    }

    #[test]
    fn reclaim_zone_a_for_clone_never_touches_a_registered_worktree() {
        let root = tmpdir("zone-a-e2e-registered-safe");
        init_git_repo(&root);
        fs::write(root.join("README.md"), "hi").unwrap();
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "-q", "-m", "init"]);

        let worktrees = root.join(".camerata-worktrees");
        fs::create_dir_all(&worktrees).unwrap();
        let add = git(
            &root,
            &[
                "worktree",
                "add",
                "-b",
                "camerata-live-branch",
                worktrees.join("camerata-live-branch").to_str().unwrap(),
            ],
        );
        assert!(add.status.success());
        fs::write(worktrees.join("camerata-live-branch").join("in_progress.rs"), "// wip").unwrap();

        let mut logged = Vec::new();
        reclaim_zone_a_for_clone(&root, false, "test", false, |e| logged.push(e.clone()));

        assert!(worktrees.join("camerata-live-branch").exists());
        assert!(worktrees
            .join("camerata-live-branch")
            .join("in_progress.rs")
            .exists());
        assert!(logged.is_empty());
    }

    // ── scan_zone_b (measure + report only, never deletes) ───────────────────────

    #[test]
    fn scan_zone_b_reports_gitignored_target_and_never_deletes_it() {
        let root = tmpdir("scan-zone-b-basic");
        init_git_repo(&root);
        fs::write(root.join(".gitignore"), "/target\n").unwrap();
        let target = root.join("target");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("bytes.bin"), "0123456789").unwrap();

        let candidates = scan_zone_b(&root, &[]);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].dir_name, "target");
        assert_eq!(candidates[0].bytes, 10);
        assert!(target.exists(), "scan_zone_b must NEVER delete anything");
    }

    #[test]
    fn scan_zone_b_excludes_tracked_directory_named_like_an_artifact() {
        let root = tmpdir("scan-zone-b-tracked-source");
        init_git_repo(&root);
        let target = root.join("target");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("real_source.rs"), "fn main() {}").unwrap();
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "-q", "-m", "tracked target dir"]);

        let candidates = scan_zone_b(&root, &[]);
        assert!(
            candidates.is_empty(),
            "a TRACKED directory named target/ must never be reported as a candidate"
        );
    }

    #[test]
    fn scan_zone_b_does_not_recurse_into_a_matched_artifact_dir() {
        let root = tmpdir("scan-zone-b-no-recurse");
        init_git_repo(&root);
        fs::write(root.join(".gitignore"), "/node_modules\n").unwrap();
        let nm = root.join("node_modules");
        // A nested package.json + node_modules INSIDE node_modules (a vendored dep) —
        // must not be reported as a second, separate candidate.
        let nested = nm.join("some-pkg").join("node_modules");
        fs::create_dir_all(&nested).unwrap();

        let candidates = scan_zone_b(&root, &[]);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].path, nm);
    }

    #[test]
    fn scan_zone_b_finds_extra_dirs_declared_by_the_repo() {
        let root = tmpdir("scan-zone-b-extra-dirs");
        init_git_repo(&root);
        fs::write(root.join(".gitignore"), "/weird-cache\n").unwrap();
        fs::create_dir_all(root.join("weird-cache")).unwrap();

        let extra = vec!["weird-cache".to_string()];
        let candidates = scan_zone_b(&root, &extra);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].dir_name, "weird-cache");
    }

    // ── check_headroom_with_reclaim (pure, injectable seam — T4) ──────────────────

    #[test]
    fn headroom_sufficient_never_calls_reclaim() {
        let mut reclaim_calls = 0u32;
        let outcome = check_headroom_with_reclaim(
            10 * 1024 * 1024 * 1024,
            || Some(20 * 1024 * 1024 * 1024),
            || {
                reclaim_calls += 1;
                0
            },
        );
        assert_eq!(outcome, HeadroomOutcome::Sufficient { available: 20 * 1024 * 1024 * 1024 });
        assert_eq!(reclaim_calls, 0, "reclaim must never run when headroom is already sufficient");
    }

    #[test]
    fn headroom_short_then_reclaim_succeeds() {
        let min = 10u64 * 1024 * 1024 * 1024;
        let mut calls = 0u32;
        let mut available_seq = vec![min - 1, min + 5].into_iter();
        let outcome = check_headroom_with_reclaim(
            min,
            || available_seq.next(),
            || {
                calls += 1;
                6
            },
        );
        assert_eq!(
            outcome,
            HeadroomOutcome::ReclaimedSufficient {
                available: min + 5,
                reclaimed_bytes: 6
            }
        );
        assert_eq!(calls, 1, "reclaim must run exactly once");
    }

    #[test]
    fn headroom_short_then_reclaim_still_insufficient_blocks() {
        let min = 10u64 * 1024 * 1024 * 1024;
        let mut available_seq = vec![min - 100, min - 50].into_iter();
        let outcome = check_headroom_with_reclaim(min, || available_seq.next(), || 50);
        assert_eq!(
            outcome,
            HeadroomOutcome::StillInsufficient {
                available: min - 50,
                reclaimed_bytes: 50
            }
        );
    }

    #[test]
    fn headroom_cannot_query_fails_open() {
        let outcome = check_headroom_with_reclaim(10, || None, || 0);
        assert_eq!(outcome, HeadroomOutcome::CannotQuery);
    }

    #[test]
    fn headroom_cannot_query_after_reclaim_also_fails_open() {
        let mut available_seq = vec![Some(0u64), None].into_iter();
        let outcome = check_headroom_with_reclaim(10, || available_seq.next().flatten(), || 5);
        assert_eq!(outcome, HeadroomOutcome::CannotQuery);
    }

    // ── config parsing ────────────────────────────────────────────────────────────

    #[test]
    fn parse_janitor_mode_variants() {
        assert_eq!(parse_janitor_mode(None), JanitorMode::On);
        assert_eq!(parse_janitor_mode(Some("on")), JanitorMode::On);
        assert_eq!(parse_janitor_mode(Some("off")), JanitorMode::Off);
        assert_eq!(parse_janitor_mode(Some("OFF")), JanitorMode::Off);
        assert_eq!(parse_janitor_mode(Some("dry-run")), JanitorMode::DryRun);
        assert_eq!(parse_janitor_mode(Some("dry_run")), JanitorMode::DryRun);
        assert_eq!(parse_janitor_mode(Some("garbage")), JanitorMode::On);
    }

    #[test]
    fn parse_gb_env_defaults_and_parses() {
        assert_eq!(parse_gb_env(None, 30), 30 * 1024 * 1024 * 1024);
        assert_eq!(parse_gb_env(Some("5"), 30), 5 * 1024 * 1024 * 1024);
        assert_eq!(parse_gb_env(Some("not-a-number"), 30), 30 * 1024 * 1024 * 1024);
    }

    #[test]
    fn parse_days_env_defaults_and_parses() {
        assert_eq!(parse_days_env(None, 14), Duration::from_secs(14 * 24 * 60 * 60));
        assert_eq!(parse_days_env(Some("7"), 14), Duration::from_secs(7 * 24 * 60 * 60));
        assert_eq!(parse_days_env(Some("nope"), 14), Duration::from_secs(14 * 24 * 60 * 60));
    }

    // ── format_zone_b_inventory ────────────────────────────────────────────────────

    #[test]
    fn format_zone_b_inventory_empty_when_no_candidates() {
        assert_eq!(format_zone_b_inventory(&[], 5), "");
    }

    #[test]
    fn format_zone_b_inventory_names_path_and_size() {
        let candidates = vec![ZoneBCandidate {
            path: PathBuf::from("/repo/target"),
            dir_name: "target".to_string(),
            tier: ArtifactTier::Cheap,
            bytes: 2 * 1024 * 1024 * 1024,
        }];
        let msg = format_zone_b_inventory(&candidates, 5);
        assert!(msg.contains("/repo/target"));
        assert!(msg.contains("2.0 GB"));
        assert!(msg.contains("rm -rf"));
    }
}
