//! Reading client code for display: the ONE resolver every code-showing endpoint uses.
//!
//! The finding modal reads the lines around a finding through this today; the code viewer's
//! file, symbol and batch endpoints will read through it tomorrow (docs/design/2026-10-24_
//! delete-and-refetch-client-source.md, section 9). It decides WHERE code may be read from:
//!
//! - the **inspected commit**, through git objects, whenever the inspection recorded one, so a
//!   finding keeps showing the code the inspection saw even though the checkout has since moved
//!   on (or been re-fetched at the latest commit);
//! - the **working tree**, labelled as such, only when no commit is known (the scan ran on a
//!   folder without git history);
//! - **nothing**, when the source was deleted: the answer is "deleted on <date>", never a silent
//!   fallback to a folder that is not there.

use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;

use super::git::{head_sha, is_full_sha};
use super::status::{deleted_record, inspected_entry, resolve_dir};
use super::Refusal;
use crate::onboard::finding_context::{lookup_at, FindingContextOutcome};
use crate::onboard::source_file::Origin;
use crate::AppState;

/// Mark a response that carries client code lines as never to be cached. The desktop webview's
/// HTTP cache would otherwise keep the lines of a file the operator has since deleted.
pub fn no_store(mut response: Response) -> Response {
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response
}

/// Query of `GET /api/onboard/finding-context`.
#[derive(Debug, serde::Deserialize)]
pub struct FindingContextParams {
    /// `owner/repo`.
    pub repo: String,
    /// Repo-relative path, as on the finding.
    pub path: String,
    /// 1-based violation line.
    pub line: usize,
    /// First line of the stored snippet, for the staleness check.
    #[serde(default)]
    pub expect: Option<String>,
    /// The project the finding belongs to; defaults to the active project, then any project
    /// that has the repo.
    #[serde(default)]
    pub project: Option<String>,
    /// Read this commit instead of the inspected one (40 hex characters).
    #[serde(default)]
    pub sha: Option<String>,
}

/// Add `"source": "git" | "working_tree"` (and, for git, the commit and whether the checkout has
/// moved on) to an outcome's JSON body.
pub(crate) fn labelled(
    outcome: &FindingContextOutcome,
    source: &str,
    git: Option<(&str, bool)>,
) -> serde_json::Value {
    let mut body = outcome.to_json();
    if let Some(map) = body.as_object_mut() {
        map.insert("source".to_string(), serde_json::json!(source));
        if let Some((sha, head_differs)) = git {
            map.insert("commit".to_string(), serde_json::json!(sha));
            map.insert("head_differs".to_string(), serde_json::json!(head_differs));
        }
    }
    body
}

/// `GET /api/onboard/finding-context`: the enclosing code block of one finding, read from the
/// inspected commit (or labelled as the working tree), or the reason it cannot be shown. See the
/// module docs and [`SourceRead`]. Always `Cache-Control: no-store`.
///
/// Invariant: when the repo's folder does not exist this never touches the filesystem and never
/// answers `source: "working_tree"`.
pub async fn finding_context(state: &AppState, params: FindingContextParams) -> Response {
    let read = match source_for_read(
        state,
        params.project.as_deref(),
        &params.repo,
        params.sha.as_deref(),
    )
    .await
    {
        Ok(read) => read,
        Err(refusal) => return no_store(refusal.into_response()),
    };
    // A file the project's exclusion list names is not shown: the inspection did not read it.
    if matches!(
        read,
        SourceRead::Git { .. } | SourceRead::WorkingTree { .. }
    ) && crate::onboard::scope::scope_for_read(state, params.project.as_deref())
        .excludes(&params.repo, &params.path)
    {
        return no_store(
            (
                StatusCode::OK,
                Json(FindingContextOutcome::ExcludedByAgreement.to_json()),
            )
                .into_response(),
        );
    }
    let expect = params.expect.as_deref();
    let (status, body) = match read {
        SourceRead::Deleted {
            deleted_at,
            record_id,
        } => (
            StatusCode::OK,
            FindingContextOutcome::SourceDeleted {
                deleted_at,
                record_id,
            }
            .to_json(),
        ),
        // Not linked / no local folder: its own status, with the reason in plain words. The UI
        // keeps `file_missing` for a file really absent at a resolved folder.
        SourceRead::Unresolved { reason } => (
            StatusCode::OK,
            FindingContextOutcome::RepoUnlinked { reason }.to_json(),
        ),
        SourceRead::WorkingTree { dir } => {
            let outcome =
                lookup_at(Origin::WorkingTree(&dir), &params.path, params.line, expect).await;
            (
                status_of(&outcome),
                labelled(&outcome, "working_tree", None),
            )
        }
        SourceRead::Git {
            dir,
            sha,
            head_differs,
        } => {
            let outcome = lookup_at(
                Origin::Git {
                    dir: &dir,
                    sha: &sha,
                },
                &params.path,
                params.line,
                expect,
            )
            .await;
            if outcome == FindingContextOutcome::CommitUnavailable {
                // The one labelled fallback, and only while the folder exists (it does: the
                // resolver checked): show the working tree, loudly.
                let fallback =
                    lookup_at(Origin::WorkingTree(&dir), &params.path, params.line, expect).await;
                let mut body = labelled(&outcome, "working_tree", None);
                if matches!(fallback, FindingContextOutcome::Ok { .. }) {
                    let shown = labelled(&fallback, "working_tree", None);
                    if let (Some(map), Some(lines)) = (body.as_object_mut(), shown.as_object()) {
                        // The working-tree block rides along; `status` stays `commit_unavailable`.
                        for (key, value) in lines.iter().filter(|(k, _)| k.as_str() != "status") {
                            map.insert(key.clone(), value.clone());
                        }
                    }
                }
                (StatusCode::OK, body)
            } else {
                (
                    status_of(&outcome),
                    labelled(&outcome, "git", Some((&sha, head_differs))),
                )
            }
        }
    };
    no_store((status, Json(body)).into_response())
}

/// Path traversal is the only outcome that is not a 200.
fn status_of(outcome: &FindingContextOutcome) -> StatusCode {
    if *outcome == FindingContextOutcome::PathTraversal {
        StatusCode::BAD_REQUEST
    } else {
        StatusCode::OK
    }
}

/// Where a repo's code may be read from right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceRead {
    /// Read through git objects at `sha`. `head_differs` is true when the checkout has moved on
    /// from that commit (the UI then notes "References commit <sha>; source on disk is newer").
    Git {
        dir: std::path::PathBuf,
        sha: String,
        head_differs: bool,
    },
    /// No commit is known: read the working tree, and say so.
    WorkingTree { dir: std::path::PathBuf },
    /// The source was deleted and not fetched back.
    Deleted {
        deleted_at: String,
        record_id: String,
    },
    /// No folder and no deletion on record (never linked, or an imported project).
    Unresolved { reason: String },
}

/// Whether `project` owns `repo` for reading its code: the repo is in `project.repos`, or one of
/// the project's own saved inspections audited it. Onboarding adds a repo to `project.repos` only
/// when the rules are applied, which is after triage, so a project still in onboarding owns its
/// repos through its inspections. The ONE ownership test every code-reading endpoint uses.
pub(crate) fn project_owns_repo(
    state: &AppState,
    project: &crate::project::Project,
    repo: &str,
) -> bool {
    project.repos.iter().any(|r| r.eq_ignore_ascii_case(repo))
        || state.inspections.any_audits(&project.id, repo)
}

/// The project that owns `repo` for a read: the named one (and only that one), else the active
/// project if it owns the repo, else the first project that does. See [`project_owns_repo`].
fn owning_project(state: &AppState, project_hint: Option<&str>, repo: &str) -> Option<String> {
    let owns = |p: &crate::project::Project| project_owns_repo(state, p, repo);
    if let Some(id) = project_hint {
        return state.projects.get(id).filter(|p| owns(p)).map(|p| p.id);
    }
    // The project that holds the deletion record answers first: its record is what says the
    // source was deleted, so a project that merely inspected the repo must not shadow it.
    let owners: Vec<crate::project::Project> = state
        .projects
        .list()
        .into_iter()
        .filter(|p| owns(p))
        .collect();
    owners
        .iter()
        .find(|p| deleted_record(state, &p.id, repo).is_some())
        .or_else(|| {
            state
                .projects
                .active()
                .and_then(|a| owners.iter().find(|p| p.id == a.id))
        })
        .or_else(|| owners.first())
        .map(|p| p.id.clone())
}

/// Every repo `project` owns: its own `repos` plus every repo its saved inspections audited
/// (a plain `owner/name` only), without duplicates (case-insensitive). The ONE answer deletion
/// uses for "this project's repos", matching [`project_owns_repo`].
pub(crate) fn owned_repos(state: &AppState, project: &crate::project::Project) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for r in &project.repos {
        if !out.iter().any(|o| o.eq_ignore_ascii_case(r)) {
            out.push(r.clone());
        }
    }
    for r in state.inspections.audited_repos(&project.id) {
        if is_owner_name(&r) && !out.iter().any(|o| o.eq_ignore_ascii_case(&r)) {
            out.push(r);
        }
    }
    out
}

/// The OTHER projects that use `repo`: it is in their `repos` or one of their saved inspections
/// audited it ([`project_owns_repo`]). A checkout any of them uses is not this project's to
/// remove.
pub(crate) fn other_projects_using(
    state: &AppState,
    project_id: &str,
    repo: &str,
) -> Vec<crate::project::Project> {
    state
        .projects
        .list()
        .into_iter()
        .filter(|p| p.id != project_id)
        .filter(|p| project_owns_repo(state, p, repo))
        .collect()
}

/// Whether `repo` is exactly `owner/name` made of the characters a GitHub login and repository
/// name can contain: one `/`, no empty segment, no `.` or `..` segment, no separator or NUL.
pub fn is_owner_name(repo: &str) -> bool {
    let mut parts = repo.split('/');
    let (Some(owner), Some(name), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    let segment_ok = |seg: &str| {
        !seg.is_empty()
            && seg != "."
            && seg != ".."
            && seg
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    };
    segment_ok(owner) && segment_ok(name)
}

/// A caller's explicit commit must be 40 hex characters, else a 400 `invalid_sha`. Checked before
/// anything is resolved, so a malformed one starts no process; a request that carries many
/// (the batch) checks them all up front with this.
pub fn check_sha(sha: Option<&str>) -> Result<(), Refusal> {
    match sha {
        Some(sha) if !is_full_sha(sha) => Err(Refusal {
            status: StatusCode::BAD_REQUEST,
            code: "invalid_sha",
            message: "sha must be a full 40-character commit id.".to_string(),
        }),
        _ => Ok(()),
    }
}

/// Decide where `repo`'s code is read from. `sha` is the caller's explicit commit (validated as
/// 40 hex characters, else a 400), defaulting to the commit the project's latest inspection that
/// covered the repo recorded.
pub async fn source_for_read(
    state: &AppState,
    project_hint: Option<&str>,
    repo: &str,
    sha: Option<&str>,
) -> Result<SourceRead, Refusal> {
    check_sha(sha)?;
    // The repo name becomes a path under the workspace root, so it must be a plain `owner/name`
    // that a project owns. A traversal ("../../etc"), an absolute path or a repo no project has
    // is refused here, before any directory is built or any file is read: the path jail of the
    // readers is relative to the directory this resolves, so it cannot catch this itself.
    // The name is checked first, so a junk name never triggers the inspection scan.
    let owner = if is_owner_name(repo) {
        owning_project(state, project_hint, repo)
    } else {
        None
    };
    let project_id = match owner {
        Some(id) => id,
        None => {
            return Ok(SourceRead::Unresolved {
                reason: if project_hint.is_some() {
                    format!("{repo} is not linked to this project, so its code cannot be shown.")
                } else {
                    format!("{repo} is not linked to any project, so its code cannot be shown.")
                },
            })
        }
    };

    // Deleted wins over everything: the directory is gone by construction.
    // A project that only inspected the repo has no record of its own: another project that owns
    // the repo may have deleted it, and the checkout is gone for both.
    let record = deleted_record(state, &project_id, repo).or_else(|| {
        other_projects_using(state, &project_id, repo)
            .iter()
            .find_map(|p| deleted_record(state, &p.id, repo))
    });
    if let Some(record) = record {
        return Ok(SourceRead::Deleted {
            deleted_at: record.completed_at,
            record_id: record.id,
        });
    }

    let Some(dir) = resolve_dir(state, repo) else {
        return Ok(SourceRead::Unresolved {
            reason: format!(
                "{repo} has no local folder: no folder is set for it and no workspace folder is configured. Set its folder in Settings."
            ),
        });
    };
    if !dir.is_dir() {
        return Ok(SourceRead::Unresolved {
            reason: format!(
                "{repo} has no local folder at {}. Set its folder in Settings or fetch the repo again.",
                dir.display()
            ),
        });
    }

    let sha = match sha {
        Some(s) => Some(s.to_string()),
        None => inspected_entry(state, &project_id, repo)
            .and_then(|e| e.sha_for(repo).map(str::to_string))
            .filter(|s| is_full_sha(s)),
    };
    Ok(match sha {
        Some(sha) => {
            let head_differs = head_sha(&dir).await.is_none_or(|head| head != sha);
            SourceRead::Git {
                dir,
                sha,
                head_differs,
            }
        }
        None => SourceRead::WorkingTree { dir },
    })
}
