//! Per-repo SOURCE STATUS: is the client's code on this machine, was it deleted, or was it
//! never here? A derived view over the disk and the two stores; nothing about it is stored.
//!
//! The four states drive the Workspace repo card, the scan surface's rescan gating, the finding
//! modal's banner and the readiness gate (design, section 2.4).

use std::path::PathBuf;

use serde::Serialize;

use super::git;
use super::records::{DeletionRecord, RefetchRecord};
use crate::workspace;
use crate::AppState;

/// The state of one repo's source on this machine.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum RepoSourceStatus {
    /// A clone whose `origin` matches the repo is on disk.
    Present {
        repo: String,
        path: String,
        head_sha: Option<String>,
        head_branch: Option<String>,
        dirty: bool,
        /// The commit the latest inspection that covered this repo read.
        inspected_sha: Option<String>,
        inspected_at: Option<String>,
        /// `git rev-list --count <inspected>..HEAD`; `None` with no inspection or when the
        /// inspected commit is not in this clone.
        newer_commits: Option<u64>,
        /// `git cat-file -e <inspected>^{commit}` succeeded.
        inspected_commit_reachable: bool,
        /// A deletion record exists for this repo (it was deleted and fetched back).
        has_deletion_history: bool,
    },
    /// The source was deleted by Camerata and has not been fetched back since.
    Deleted {
        repo: String,
        path: String,
        deleted_at: String,
        record_id: String,
        inspected_sha: Option<String>,
        total_files: u64,
        total_bytes: u64,
    },
    /// Nothing on disk and no record: the pre-existing "not cloned / not linked" state (also what
    /// a freshly imported project's repos are).
    Unresolved {
        repo: String,
        path: Option<String>,
        reason: String,
    },
    /// A folder is there but is not this repository (wrong origin, no origin). Reported, never
    /// touched.
    Mismatch {
        repo: String,
        path: String,
        reason: String,
    },
}

impl RepoSourceStatus {
    /// The repo this status is about.
    pub fn repo(&self) -> &str {
        match self {
            RepoSourceStatus::Present { repo, .. }
            | RepoSourceStatus::Deleted { repo, .. }
            | RepoSourceStatus::Unresolved { repo, .. }
            | RepoSourceStatus::Mismatch { repo, .. } => repo,
        }
    }

    /// `Some((deleted_at, record_id))` for a `Deleted` repo.
    pub fn deleted(&self) -> Option<(&str, &str)> {
        match self {
            RepoSourceStatus::Deleted {
                deleted_at,
                record_id,
                ..
            } => Some((deleted_at, record_id)),
            _ => None,
        }
    }
}

/// Where the repo's folder is, per the machine-local override else the workspace layout. The ONE
/// way to find a repo's folder for deletion, reading and re-fetch.
pub(crate) fn resolve_dir(state: &AppState, repo: &str) -> Option<PathBuf> {
    workspace::resolve_repo_dir(
        state.settings.repo_path(repo).as_deref(),
        state.settings.workspace_root().as_deref(),
        repo,
    )
}

/// The newest inspection entry of the project that covers `repo` with a commit, if any.
pub(crate) fn inspected_entry(
    state: &AppState,
    project_id: &str,
    repo: &str,
) -> Option<crate::inspections::InspectionEntry> {
    state
        .inspections
        .list(project_id)
        .into_iter()
        .find(|e| e.sha_for(repo).is_some())
}

/// The deletion record that still stands, i.e. the newest deletion unless a re-fetch is newer.
/// A deletion followed by a re-fetch is history; a re-fetch followed by a deletion is deleted.
fn newer_deletion(
    deletion: Option<DeletionRecord>,
    refetch: Option<&RefetchRecord>,
) -> Option<DeletionRecord> {
    let deletion = deletion?;
    let fetched_back_later = refetch.is_some_and(|r| r.seq > deletion.seq);
    (!fetched_back_later).then_some(deletion)
}

/// The deletion record behind a `Deleted` repo, without any git probing: `Some` exactly when
/// [`source_status`] would answer `Deleted`. Cheap enough to run on every scan request, which is
/// where "source deleted" must be answered before anything tries to read the missing folder.
pub(crate) fn deleted_record(
    state: &AppState,
    project_id: &str,
    repo: &str,
) -> Option<DeletionRecord> {
    if resolve_dir(state, repo)
        .as_deref()
        .is_some_and(workspace::is_git_repo)
    {
        return None;
    }
    newer_deletion(
        state.source_records.latest_deletion(project_id, repo),
        state
            .source_records
            .latest_refetch(project_id, repo)
            .as_ref(),
    )
}

/// The status of one repo of `project_id`. Never errors: every failure is a state.
pub async fn source_status(state: &AppState, project_id: &str, repo: &str) -> RepoSourceStatus {
    let dir = resolve_dir(state, repo);
    let deletion = state.source_records.latest_deletion(project_id, repo);
    let refetch = state.source_records.latest_refetch(project_id, repo);

    // A folder that is a git checkout decides the matter by what it IS, whatever the records say:
    // an operator who cloned the repo back by hand has source on disk.
    if let Some(dir) = dir.as_deref().filter(|d| workspace::is_git_repo(d)) {
        let path = dir.to_string_lossy().into_owned();
        return match workspace::detect_origin(dir).await {
            Ok(found) if found.eq_ignore_ascii_case(repo.trim()) => {
                present(state, project_id, repo, dir, path, deletion.is_some()).await
            }
            Ok(found) => RepoSourceStatus::Mismatch {
                repo: repo.to_string(),
                path,
                reason: format!("that folder is a different repo (origin: {found})"),
            },
            Err(e) => RepoSourceStatus::Mismatch {
                repo: repo.to_string(),
                path,
                // The same words the readiness gate has always shown for this failure.
                reason: e.to_string(),
            },
        };
    }

    // No checkout on disk. The newest of {deletion, re-fetch} decides.
    if let Some(deletion) = newer_deletion(deletion, refetch.as_ref()) {
        return RepoSourceStatus::Deleted {
            repo: repo.to_string(),
            path: deletion.path.clone(),
            deleted_at: deletion.completed_at.clone(),
            record_id: deletion.id.clone(),
            inspected_sha: deletion.inspected_sha.clone(),
            total_files: deletion.total_files,
            total_bytes: deletion.total_bytes,
        };
    }

    let (path, reason) = match &dir {
        None => (
            None,
            "no local path set — choose the repo's folder (or set a workspace root)".to_string(),
        ),
        Some(d) if d.is_dir() => (
            Some(d.to_string_lossy().into_owned()),
            "that folder is not a git clone (no .git)".to_string(),
        ),
        Some(d) => (
            Some(d.to_string_lossy().into_owned()),
            "not cloned yet".to_string(),
        ),
    };
    RepoSourceStatus::Unresolved {
        repo: repo.to_string(),
        path,
        reason,
    }
}

async fn present(
    state: &AppState,
    project_id: &str,
    repo: &str,
    dir: &std::path::Path,
    path: String,
    has_deletion_history: bool,
) -> RepoSourceStatus {
    let head_sha = git::head_sha(dir).await;
    let head_branch = git::head_branch(dir).await;
    let dirty = git::dirty_file_count(dir).await > 0;
    let entry = inspected_entry(state, project_id, repo);
    let inspected_sha = entry
        .as_ref()
        .and_then(|e| e.sha_for(repo))
        .map(str::to_string);
    let inspected_at = entry.as_ref().map(|e| e.finished_at.clone());
    let (inspected_commit_reachable, newer_commits) = match inspected_sha.as_deref() {
        Some(sha) if git::commit_exists(dir, sha).await => {
            (true, git::commits_between(dir, sha, "HEAD").await)
        }
        _ => (false, None),
    };
    RepoSourceStatus::Present {
        repo: repo.to_string(),
        path,
        head_sha,
        head_branch,
        dirty,
        inspected_sha,
        inspected_at,
        newer_commits,
        inspected_commit_reachable,
        has_deletion_history,
    }
}
