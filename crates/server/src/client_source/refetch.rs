//! Re-fetching a repo's source after it was deleted: a FULL-history clone of the latest code (or
//! of the commit the inspection read), then the usual "N commits newer than the inspection"
//! status. Remediation and the monthly delta retest need the latest code; reproducing or
//! disputing a finding needs the inspected commit; both come from here.
//!
//! Full history is deliberate (never `--depth`): the inspected commit must always be reachable,
//! or finding code views and the delta rescan would silently lose their anchor.

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;

use super::git;
use super::records::{now_rfc3339, RefetchRecord};
use super::remote::{
    clone_full_from, fetch_commit_from, probe_remote_from, renamed_to, RefetchError,
};
use super::status::{project_and_repo, resolve_dir, source_status, RepoSourceStatus};
use super::Refusal;
use crate::workspace;
use crate::AppState;

/// Which code to bring back.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RefetchTarget {
    /// The default branch's head.
    Latest,
    /// Exactly this commit (40 hex characters), checked out detached.
    Commit { sha: String },
}

/// Body of `POST .../source/refetch`.
#[derive(Debug, Clone, Deserialize)]
pub struct RefetchRequest {
    pub target: RefetchTarget,
}

/// What a successful re-fetch produced.
#[derive(Debug)]
pub struct RefetchOutcome {
    pub status: RepoSourceStatus,
    /// GitHub now calls the repository this; the project still records the old name.
    pub renamed_to: Option<String>,
    /// The inspected commit was asked for but the remote does not serve it; the latest source
    /// was fetched instead. Carries the commit that was asked for.
    pub commit_not_on_remote: Option<String>,
}

/// The operator-facing refusal for a failed network git step.
pub fn refusal_for(error: RefetchError, repo: &str) -> Refusal {
    match error {
        RefetchError::AuthFailed => Refusal::conflict(
            "auth_failed",
            format!(
                "GitHub refused Camerata's token for {repo}. The token may be revoked or lack \
                 access to this repository."
            ),
        ),
        RefetchError::RepoNotFound => Refusal::conflict(
            "repo_not_found",
            format!(
                "GitHub has no repository at {repo}. It was deleted, renamed, or made private to \
                 this token."
            ),
        ),
        RefetchError::Network(message) => Refusal {
            status: axum::http::StatusCode::BAD_GATEWAY,
            code: "network",
            message,
        },
    }
}

fn no_token() -> Refusal {
    Refusal::conflict(
        "no_token",
        "No GitHub token is set. Add one under Credentials.",
    )
}

/// What the re-fetch dialog shows before the operator chooses.
#[derive(Debug, serde::Serialize)]
pub struct RemoteInfo {
    pub default_branch: Option<String>,
    pub head_sha: String,
    /// Filled in after a clone; the dialog shows the short sha until then.
    pub head_date: Option<String>,
}

/// Ask the remote for its default branch and head (`git ls-remote --symref`, no API call).
pub async fn probe_remote(
    state: &AppState,
    project_id: &str,
    repo_param: &str,
) -> Result<RemoteInfo, Refusal> {
    let (_, repo) = project_and_repo(state, project_id, repo_param)?;
    let token = state.github_token().ok_or_else(no_token)?;
    let url = state.git_remotes.fetch_url(&repo, &token);
    let head = probe_remote_from(&url, &repo, &token)
        .await
        .map_err(|e| refusal_for(e, &repo))?;
    Ok(RemoteInfo {
        default_branch: head.default_branch,
        head_sha: head.head_sha,
        head_date: None,
    })
}

/// A destination that may be cloned into: absent, or an existing EMPTY directory.
fn destination_is_free(dir: &std::path::Path) -> bool {
    match std::fs::read_dir(dir) {
        Ok(mut entries) => entries.next().is_none(),
        Err(e) => e.kind() == std::io::ErrorKind::NotFound,
    }
}

/// Re-fetch the source of `repo`, requiring the configured disk headroom
/// (`CAMERATA_MIN_DISK_HEADROOM_GB`, default 10 GiB). See the module docs and the design,
/// section 7.
pub async fn refetch_source(
    state: &AppState,
    project_id: &str,
    repo_param: &str,
    request: &RefetchRequest,
) -> Result<RefetchOutcome, Refusal> {
    refetch_source_with_headroom(
        state,
        project_id,
        repo_param,
        request,
        workspace::disk_headroom_threshold_bytes(),
    )
    .await
}

/// [`refetch_source`] with the headroom requirement passed in, so a test can demand an
/// impossible amount without touching the process environment.
pub async fn refetch_source_with_headroom(
    state: &AppState,
    project_id: &str,
    repo_param: &str,
    request: &RefetchRequest,
    min_headroom_bytes: u64,
) -> Result<RefetchOutcome, Refusal> {
    let (_, repo) = project_and_repo(state, project_id, repo_param)?;
    if let RefetchTarget::Commit { sha } = &request.target {
        if !git::is_full_sha(sha) {
            return Err(Refusal::bad_request(
                "invalid_sha",
                "sha must be a full 40-character commit id.",
            ));
        }
    }

    // 1. Preconditions.
    match source_status(state, project_id, &repo).await {
        RepoSourceStatus::Present { path, .. } => {
            return Err(Refusal::conflict(
                "already_present",
                format!(
                    "{repo} is already on disk at {path}; use Clone / update or the git panel."
                ),
            ));
        }
        RepoSourceStatus::Deleted { .. }
        | RepoSourceStatus::Unresolved { .. }
        | RepoSourceStatus::Mismatch { .. } => {}
    }
    let token = state.github_token().ok_or_else(no_token)?;
    if state.jobs.has_running_for_project(project_id) || state.source_ops.scan_running(project_id) {
        return Err(Refusal::conflict(
            "scan_in_progress",
            "A scan is running for this project. Wait for it to finish or cancel it first.",
        ));
    }
    let Some(_claim) = state.source_ops.try_begin(project_id, &repo) else {
        return Err(Refusal::conflict(
            "scan_in_progress",
            format!("Another operation on {repo} is already running. Wait for it to finish."),
        ));
    };

    // 2. Destination: the recorded override, else the workspace layout.
    let Some(dest) = resolve_dir(state, &repo) else {
        return Err(Refusal::conflict(
            "no_path",
            "Pick a workspace folder first, then re-fetch.",
        ));
    };
    if !destination_is_free(&dest) {
        return Err(Refusal::conflict(
            "destination_not_empty",
            format!(
                "{} already contains files; move them aside or link that folder instead.",
                dest.display()
            ),
        ));
    }
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|e| {
            Refusal::conflict(
                "destination_not_empty",
                format!("Could not create {}: {e}.", parent.display()),
            )
        })?;

        // 3. Headroom, measured where the clone lands.
        if let Err(e) = workspace::ensure_disk_headroom(parent, min_headroom_bytes) {
            return Err(Refusal::conflict("no_disk_headroom", e.to_string()));
        }
    }

    // 4. Clone, full history.
    let url = state.git_remotes.fetch_url(&repo, &token);
    clone_full_from(&url, &repo, &dest, &token)
        .await
        .map_err(|e| refusal_for(e, &repo))?;

    // 5. Check out the requested target.
    let mut commit_not_on_remote = None;
    let target_label = match &request.target {
        RefetchTarget::Latest => "latest".to_string(),
        RefetchTarget::Commit { sha } => {
            let have = git::commit_exists(&dest, sha).await
                || fetch_commit_from(&dest, &url, sha, &repo, &token).await;
            if have {
                let out = workspace::git(Some(&dest), &["checkout", "--detach", "-q", sha])
                    .await
                    .map_err(|e| Refusal::conflict("network", format!("couldn't run git: {e}")))?;
                if !out.status.success() {
                    return Err(Refusal::conflict(
                        "network",
                        format!("git checkout failed: {}", workspace::stderr_of(&out)),
                    ));
                }
                format!("commit:{sha}")
            } else {
                // The clone stays at the latest source; say plainly that the commit is gone.
                commit_not_on_remote = Some(sha.clone());
                "latest".to_string()
            }
        }
    };

    // 6. Rename detection: GitHub redirects clones silently.
    let renamed = renamed_to(
        &repo,
        state
            .git_remotes
            .current_full_name(&repo, &token)
            .await
            .as_deref(),
    );

    // 7. Record and report.
    let record = RefetchRecord {
        seq: 0,
        repo: repo.clone(),
        path: dest.to_string_lossy().into_owned(),
        sha: git::head_sha(&dest).await,
        target: target_label,
        completed_at: now_rfc3339(),
    };
    if let Err(e) = state.source_records.add_refetch(project_id, record) {
        tracing::warn!(error = %e, "could not persist the re-fetch record");
    }
    Ok(RefetchOutcome {
        status: source_status(state, project_id, &repo).await,
        renamed_to: renamed,
        commit_not_on_remote,
    })
}

/// `GET /api/projects/:id/repos/:repo/source/remote` — the dialog's probe.
pub async fn remote_probe(
    State(state): State<AppState>,
    Path((id, repo)): Path<(String, String)>,
) -> Response {
    match probe_remote(&state, &id, &repo).await {
        Ok(info) => Json(serde_json::json!({
            "ok": true,
            "default_branch": info.default_branch,
            "head_sha": info.head_sha,
            "head_date": info.head_date,
        }))
        .into_response(),
        Err(refusal) => refusal.into_response(),
    }
}

/// `POST /api/projects/:id/repos/:repo/source/refetch`.
pub async fn refetch(
    State(state): State<AppState>,
    Path((id, repo)): Path<(String, String)>,
    Json(request): Json<RefetchRequest>,
) -> Response {
    match refetch_source(&state, &id, &repo, &request).await {
        Err(refusal) => refusal.into_response(),
        Ok(outcome) => match outcome.commit_not_on_remote {
            // The latest clone IS in place; the dialog refreshes the card and shows why the
            // requested commit could not be used.
            Some(sha) => Json(serde_json::json!({
                "ok": false,
                "code": "commit_not_on_remote",
                "message": format!(
                    "The inspected commit {sha} is not reachable from any branch or tag on GitHub \
                     (history was rewritten). The latest source was fetched instead; finding code \
                     views for this inspection are unavailable."
                ),
                "status": outcome.status,
                "renamed_to": outcome.renamed_to,
            }))
            .into_response(),
            None => Json(serde_json::json!({
                "ok": true,
                "status": outcome.status,
                "renamed_to": outcome.renamed_to,
            }))
            .into_response(),
        },
    }
}
