//! `POST /api/inspections/:id/diagnostic-bundle` (and the same under `/api/runs/:id/`): the
//! diagnostic bundle of a saved inspection, made from the app, so an operator whose inspection
//! had a failed part can hand a developer what is needed to fix the bug without any client code.
//!
//! The bundle is the one [`camerata diagnose bundle`](crate::diagnostic_bundle) makes: the same
//! build, the same leak check, the same folder and the same refusal words
//! ([`crate::diagnostic_bundle::make`]). The command line reads the integrity ledger and the
//! failure report from the files `camerata inspect` writes beside the repo; an inspection started
//! from the app keeps its ledger inside its saved report instead, so this route takes both from
//! that report (the failure report is rebuilt from it by [`crate::failure_report::build`], the
//! function the command line uses to write the file). Nothing is sent anywhere.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};

use crate::diagnostic_bundle::{self, MakeError};
use crate::AppState;

/// The routes, for the router to merge in.
pub(crate) fn diagnostic_bundle_routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/inspections/:id/diagnostic-bundle",
            post(post_diagnostic_bundle),
        )
        .route(
            "/api/runs/:id/diagnostic-bundle",
            post(post_diagnostic_bundle),
        )
}

fn refusal(status: StatusCode, message: &str) -> Response {
    (status, Json(serde_json::json!({ "error": message }))).into_response()
}

/// The saved report of the inspection `run_id` in `project_id`: the newest saved inspection that
/// is this run, else the project's last scan when it is.
fn report_of_run(
    state: &AppState,
    project_id: &str,
    run_id: &str,
) -> Option<crate::onboard::ScanReport> {
    state
        .inspections
        .list(project_id)
        .into_iter()
        .map(|e| e.report)
        .find(|r| r.run_id.as_deref() == Some(run_id))
        .or_else(|| {
            state
                .get_last_scan(project_id)
                .filter(|r| r.run_id.as_deref() == Some(run_id))
        })
}

/// `POST /api/inspections/:id/diagnostic-bundle`.
pub(crate) async fn post_diagnostic_bundle(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Response {
    if !diagnostic_bundle::valid_run_id(&id) {
        return refusal(StatusCode::BAD_REQUEST, "that is not an inspection id");
    }
    let record = match crate::run_store::read_record(&id) {
        Ok(r) => r,
        Err(e) => return refusal(StatusCode::NOT_FOUND, &e.to_string()),
    };
    // A bundle is made only in the project that started the inspection, as it is resumed.
    if let Some(why) = crate::resume_project_refusal(&state, &record) {
        return refusal(StatusCode::CONFLICT, &why);
    }
    let run_dir = crate::run_store::runs_dir().join(&id);
    let report = record
        .project_id
        .as_deref()
        .and_then(|pid| report_of_run(&state, pid, &id));
    let ledger = report
        .as_ref()
        .and_then(|r| serde_json::to_value(&r.ledger).ok());
    let failures = report
        .as_ref()
        .and_then(|r| serde_json::to_value(crate::failure_report::build(r)).ok());
    let Some(out_dir) = diagnostic_bundle::default_out_dir() else {
        return refusal(
            StatusCode::INTERNAL_SERVER_ERROR,
            "this system has no folder for app data, so the bundle has nowhere to go",
        );
    };
    let known = diagnostic_bundle::shipped_rule_ids().await;
    let made = tokio::task::spawn_blocking(move || {
        diagnostic_bundle::make(
            &run_dir,
            ledger.as_ref(),
            failures.as_ref(),
            &out_dir,
            diagnostic_bundle::MakeOptions {
                named_paths: false,
                shapes: true,
                known_rule_ids: known,
                home_folder: diagnostic_bundle::home_folder_name(),
            },
        )
    })
    .await;
    match made {
        Ok(Ok(made)) => {
            let mut notes = made.written.notes.clone();
            if report.is_none() {
                notes.push(
                    "the saved report of this inspection was not found, so the bundle holds the \
                     run record alone"
                        .to_string(),
                );
            }
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "preview": made.preview,
                    "json": made.written.json,
                    "preview_path": made.written.preview,
                    "lookup": made.written.lookup,
                    "notes": notes,
                })),
            )
                .into_response()
        }
        Ok(Err(e @ MakeError::Refused(_))) => {
            refusal(StatusCode::UNPROCESSABLE_ENTITY, &e.to_string())
        }
        Ok(Err(e)) => refusal(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
        Err(e) => refusal(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("the bundle could not be made: {e}"),
        ),
    }
}
