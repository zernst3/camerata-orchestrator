//! The remote-match guard: every reason Camerata refuses to delete a folder, in a fixed order.
//!
//! Deleting source is the one destructive act in the product, and it runs on a path read out of
//! a settings file. So nothing is removed until a folder has passed ALL thirteen checks below,
//! and the first one that fails is the one reported (design, section 4):
//!
//! | # | code | refuses when |
//! |---|------|--------------|
//! | R1 | `unknown_project` / `repo_not_in_project` | the project or the repo is not known |
//! | R2 | `no_path` | no local folder is recorded for the repo |
//! | R3 | `not_a_directory` | the recorded path is missing or is not a folder |
//! | R4 | `symlinked_path` | any component of the path is a symbolic link |
//! | R5 | `protected_path` | the folder is the home dir, the workspace root, Camerata's own stores or program, an ancestor of any of those or of another checkout, too shallow, relative, or (in a test) a real user location |
//! | R6 | `not_a_git_repo` / `linked_worktree` | the folder has no `.git`, or its `.git` is a file (it is a linked worktree of another clone) |
//! | R7 | `no_origin` / `origin_not_github` / `origin_unreadable` | its `origin` cannot be read as a GitHub repo |
//! | R8 | `origin_mismatch` | its `origin` is a different repo than the project's |
//! | R9 | `confirmation_mismatch` | the typed name is not the repo's name, byte for byte |
//! | R10 | `dirty_tree` | uncommitted work and the operator did not tick "Delete uncommitted work too" |
//! | R11 | `unpushed_commits` | commits not on GitHub, same tick |
//! | R12 | `scan_in_progress` / `governed_run_in_progress` | a scan, re-fetch or delete is running for the project or repo, or an unfinished governed, investigation, update-branch or PR-resolve run is working on the repo |
//! | R13 | `worktree_outside_refused` | a registered worktree is itself a symlink or a protected location, or a listed folder is not really a worktree of this clone |
//!
//! A preview ([`Intent::Preview`]) runs the same checks except R9 (nothing was typed yet) and
//! reports R10/R11 as facts about the target instead of refusing, so the confirmation dialog can
//! show them and offer the tick.

use std::path::{Component, Path, PathBuf};

use super::git::{self, Unpushed};
use super::ops::OpGuard;
use super::status::{project_and_repo, resolve_dir};
use super::storage::{ensure_not_real_location_in_test, StorageRoots};
use super::Refusal;
use crate::onboard::finding_context::canonicalize_existing_prefix;
use crate::workspace::{self, OriginError};
use crate::AppState;

/// What the caller wants to do, which decides how R9 to R11 behave.
#[derive(Debug, Clone, Copy)]
pub enum Intent<'a> {
    /// Show the plan. No confirmation is required; dirty and unpushed work are reported, not
    /// refused.
    Preview,
    /// Delete. `confirm` must equal the repo's name; unpushed or uncommitted work refuses unless
    /// `include_uncommitted`.
    Delete {
        confirm: &'a str,
        include_uncommitted: bool,
    },
}

/// A folder that passed the guard, with everything the plan and the record need to know about it.
/// Holding it keeps the repo's exclusive operation claim.
pub struct GuardedTarget {
    pub project_id: String,
    /// The repo as the project spells it.
    pub repo: String,
    /// The canonical path of the checkout.
    pub path: PathBuf,
    /// `origin` as observed, credentials scrubbed.
    pub origin_url: Option<String>,
    pub head_sha: Option<String>,
    /// `git status --porcelain` entries (Camerata scratch excluded).
    pub dirty_files: u64,
    pub stashes: u64,
    pub unpushed: Vec<Unpushed>,
    /// Registered worktrees other than the checkout itself, as git lists them.
    pub worktrees: Vec<PathBuf>,
    _claim: OpGuard,
}

/// Run R1 to R13 for `repo` of `project_id`. See the module docs.
pub async fn guard(
    state: &AppState,
    project_id: &str,
    repo_param: &str,
    intent: Intent<'_>,
) -> Result<GuardedTarget, Refusal> {
    // ── R1 ──────────────────────────────────────────────────────────────────────────────────
    let (project, repo) = project_and_repo(state, project_id, repo_param)?;

    // ── R2 ──────────────────────────────────────────────────────────────────────────────────
    let Some(recorded) = resolve_dir(state, &repo) else {
        return Err(Refusal::conflict(
            "no_path",
            format!("No local folder is recorded for {repo}, so there is nothing to delete."),
        ));
    };

    // ── R3 ──────────────────────────────────────────────────────────────────────────────────
    if !recorded.is_dir() {
        return Err(Refusal::conflict(
            "not_a_directory",
            format!(
                "{} does not exist (or is not a folder). Nothing to delete.",
                recorded.display()
            ),
        ));
    }

    // ── R4 ──────────────────────────────────────────────────────────────────────────────────
    if let Some(link) = first_symlink(&recorded) {
        return Err(Refusal::conflict(
            "symlinked_path",
            format!(
                "{} is a symbolic link. Camerata deletes only a real folder; link the real folder \
                 instead.",
                link.display()
            ),
        ));
    }

    // ── R5 ──────────────────────────────────────────────────────────────────────────────────
    let path = canonicalize_existing_prefix(&recorded);
    if let Some(why) = protected_reason(state, &recorded, &path, Some((&project.id, &repo))) {
        return Err(Refusal::conflict(
            "protected_path",
            format!(
                "{} is a protected location, not a repository checkout. ({why})",
                recorded.display()
            ),
        ));
    }

    // ── R6 ──────────────────────────────────────────────────────────────────────────────────
    if !workspace::is_git_repo(&path) {
        return Err(Refusal::conflict(
            "not_a_git_repo",
            format!(
                "{} is not a git repository (no .git). Camerata deletes only a git checkout whose \
                 origin matches the project.",
                path.display()
            ),
        ));
    }

    // A linked worktree has a `.git` FILE, not a directory. Deleting it would leave the clone it
    // belongs to (and all of its code) on disk while the repo read as deleted.
    if path.join(".git").is_file() {
        let clone = match git::common_dir(&path).await {
            Some(common) if common.file_name().is_some_and(|n| n == ".git") => {
                common.parent().map(Path::to_path_buf).unwrap_or(common)
            }
            Some(common) => common,
            None => PathBuf::from("its main clone"),
        };
        return Err(Refusal::conflict(
            "linked_worktree",
            format!(
                "{} is a worktree of the clone at {}; delete that clone instead.",
                path.display(),
                clone.display()
            ),
        ));
    }

    // ── R7 ──────────────────────────────────────────────────────────────────────────────────
    let found = match workspace::detect_origin(&path).await {
        Ok(found) => found,
        Err(e @ OriginError::NoRemote) => {
            return Err(Refusal::conflict("no_origin", e.to_string()));
        }
        Err(e @ OriginError::NotGithub { .. }) => {
            return Err(Refusal::conflict("origin_not_github", e.to_string()));
        }
        Err(e @ (OriginError::Git(_) | OriginError::GitUnavailable(_))) => {
            return Err(Refusal::conflict("origin_unreadable", e.to_string()));
        }
    };

    // ── R8 ──────────────────────────────────────────────────────────────────────────────────
    if !found.eq_ignore_ascii_case(repo.trim()) {
        return Err(Refusal::conflict(
            "origin_mismatch",
            format!(
                "{} is a clone of {found}, not {repo}. Nothing was deleted.",
                path.display()
            ),
        ));
    }

    // ── R9 ──────────────────────────────────────────────────────────────────────────────────
    if let Intent::Delete { confirm, .. } = intent {
        if confirm != repo {
            return Err(Refusal::conflict(
                "confirmation_mismatch",
                format!("Type the repository name exactly as shown: {repo}."),
            ));
        }
    }

    // Facts about the work in the checkout (needed by R10, R11 and the preview).
    let dirty_files = git::dirty_file_count(&path).await;
    let stashes = git::stash_count(&path).await;
    let unpushed = git::unpushed(&path).await;

    // ── R10 / R11 ───────────────────────────────────────────────────────────────────────────
    if let Intent::Delete {
        include_uncommitted: false,
        ..
    } = intent
    {
        let uncommitted = dirty_files + stashes;
        if uncommitted > 0 {
            return Err(Refusal::conflict(
                "dirty_tree",
                format!(
                    "{repo} has uncommitted changes ({uncommitted} {}). Tick 'Delete uncommitted \
                     work too' to remove them.",
                    if uncommitted == 1 { "item" } else { "items" }
                ),
            ));
        }
        if !unpushed.is_empty() {
            let list = unpushed
                .iter()
                .map(|u| format!("{}: {}", u.branch, u.ahead))
                .collect::<Vec<_>>()
                .join(", ");
            return Err(Refusal::conflict(
                "unpushed_commits",
                format!(
                    "{repo} has commits that are not on GitHub ({list}). Tick 'Delete uncommitted \
                     work too' to remove them."
                ),
            ));
        }
    }

    // ── R12 ─────────────────────────────────────────────────────────────────────────────────
    if state.jobs.has_running_for_project(&project.id) || state.source_ops.scan_running(&project.id)
    {
        return Err(Refusal::conflict(
            "scan_in_progress",
            "A scan is running for this project. Wait for it to finish or cancel it first.",
        ));
    }
    if governed_run_active_on(state, &repo) {
        return Err(Refusal::conflict(
            "governed_run_in_progress",
            format!(
                "A governed run is in progress on {repo}; wait for it to finish or stop it first."
            ),
        ));
    }
    let Some(claim) = state.source_ops.try_begin(&project.id, &repo) else {
        return Err(Refusal::conflict(
            "scan_in_progress",
            format!("Another operation on {repo} is already running. Wait for it to finish."),
        ));
    };

    // ── R13 ─────────────────────────────────────────────────────────────────────────────────
    let worktrees = match git::linked_worktrees(&path).await {
        Ok(list) => list,
        Err(e) => {
            return Err(Refusal::conflict(
                "worktree_outside_refused",
                format!("Could not list the worktrees of {repo} ({e}). Nothing was deleted."),
            ));
        }
    };
    for wt in &worktrees {
        if let Some(why) = worktree_refusal(state, wt) {
            return Err(Refusal::conflict(
                "worktree_outside_refused",
                format!(
                    "A worktree of {repo} at {} {why}; remove it by hand first.",
                    wt.display()
                ),
            ));
        }
    }

    let origin_url = git::read(&path, &["config", "--get", "remote.origin.url"])
        .await
        .map(|u| workspace::scrub_remote_url(&u));
    let head_sha = git::head_sha(&path).await;
    Ok(GuardedTarget {
        project_id: project.id.clone(),
        repo,
        path,
        origin_url,
        head_sha,
        dirty_files,
        stashes,
        unpushed,
        worktrees,
        _claim: claim,
    })
}

/// Whether a run that has not finished is working on `repo`. Every governed development,
/// investigation, update-branch and PR-resolve run (routine-driven ones too) is created in
/// `AppState::runs` and stays there, not `done`, until it ends or is cancelled, so that store is
/// the registry; a run parked on a question or a review is still open and counts.
///
/// A run is attributed to a repo by its story id (`owner/repo#n`), the work item a draft story
/// links to, and the repos a UoW put in scope at intake. A story that names no repo (a scripted
/// demo story) is attributed to none.
fn governed_run_active_on(state: &AppState, repo: &str) -> bool {
    state
        .runs
        .snapshot_active()
        .iter()
        .any(|run| story_touches_repo(state, &run.story_id, repo))
}

fn story_touches_repo(state: &AppState, story_id: &str, repo: &str) -> bool {
    let mut repos: Vec<String> = crate::repo_from_story_id(story_id).into_iter().collect();
    if let Some(uow) = state.uow.get(story_id) {
        repos.extend(uow.intake.repos.iter().map(|scope| scope.repo.clone()));
        repos.extend(uow.work_item.as_deref().and_then(crate::repo_from_story_id));
    }
    repos.iter().any(|r| r.eq_ignore_ascii_case(repo.trim()))
}

/// R13 for ONE registered worktree: why it must not be removed along with the checkout, or
/// `None`. A worktree is removed like the checkout is, so it faces the same two refusals: it may
/// not be reached through a symbolic link, and it may not be a protected location.
pub(crate) fn worktree_refusal(state: &AppState, wt: &Path) -> Option<String> {
    if let Some(link) = first_symlink(wt) {
        return Some(format!("goes through the symbolic link {}", link.display()));
    }
    let canon = canonicalize_existing_prefix(wt);
    protected_reason(state, wt, &canon, None)
        .map(|why| format!("is in a protected location ({why})"))
}

/// The first component of `path` (cumulatively) that is a symbolic link, if any. The operating
/// system's own top-level aliases (macOS's `/var`, `/tmp`, `/etc`, which resolve under
/// `/private`) are not the operator's links and are skipped.
pub(crate) fn first_symlink(path: &Path) -> Option<PathBuf> {
    let mut cur = PathBuf::new();
    let mut depth = 0usize;
    for comp in path.components() {
        cur.push(comp);
        if !matches!(comp, Component::Normal(_)) {
            continue;
        }
        depth += 1;
        let is_link = std::fs::symlink_metadata(&cur).is_ok_and(|m| m.file_type().is_symlink());
        if is_link && !(depth == 1 && is_system_alias(&cur)) {
            return Some(cur);
        }
    }
    None
}

/// `/var` -> `/private/var` and its siblings: a top-level link whose target sits under `/private`.
fn is_system_alias(link: &Path) -> bool {
    std::fs::canonicalize(link).is_ok_and(|c| c.starts_with("/private"))
}

/// Why `canon` (the canonical form of `recorded`) must never be deleted as a repository
/// checkout, or `None` when it may be. `own` is the `(project, repo)` being checked, whose own
/// checkout is exempt from the "contains another checkout" rule; `None` for a worktree check.
pub(crate) fn protected_reason(
    state: &AppState,
    recorded: &Path,
    canon: &Path,
    own: Option<(&str, &str)>,
) -> Option<String> {
    if !recorded.is_absolute() {
        return Some("not an absolute path".to_string());
    }
    // (`Path::components` already drops a mid-path `.`; `..` is the segment that can redirect.)
    if recorded
        .components()
        .any(|c| matches!(c, Component::ParentDir))
    {
        return Some("contains '..' segments".to_string());
    }
    let normal = canon
        .components()
        .filter(|c| matches!(c, Component::Normal(_)))
        .count();
    if canon.parent().is_none() || normal < 3 {
        return Some("too close to the filesystem root".to_string());
    }

    let roots: &StorageRoots = &state.storage_roots;
    // The path may be, or contain, any of these: refuse both.
    let mut shielded: Vec<(&str, PathBuf)> = Vec::new();
    if let Some(home) = &roots.home {
        shielded.push(("the home directory", canonicalize_existing_prefix(home)));
    }
    if let Some(ws) = state.settings.workspace_root() {
        shielded.push((
            "the workspace root",
            canonicalize_existing_prefix(Path::new(&ws)),
        ));
    }
    if let Some(exe) = &roots.exe_dir {
        shielded.push(("Camerata's own program", canonicalize_existing_prefix(exe)));
    }
    for (what, p) in &shielded {
        if p.starts_with(canon) {
            return Some(format!("it is, or contains, {what}"));
        }
    }
    // These may not be inside the path nor contain it (Camerata's own data).
    let mut own_data: Vec<(&str, PathBuf)> = Vec::new();
    if let Some(d) = &roots.data_dir {
        own_data.push(("Camerata's data folder", canonicalize_existing_prefix(d)));
    }
    if let Some(d) = &roots.cli_state_dir {
        own_data.push(("Camerata's cache folder", canonicalize_existing_prefix(d)));
    }
    if let Some(d) = &roots.claude_projects_dir {
        own_data.push((
            "Claude Code's session folder",
            canonicalize_existing_prefix(d),
        ));
    }
    for (what, p) in &own_data {
        if p.starts_with(canon) || canon.starts_with(p) {
            return Some(format!("it is, contains, or is inside {what}"));
        }
    }
    // A folder that holds another registered checkout is a parent of repos, not a repo.
    if let Some((project_id, repo)) = own {
        for other in other_checkouts(state, project_id, repo) {
            let other = canonicalize_existing_prefix(&other);
            if other != *canon && other.starts_with(canon) {
                return Some("it contains another repository's checkout".to_string());
            }
        }
    }
    if ensure_not_real_location_in_test(canon).is_err() {
        return Some("a test process may not delete under a real user location".to_string());
    }
    None
}

/// The folders of every repo of every project except `(project_id, repo)` itself.
fn other_checkouts(state: &AppState, project_id: &str, repo: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for project in state.projects.list() {
        for r in &project.repos {
            if project.id == project_id && r.eq_ignore_ascii_case(repo) {
                continue;
            }
            if let Some(dir) = resolve_dir(state, r) {
                out.push(dir);
            }
        }
    }
    out
}
