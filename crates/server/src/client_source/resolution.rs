//! Where "the repo must exist" stops being true.
//!
//! Before source deletion every surface asked one question of a repo: does it resolve to a git
//! clone on this disk? If not, the project was "paused" and the whole Onboard surface (findings,
//! review, exports) disappeared behind a "link the repo" prompt. A repo whose source Camerata
//! deleted on purpose must not do that: the delivered inspection has to stay fully usable with
//! no code on disk. This module answers the question for the readiness gate, the repo-health
//! banner and the Workspace cards from ONE place, and holds the gate that turns away the
//! operations that genuinely need the code.

use axum::extract::{Request, State};
use axum::http::Uri;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

use super::records::{long_date, DeletionRecord};
use super::status::{deleted_record, source_status, RepoSourceStatus};
use super::Refusal;
use crate::project::Project;
use crate::workspace::{self, RepoCheckout, RepoResolution};
use crate::AppState;
use camerata_app_core::readiness::RepoLink;

/// The filesystem resolution of `repo`, with the deletion records folded in: `source_deleted`
/// and `deleted_at` are set exactly when the source was deleted on purpose and not fetched back.
pub async fn resolve_with_records(
    state: &AppState,
    project: &Project,
    repo: &str,
) -> RepoResolution {
    let mut resolution = workspace::repo_resolution(
        state.settings.repo_path(repo).as_deref(),
        state.settings.workspace_root().as_deref(),
        repo,
    )
    .await;
    if !resolution.resolved {
        if let Some(record) = deleted_record(state, &project.id, repo) {
            resolution.source_deleted = true;
            resolution.reason = format!(
                "source deleted on {}; use Re-fetch source to restore it",
                long_date(&record.completed_at)
            );
            resolution.deleted_at = Some(record.completed_at);
        }
    }
    resolution
}

/// [`resolve_with_records`] for every repo of the project, in order.
pub async fn resolve_all(state: &AppState, project: &Project) -> Vec<RepoResolution> {
    let mut out = Vec::with_capacity(project.repos.len());
    for repo in &project.repos {
        out.push(resolve_with_records(state, project, repo).await);
    }
    out
}

/// What the readiness gate sees for one resolution.
pub fn link_of(resolution: &RepoResolution) -> RepoLink {
    if resolution.resolved {
        RepoLink::Resolved
    } else if resolution.source_deleted {
        RepoLink::Deleted
    } else {
        RepoLink::Unresolved
    }
}

/// One repo's checkout status plus its source status: everything the Workspace card renders.
/// The checkout fields are flattened in, so the JSON is a superset of the old `RepoCheckout`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RepoCheckoutView {
    #[serde(flatten)]
    pub checkout: RepoCheckout,
    pub source: RepoSourceStatus,
    pub source_deleted: bool,
    pub deleted_at: Option<String>,
}

impl RepoCheckoutView {
    /// Combine a filesystem checkout status with the repo's source status. A deleted repo reads
    /// "source deleted on <date>; use Re-fetch source" instead of "not cloned yet".
    pub fn new(mut checkout: RepoCheckout, source: RepoSourceStatus) -> Self {
        let deleted_at = source.deleted().map(|(at, _)| at.to_string());
        if let Some(at) = &deleted_at {
            checkout.detail = format!("source deleted on {}; use Re-fetch source", long_date(at));
        }
        Self {
            checkout,
            source_deleted: deleted_at.is_some(),
            deleted_at,
            source,
        }
    }
}

/// The checkout views of every repo of the project (the Workspace tab's data).
pub async fn checkout_views(state: &AppState, project: &Project) -> Vec<RepoCheckoutView> {
    let mut out = Vec::with_capacity(project.repos.len());
    for repo in &project.repos {
        let checkout = workspace::checkout_status_resolved(
            state.settings.repo_path(repo).as_deref(),
            state.settings.workspace_root().as_deref(),
            repo,
        )
        .await;
        let source = source_status(state, &project.id, repo).await;
        out.push(RepoCheckoutView::new(checkout, source));
    }
    out
}

// ── The gate for operations that need the source ─────────────────────────────────────────────

/// The 409 for an operation that cannot run because a repo's source is deleted.
pub fn source_deleted_refusal(repo: &str, deleted_at: &str) -> Refusal {
    Refusal::conflict(
        "source_deleted",
        format!(
            "Source deleted on {}. Re-fetch source to rescan. ({repo})",
            long_date(deleted_at)
        ),
    )
}

/// The repos an operation's JSON request body names: `repos`, `repo`, `rules[].repos` and
/// `findings[].repo` (the shapes of the scan, audit, apply, arm and open-PR requests). Pure.
pub fn scope_repos(body: &[u8]) -> Vec<String> {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(body) else {
        return Vec::new();
    };
    let mut out: Vec<String> = Vec::new();
    let mut push_strings = |v: Option<&serde_json::Value>| {
        if let Some(arr) = v.and_then(|v| v.as_array()) {
            out.extend(arr.iter().filter_map(|s| s.as_str()).map(str::to_string));
        }
    };
    push_strings(value.get("repos"));
    if let Some(rules) = value.get("rules").and_then(|r| r.as_array()) {
        for rule in rules {
            push_strings(rule.get("repos"));
        }
    }
    if let Some(repo) = value.get("repo").and_then(|r| r.as_str()) {
        out.push(repo.to_string());
    }
    if let Some(findings) = value.get("findings").and_then(|f| f.as_array()) {
        out.extend(
            findings
                .iter()
                .filter_map(|f| f.get("repo").and_then(|r| r.as_str()))
                .map(str::to_string),
        );
    }
    out.sort();
    out.dedup();
    out
}

/// `/api/projects/<id>/...` -> the id.
fn project_id_in_path(uri: &Uri) -> Option<String> {
    let mut segments = uri.path().split('/').filter(|s| !s.is_empty());
    (segments.next()? == "api" && segments.next()? == "projects")
        .then(|| segments.next().map(str::to_string))
        .flatten()
}

/// The deletion record that still stands for `repo` in ANY project that has it, as
/// `(project_id, record)`. A repo's folder is per machine, not per project, so a deletion
/// recorded against one project means the folder is gone for every project that lists the repo.
pub(crate) fn deleted_in_any_project(
    state: &AppState,
    repo: &str,
) -> Option<(String, DeletionRecord)> {
    state.projects.list().into_iter().find_map(|project| {
        let spelled = project
            .repos
            .iter()
            .find(|r| r.eq_ignore_ascii_case(repo.trim()))?;
        deleted_record(state, &project.id, spelled).map(|record| (project.id.clone(), record))
    })
}

/// The first repo in scope whose source is deleted, as `(repo, deleted_at)`.
///
/// Scope is the repos the request names that belong to the project (a request that names none, a
/// project-level rule set, is scoped to ALL of the project's repos), PLUS every repo the request
/// names that belongs to any other project: a deletion recorded against another project removes
/// the same folder (owner decision, 2026-10-24).
pub fn first_deleted_in_scope(
    state: &AppState,
    project_id: Option<&str>,
    named: &[String],
) -> Option<(String, String)> {
    let project = match project_id {
        Some(id) => state.projects.get(id),
        None => state.projects.active(),
    };
    let mut candidates: Vec<String> = Vec::new();
    if let Some(project) = &project {
        let in_scope: Vec<&String> = project
            .repos
            .iter()
            .filter(|r| named.iter().any(|n| n.eq_ignore_ascii_case(r)))
            .collect();
        if in_scope.is_empty() {
            candidates.extend(project.repos.iter().cloned());
        } else {
            candidates.extend(in_scope.into_iter().cloned());
        }
    }
    candidates.extend(named.iter().cloned());
    candidates.into_iter().find_map(|repo| {
        deleted_in_any_project(state, &repo).map(|(_, record)| (repo, record.completed_at))
    })
}

/// Where a handler may let `apply_local` clone `repo` when it is not on disk: the workspace root,
/// or `None` for a repo with a per-repo path override (never clone over a folder the architect
/// chose by hand). `Err` for a repo whose source was deleted: the route must report the refusal
/// instead of cloning, so no emit or apply can bring deleted source back as a side effect.
pub(crate) fn clone_root_for<'a>(
    state: &AppState,
    repo: &str,
    override_path: Option<&str>,
    workspace_root: Option<&'a str>,
) -> Result<Option<&'a std::path::Path>, Refusal> {
    if let Some((_, record)) = deleted_in_any_project(state, repo) {
        return Err(source_deleted_refusal(repo, &record.completed_at));
    }
    if override_path.is_some_and(|p| !p.trim().is_empty()) {
        return Ok(None);
    }
    Ok(workspace_root.map(std::path::Path::new))
}

/// Axum middleware for the routes that scan or write into a repo: answer `409 source_deleted`
/// BEFORE the handler resolves any source folder (without it they would quietly note "not a
/// local git clone" and scan nothing). Reads the JSON body to learn the scope, then hands the
/// untouched bytes on.
pub async fn scope_gate(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let (parts, body) = request.into_parts();
    let bytes = match axum::body::to_bytes(body, 64 * 1024 * 1024).await {
        Ok(b) => b,
        Err(_) => {
            return Refusal::bad_request("body_too_large", "The request body is too large.")
                .into_response();
        }
    };
    let named = scope_repos(&bytes);
    let project_id = project_id_in_path(&parts.uri);
    if let Some((repo, at)) = first_deleted_in_scope(&state, project_id.as_deref(), &named) {
        return source_deleted_refusal(&repo, &at).into_response();
    }
    next.run(Request::from_parts(parts, axum::body::Body::from(bytes)))
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_repos_reads_every_request_shape() {
        let scan = br#"{"repos":["a/b","a/c"],"repo":"a/d"}"#;
        assert_eq!(scope_repos(scan), ["a/b", "a/c", "a/d"]);
        let arm = br#"{"rules":[{"id":"X","repos":["a/b"]},{"id":"Y","repos":[]}],"findings":[{"repo":"a/c"},{"repo":"a/b"}]}"#;
        assert_eq!(scope_repos(arm), ["a/b", "a/c"]);
        assert!(scope_repos(br#"{"overrides":{"R":"opt"}}"#).is_empty());
        assert!(scope_repos(b"not json").is_empty());
        assert!(scope_repos(b"").is_empty());
    }

    #[test]
    fn the_project_id_is_read_from_a_project_scoped_path_only() {
        let uri = |p: &str| p.parse::<Uri>().unwrap();
        assert_eq!(
            project_id_in_path(&uri("/api/projects/proj-3/rescan-alternatives")).as_deref(),
            Some("proj-3")
        );
        assert_eq!(project_id_in_path(&uri("/api/onboard/scan")), None);
        assert_eq!(project_id_in_path(&uri("/api/projects")), None);
    }

    #[test]
    fn link_of_reads_the_resolution() {
        let base = |resolved, deleted| RepoResolution {
            repo: "a/b".to_string(),
            path: None,
            resolved,
            reason: String::new(),
            source_deleted: deleted,
            deleted_at: None,
        };
        assert_eq!(link_of(&base(true, false)), RepoLink::Resolved);
        assert_eq!(link_of(&base(false, true)), RepoLink::Deleted);
        assert_eq!(link_of(&base(false, false)), RepoLink::Unresolved);
    }
}
