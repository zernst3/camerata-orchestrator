//! `GET /api/projects/:id/rewrite-advisory`: the operator's rewrite advisory for the project's
//! latest inspection, for the cockpit's internal panel.
//!
//! OPERATOR ONLY, like the file `camerata inspect` writes beside the failure report. The answer
//! is [`crate::rewrite_advisory::compute`] over the inspection as it was stored at the end of the
//! run (the same pure function the command line writes to disk, so the two say the same), never
//! over the reviewer's decisions, and nothing here reaches an export: the export request has no
//! advisory field and the release gate refuses any package that names one.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use camerata_api_types::review_view::ReviewViewError;

use crate::rewrite_advisory::{compute, AdvisoryDocument};
use crate::AppState;

/// The route, for the router to merge in.
pub(crate) fn rewrite_advisory_routes() -> Router<AppState> {
    Router::new().route(
        "/api/projects/:id/rewrite-advisory",
        get(get_rewrite_advisory),
    )
}

fn refusal(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(ReviewViewError {
            ok: false,
            code: code.to_string(),
            message: message.to_string(),
        }),
    )
        .into_response()
}

/// `GET /api/projects/:id/rewrite-advisory`.
pub(crate) async fn get_rewrite_advisory(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Response {
    if state.projects.get(&id).is_none() {
        return refusal(StatusCode::NOT_FOUND, "no_project", "no such project");
    }
    let Some(latest) = state.inspections.latest(&id) else {
        return refusal(
            StatusCode::CONFLICT,
            "no_inspection",
            "This project has no completed inspection yet. Run an audit first.",
        );
    };
    // The corpus gives each rule its enforcement tier and each Fix its authored effort. A missing
    // or unreadable corpus leaves both to the rows, as every export does.
    let corpus_path = camerata_rules::corpus_path();
    let corpus = if corpus_path.exists() {
        let (set, errs) = camerata_rules::load_corpus_lenient(&corpus_path).await;
        crate::log_corpus_load_errors("rewrite advisory", &errs);
        Some(set)
    } else {
        None
    };
    Json(AdvisoryDocument {
        rewrite_advisory: compute(&latest.report, corpus.as_ref()),
    })
    .into_response()
}
