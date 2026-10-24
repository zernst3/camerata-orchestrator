//! The HTTP surface of source status and deletion records.

use axum::extract::{Path, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::Json;

use super::status::source_status;
use super::Refusal;
use crate::AppState;

fn unknown_project(id: &str) -> Refusal {
    Refusal::not_found("unknown_project", format!("project not found: {id}"))
}

/// `GET /api/projects/:id/source` — the source status of every repo, in `project.repos` order.
pub async fn get_source(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let Some(project) = state.projects.get(&id) else {
        return unknown_project(&id).into_response();
    };
    let mut repos = Vec::with_capacity(project.repos.len());
    for repo in &project.repos {
        repos.push(source_status(&state, &project.id, repo).await);
    }
    // The current inspection, for the scan surface's header ("Delta retest of changes since
    // <sha>"): which kind it is and when it finished. `null` before the first inspection.
    let inspection = state.inspections.latest(&project.id).map(|e| {
        serde_json::json!({
            "id": e.id,
            "kind": e.kind,
            "finished_at": e.finished_at,
        })
    });
    Json(serde_json::json!({ "ok": true, "repos": repos, "inspection": inspection }))
        .into_response()
}

/// `GET /api/projects/:id/source/records` — every deletion and re-fetch record, newest first.
pub async fn get_records(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    if state.projects.get(&id).is_none() {
        return unknown_project(&id).into_response();
    }
    Json(serde_json::json!({
        "ok": true,
        "records": state.source_records.deletions(&id),
        "refetches": state.source_records.refetches(&id),
    }))
    .into_response()
}

/// `GET /api/projects/:id/source/records/:record_id/text` — one deletion record as the plain
/// text the client receives ("Save record as .txt").
pub async fn get_record_text(
    State(state): State<AppState>,
    Path((id, record_id)): Path<(String, String)>,
) -> Response {
    if state.projects.get(&id).is_none() {
        return unknown_project(&id).into_response();
    }
    match state.source_records.deletion(&id, &record_id) {
        Some(record) => (
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            record.render_text(),
        )
            .into_response(),
        None => Refusal::not_found(
            "unknown_record",
            format!("no deletion record {record_id} in this project"),
        )
        .into_response(),
    }
}
