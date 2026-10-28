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

use axum::extract::State;
use axum::http::StatusCode;
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
    let project = state.projects.active();
    if let (Some(repo), Some(p)) = (repo.as_deref(), project.as_ref()) {
        if !p.repos.is_empty() && !p.repos.iter().any(|r| r == repo) {
            return bad(format!(
                "`{repo}` is not one of this project's repositories ({})",
                p.repos.join(", ")
            ));
        }
    }

    let uow = state.uow.create_native(repo, project.map(|p| p.id));
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

#[cfg(test)]
#[path = "native_units_tests.rs"]
mod tests;
