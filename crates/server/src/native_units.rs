//! Native units of work: created and tracked inside Camerata, with no board.
//!
//! A unit of work used to need a GitHub issue to exist: its key was the issue's
//! `OWNER/REPO#N`, and its repository and commit reference were parsed out of that key. A
//! unit now carries its own identity (see [`crate::uow::UnitOfWork`]: `repo`, `external_ref`,
//! `native_number`), so a tracker is one optional adapter. `POST /api/uow/native` creates a
//! unit with no tracker item at all: Camerata keys it `uow-<token>`, numbers it, records the
//! repository it works in, and puts a spine story (title and description, no external link)
//! behind it so the governed run has a spec to read. From there it goes through the same
//! intake, investigation, decision gate, run, branch and commit as an issue-backed unit.
//! Push still needs a remote (and a token to push over HTTPS); nothing needs an issue.
//!
//! Push and PR need the unit's repository as `OWNER/REPO`: the push URL
//! (`https://github.com/OWNER/REPO.git`), the PR API call and the local clone path under the
//! workspace root are all built from it. The one shared validator
//! ([`camerata_api_types::uow::validate_repo_name`]) checks that shape wherever a repository
//! enters a unit (creation here, the Intake repo scope, a unit's repository fallback), so a
//! name like `../../other`, one with whitespace or one with extra separators is refused with
//! a 400 instead of reaching a path or a URL. A native unit with no repository can still be
//! created; it picks one at Intake before it runs.
//!
//! A native unit's commits and PRs refer to it as `uow-<number>` (see
//! [`crate::vcs_choke::config_for_unit`]), never `#<number>`: GitHub would link a bare `#3` to
//! issue or pull request 3 of the repository.
//!
//! The store hands out `uow-` keys only through creation. A request that names an unknown
//! `uow-` key ([`unknown_native_unit_gate`]) is answered 404 and creates nothing, so no
//! number-less phantom unit can appear and be mistaken for a native one.

use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use camerata_worktracker::{CanonicalStory, FeatureStatus};

use crate::AppState;

/// Body for `POST /api/uow/native`.
#[derive(serde::Deserialize)]
pub(crate) struct NativeUnitReq {
    /// The unit's title (required, non-blank).
    title: String,
    /// The unit's description: the spec the investigation and the implementer read.
    #[serde(default)]
    description: String,
    /// The repository the unit works in. Optional: it can also be picked at Intake (Repos in
    /// scope). When the active project lists repositories, it must be one of them.
    #[serde(default)]
    repo: Option<String>,
}

/// `POST /api/uow/native` → `{ uow_id, native_number }`: create a native unit of work in the
/// active project (or unscoped when none is active). 400 when the title is blank or the
/// repository is not one of the active project's.
pub(crate) async fn uow_native_create(
    State(state): State<AppState>,
    Json(req): Json<NativeUnitReq>,
) -> Response {
    let bad = |msg: String| {
        (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": msg })),
        )
            .into_response()
    };
    let title = req.title.trim();
    if title.is_empty() {
        return bad("a unit of work needs a title".to_string());
    }
    let repo = req
        .repo
        .as_deref()
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .map(str::to_string);
    if let Some(repo) = repo.as_deref() {
        if let Err(reason) = camerata_api_types::uow::validate_repo_name(repo) {
            return bad(reason);
        }
    }
    let project = state.projects.active();
    if let (Some(repo), Some(p)) = (repo.as_deref(), project.as_ref()) {
        // The same membership test the run, the branch record and the Intake scope use.
        if !crate::run_target::repo_in_project(repo, Some(p.repos.as_slice())) {
            return bad(format!(
                "`{repo}` is not one of this project's repositories ({})",
                p.repos.join(", ")
            ));
        }
    }

    let uow = match state.uow.create_native(repo, project.map(|p| p.id)) {
        Ok(uow) => uow,
        Err(reason) => return bad(reason),
    };
    let story = CanonicalStory {
        id: uow.story_id.clone(),
        // Native: tracked by Camerata itself, linked to no board.
        external_ref: None,
        title: title.to_string(),
        description: req.description.trim().to_string(),
        status: FeatureStatus::Intake,
        created_by: "camerata".to_string(),
        targets: vec![],
    };
    if let Err(e) = state.stories.upsert(story).await {
        // No spec behind it: do not leave a unit the run cannot read.
        state.uow.delete(&uow.story_id);
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": format!("could not record the unit: {e}") })),
        )
            .into_response();
    }
    Json(serde_json::json!({
        "uow_id": uow.story_id,
        "native_number": uow.native_number,
    }))
    .into_response()
}

/// The unit key a request path addresses: the segment after `/api/uow/` or `/api/stories/`,
/// percent-decoded. `None` for any other path.
fn unit_key_in_path(path: &str) -> Option<String> {
    let rest = path
        .strip_prefix("/api/uow/")
        .or_else(|| path.strip_prefix("/api/stories/"))?;
    let segment = rest.split('/').next()?;
    if segment.is_empty() {
        return None;
    }
    let bytes = segment.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    Some(String::from_utf8_lossy(&out).into_owned())
}

/// Middleware: a request about a unit whose key has the native shape (`uow-...`) but which the
/// store never created is answered 404, whatever the method. Without this, a write such as
/// `POST /api/uow/uow-guess/branch` would materialize a unit with that key (every store write
/// creates a missing unit), a number-less look-alike of a native unit. Keys of other shapes
/// keep their long-standing create-on-write behaviour.
pub(crate) async fn unknown_native_unit_gate(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    if let Some(key) = unit_key_in_path(request.uri().path()) {
        if key.starts_with("uow-") && state.uow.get(&key).is_none() {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({
                    "error": "no such unit of work",
                    "story_id": key,
                })),
            )
                .into_response();
        }
    }
    next.run(request).await
}

#[cfg(test)]
#[path = "native_units_tests.rs"]
mod tests;
