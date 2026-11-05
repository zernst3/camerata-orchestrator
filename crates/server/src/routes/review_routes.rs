//! The review read model's routes (review UI spec sections 4 to 10; work order WO9c).
//!
//! - `GET /api/projects/:id/review`: the shell (counts, lanes, the escalation line, the export gate).
//! - `GET /api/projects/:id/review/tree`: nodes, counts and markers; never a site (U1).
//! - `GET /api/projects/:id/review/sites?node=&cursor=&limit=`: one node's sites, 500 at a time.
//! - `GET /api/projects/:id/review/queue?show_below=&below_offset=&below_limit=`: the escalations;
//!   `show_below` is a view flag and writes nothing (U5).
//! - `GET /api/projects/:id/review/engine-check` and `POST .../review/engine-check/:key`.
//! - `GET /api/projects/:id/review/export-gate`: what the release gate refuses.
//! - `GET /api/projects/:id/review/settings`.
//! - `POST /api/projects/:id/escalation-budget/preview`: the line for a proposed rate and floor;
//!   writes nothing. `POST /api/projects/:id/escalation-budget`: the same, and saves the setting.
//! - `POST /api/projects/:id/sample-size`.
//!
//! Request and response types are in `camerata_api_types::review_view`, shared with the UI. Every
//! view is composed from the reviewed report by [`crate::review_view::ReviewComposer`]; a handler
//! only loads the inputs. A `run_id` query that is not the latest inspection answers 409
//! `run_changed`, so a screen never mixes two runs.

use std::collections::HashMap;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use camerata_api_types::project::ReviewSettings;
use camerata_api_types::review::{EscalationBudget, EscalationReason};
use camerata_api_types::review_view::{
    EngineCheckVerdictReq, EscalationBudgetOutcome, EscalationBudgetReq, MovedFromView,
    ReviewViewError, SampleSizeReq, SampleSizeResp, ESCALATION_FLOOR_MAX, ESCALATION_RATE_MAX,
    QUEUE_BELOW_PAGE_SIZE, SAMPLE_SIZE_MAX, SITES_PAGE_SIZE,
};
use serde::Deserialize;

use crate::inspections::InspectionEntry;
use crate::review_tree::KeyLink;
use crate::review_view::{decode_cursor, ReviewComposer, ReviewInputs};
use crate::AppState;

/// The review routes, for the router to merge in.
pub(crate) fn review_routes() -> Router<AppState> {
    Router::new()
        .route("/api/projects/:id/review", get(get_shell))
        .route("/api/projects/:id/review/tree", get(get_tree))
        .route("/api/projects/:id/review/sites", get(get_sites))
        .route("/api/projects/:id/review/queue", get(get_queue))
        .route(
            "/api/projects/:id/review/engine-check",
            get(get_engine_check),
        )
        .route(
            "/api/projects/:id/review/engine-check/:key",
            post(post_engine_check_verdict),
        )
        .route("/api/projects/:id/review/export-gate", get(get_export_gate))
        .route("/api/projects/:id/review/settings", get(get_settings))
        .route(
            "/api/projects/:id/escalation-budget/preview",
            post(preview_escalation_budget),
        )
        .route(
            "/api/projects/:id/escalation-budget",
            post(set_escalation_budget),
        )
        .route("/api/projects/:id/sample-size", post(set_sample_size))
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn refusal(status: StatusCode, code: &str, message: impl Into<String>) -> Response {
    (
        status,
        Json(ReviewViewError {
            ok: false,
            code: code.to_string(),
            message: message.into(),
        }),
    )
        .into_response()
}

fn no_inspection() -> Response {
    refusal(
        StatusCode::CONFLICT,
        "no_inspection",
        "This project has no completed inspection to review. Run an audit first.",
    )
}

/// The project exists and none of its stores is damaged.
#[allow(clippy::result_large_err)]
fn guard(state: &AppState, id: &str) -> Result<crate::project::Project, Response> {
    let Some(project) = state.projects.get(id) else {
        return Err(refusal(
            StatusCode::NOT_FOUND,
            "no_project",
            "no such project",
        ));
    };
    if state.decisions.is_corrupt(id) {
        return Err(crate::decisions::routes::corrupt_response());
    }
    if state.edits.is_corrupt(id) {
        return Err(crate::edits::routes::corrupt_response());
    }
    if state.engine_check.is_corrupt(id) {
        return Err(refusal(
            StatusCode::SERVICE_UNAVAILABLE,
            "store_corrupt",
            crate::engine_check::EngineCheckStoreError::Corrupt.to_string(),
        ));
    }
    Ok(project)
}

/// The sites that moved between each earlier run a decision or an edit was made on and `latest`:
/// the key links the decision standings read, and where each moved site was.
fn moved_sites(
    state: &AppState,
    id: &str,
    latest: &InspectionEntry,
) -> (Vec<KeyLink>, HashMap<String, MovedFromView>) {
    let mut runs: Vec<String> = state
        .decisions
        .active(id)
        .into_iter()
        .map(|d| d.run_id)
        .chain(state.edits.active(id).into_iter().map(|e| e.run_id))
        .filter(|r| !r.is_empty() && *r != latest.id)
        .collect();
    runs.sort();
    runs.dedup();
    let mut links: Vec<KeyLink> = Vec::new();
    let mut moved: HashMap<String, MovedFromView> = HashMap::new();
    if runs.is_empty() {
        return (links, moved);
    }
    let entries = state.inspections.list(id);
    for run in runs {
        let Some(prev) = entries.iter().find(|e| e.id == run) else {
            continue;
        };
        let old_lines: HashMap<&str, usize> = prev
            .report
            .findings
            .iter()
            .map(|f| (f.site_key.as_str(), f.line))
            .collect();
        for link in
            crate::review_tree::match_runs(&prev.report.findings, &latest.report.findings).moved
        {
            if links.iter().any(|l| l.old_key == link.old_key) {
                continue;
            }
            moved
                .entry(link.new_key.clone())
                .or_insert_with(|| MovedFromView {
                    key: link.old_key.clone(),
                    line: old_lines.get(link.old_key.as_str()).copied(),
                });
            links.push(link);
        }
    }
    (links, moved)
}

/// Load what the views are composed from and run `compose` on it. Refuses with the route's
/// answer when the project, a store or the inspection is not there, or `run_id` is not the latest.
fn with_composer(
    state: &AppState,
    id: &str,
    run_id: Option<&str>,
    compose: impl FnOnce(&ReviewComposer<'_>) -> Response,
) -> Response {
    let project = match guard(state, id) {
        Ok(p) => p,
        Err(r) => return r,
    };
    let Some(latest) = state.inspections.latest(id) else {
        return no_inspection();
    };
    if let Some(wanted) = run_id.filter(|r| !r.is_empty()) {
        if wanted != latest.id {
            return refusal(
                StatusCode::CONFLICT,
                "run_changed",
                format!(
                    "This screen is showing inspection {wanted}, and the latest is {}. Reload.",
                    latest.id
                ),
            );
        }
    }
    let (_, reviewed) = crate::edits::routes::review_of(state, id, &latest);
    let edits = state.edits.active(id);
    // The verdicts in force for this run: its own, and an earlier run's No that nothing here has
    // answered (marked `carried_from`), so the lane shows the card as the No it still is.
    let verdicts = state.engine_check.verdicts_in_force(id, &latest.id);
    let (links, moved) = moved_sites(state, id, &latest);
    let inputs = ReviewInputs {
        run_id: &latest.id,
        engine_version: env!("CARGO_PKG_VERSION"),
        reviewed: &reviewed,
        edits: &edits,
        settings: &project.review,
        scan_model: &project.step_models.audit,
        calibration_model: &project.step_models.calibration,
        fix_model: &project.step_models.fix,
        verdicts: &verdicts,
        moved: &moved,
        links: &links,
    };
    compose(&ReviewComposer::new(&inputs))
}

#[derive(Deserialize)]
pub(crate) struct RunQuery {
    #[serde(default)]
    run_id: Option<String>,
}

/// `GET /api/projects/:id/review`.
pub(crate) async fn get_shell(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<RunQuery>,
) -> Response {
    with_composer(&state, &id, q.run_id.as_deref(), |c| {
        Json(c.shell()).into_response()
    })
}

/// `GET /api/projects/:id/review/tree`.
pub(crate) async fn get_tree(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<RunQuery>,
) -> Response {
    with_composer(&state, &id, q.run_id.as_deref(), |c| {
        Json(c.tree()).into_response()
    })
}

#[derive(Deserialize)]
pub(crate) struct SitesQuery {
    #[serde(default)]
    run_id: Option<String>,
    node: String,
    #[serde(default)]
    cursor: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

/// `GET /api/projects/:id/review/sites`.
pub(crate) async fn get_sites(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<SitesQuery>,
) -> Response {
    if q.cursor
        .as_deref()
        .is_some_and(|c| decode_cursor(c).is_none())
    {
        return refusal(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_cursor",
            "The cursor is not one this server returned. Start again from the first page.",
        );
    }
    with_composer(&state, &id, q.run_id.as_deref(), |c| {
        match c.sites_page(
            &q.node,
            q.cursor.as_deref(),
            q.limit.unwrap_or(SITES_PAGE_SIZE),
        ) {
            Some(page) => Json(page).into_response(),
            None => refusal(
                StatusCode::NOT_FOUND,
                "unknown_node",
                format!("The tree has no node {}.", q.node),
            ),
        }
    })
}

#[derive(Deserialize)]
pub(crate) struct QueueQuery {
    #[serde(default)]
    run_id: Option<String>,
    /// A view flag: list the candidates below the line. Writes nothing.
    #[serde(default)]
    show_below: bool,
    #[serde(default)]
    below_offset: usize,
    #[serde(default)]
    below_limit: Option<usize>,
}

/// `GET /api/projects/:id/review/queue`.
pub(crate) async fn get_queue(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<QueueQuery>,
) -> Response {
    with_composer(&state, &id, q.run_id.as_deref(), |c| {
        Json(c.queue(
            q.show_below,
            q.below_offset,
            q.below_limit.unwrap_or(QUEUE_BELOW_PAGE_SIZE),
        ))
        .into_response()
    })
}

/// `GET /api/projects/:id/review/engine-check`.
pub(crate) async fn get_engine_check(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<RunQuery>,
) -> Response {
    with_composer(&state, &id, q.run_id.as_deref(), |c| {
        Json(c.engine_check()).into_response()
    })
}

/// `POST /api/projects/:id/review/engine-check/:key`: one Yes or No on one sampled row.
pub(crate) async fn post_engine_check_verdict(
    State(state): State<AppState>,
    Path((id, key)): Path<(String, String)>,
    Json(req): Json<EngineCheckVerdictReq>,
) -> Response {
    if let Err(r) = guard(&state, &id) {
        return r;
    }
    let Some(latest) = state.inspections.latest(&id) else {
        return no_inspection();
    };
    // Only a row of the sample can be judged.
    let lane_has_it = with_composer(&state, &id, Some(&latest.id), |c| {
        if c.engine_check()
            .cards
            .iter()
            .any(|card| card.site.site_key == key)
        {
            StatusCode::OK.into_response()
        } else {
            refusal(
                StatusCode::NOT_FOUND,
                "not_in_sample",
                "This row is not in the Engine Check sample for this run.",
            )
        }
    });
    if lane_has_it.status() != StatusCode::OK {
        return lane_has_it;
    }
    match state
        .engine_check
        .record(&id, &key, &req, &latest.id, &now())
    {
        Ok(verdict) => Json(verdict).into_response(),
        Err(e) => {
            let status = match e {
                crate::engine_check::EngineCheckStoreError::Corrupt => {
                    StatusCode::SERVICE_UNAVAILABLE
                }
                crate::engine_check::EngineCheckStoreError::Persist(_) => {
                    StatusCode::INTERNAL_SERVER_ERROR
                }
                _ => StatusCode::UNPROCESSABLE_ENTITY,
            };
            refusal(status, e.code(), e.to_string())
        }
    }
}

/// `GET /api/projects/:id/review/export-gate`.
pub(crate) async fn get_export_gate(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<RunQuery>,
) -> Response {
    with_composer(&state, &id, q.run_id.as_deref(), |c| {
        Json(c.export_gate()).into_response()
    })
}

/// `GET /api/projects/:id/review/settings`.
pub(crate) async fn get_settings(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<RunQuery>,
) -> Response {
    with_composer(&state, &id, q.run_id.as_deref(), |c| {
        Json(c.settings()).into_response()
    })
}

fn invalid_setting(message: impl Into<String>) -> Response {
    refusal(StatusCode::UNPROCESSABLE_ENTITY, "invalid_setting", message)
}

/// The refusal for a budget request outside the bounds the settings validate against, or one
/// whose reasons leave out evidence unverified (design D5: it is always escalated).
#[allow(clippy::result_large_err)]
fn check_budget_req(req: &EscalationBudgetReq) -> Result<(), Response> {
    if !req.rate_per_kloc.is_finite()
        || req.rate_per_kloc < 0.0
        || req.rate_per_kloc > ESCALATION_RATE_MAX
    {
        return Err(invalid_setting(format!(
            "The rate must be between 0 and {ESCALATION_RATE_MAX} escalations per 1,000 lines."
        )));
    }
    if req.floor > ESCALATION_FLOOR_MAX {
        return Err(invalid_setting(format!(
            "The floor must be between 0 and {ESCALATION_FLOOR_MAX}."
        )));
    }
    // Design D5: evidence unverified is always escalated. The release gate holds those rows
    // anyway, so a set that drops it only hides them from the architect's queue.
    if !req
        .over_budget_reasons
        .contains(&EscalationReason::EvidenceUnverified)
    {
        return Err(invalid_setting(
            "The over-budget reasons must include evidence unverified: it is always escalated.",
        ));
    }
    Ok(())
}

/// The line before and after `req`, from the latest inspection when there is one, and from the
/// settings alone (no candidates) when there is not.
fn budget_outcome_for(
    state: &AppState,
    id: &str,
    project: &crate::project::Project,
    req: &EscalationBudgetReq,
    applied: bool,
) -> Response {
    if state.inspections.latest(id).is_some() {
        return with_composer(state, id, None, |c| {
            Json(c.budget_outcome(req, applied)).into_response()
        });
    }
    let before = EscalationBudget::compute(
        project.review.escalation_rate_per_kloc,
        project.review.escalation_floor,
        0,
    );
    let after = EscalationBudget::compute(req.rate_per_kloc, req.floor, 0);
    Json(EscalationBudgetOutcome {
        applied,
        before,
        after,
        moved_above: 0,
        moved_below: 0,
    })
    .into_response()
}

/// `POST /api/projects/:id/escalation-budget/preview`: the line for a proposed rate and floor.
/// Writes nothing and calls no model.
pub(crate) async fn preview_escalation_budget(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<EscalationBudgetReq>,
) -> Response {
    let project = match guard(&state, &id) {
        Ok(p) => p,
        Err(r) => return r,
    };
    if let Err(r) = check_budget_req(&req) {
        return r;
    }
    budget_outcome_for(&state, &id, &project, &req, false)
}

/// `POST /api/projects/:id/escalation-budget`: save the rate, floor and over-budget reasons. The
/// next read marks the line again from the stored candidates (design A4); no model is called.
pub(crate) async fn set_escalation_budget(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<EscalationBudgetReq>,
) -> Response {
    let project = match guard(&state, &id) {
        Ok(p) => p,
        Err(r) => return r,
    };
    if let Err(r) = check_budget_req(&req) {
        return r;
    }
    // The outcome is worked out against the settings as they stand BEFORE the write.
    let outcome = budget_outcome_for(&state, &id, &project, &req, true);
    let settings = ReviewSettings {
        escalation_rate_per_kloc: req.rate_per_kloc,
        escalation_floor: req.floor,
        over_budget_reasons: req.over_budget_reasons.clone(),
        ..project.review
    };
    match state.projects.set_review_settings(&id, settings) {
        Some(Ok(_)) => outcome,
        Some(Err(message)) => invalid_setting(message),
        None => refusal(StatusCode::NOT_FOUND, "no_project", "no such project"),
    }
}

/// `POST /api/projects/:id/sample-size`.
pub(crate) async fn set_sample_size(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<SampleSizeReq>,
) -> Response {
    let project = match guard(&state, &id) {
        Ok(p) => p,
        Err(r) => return r,
    };
    if req.sample_size_per_run > SAMPLE_SIZE_MAX {
        return invalid_setting(format!(
            "The sample size must be between 0 and {SAMPLE_SIZE_MAX}."
        ));
    }
    let settings = ReviewSettings {
        sample_size_per_run: req.sample_size_per_run,
        ..project.review
    };
    match state.projects.set_review_settings(&id, settings) {
        Some(Ok(_)) => Json(SampleSizeResp {
            sample_size_per_run: req.sample_size_per_run,
            lane_off: req.sample_size_per_run == 0,
        })
        .into_response(),
        Some(Err(message)) => invalid_setting(message),
        None => refusal(StatusCode::NOT_FOUND, "no_project", "no such project"),
    }
}
