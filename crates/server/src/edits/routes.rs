//! The edit routes (design section 5):
//!
//! - `GET /api/projects/:id/edits`: every edit, and the ones that are stale on the latest run.
//! - `POST /api/projects/:id/edits`: one edit of one field of one site.
//! - `POST /api/projects/:id/edits/split`: move sites to a sub-group of their own.
//! - `POST /api/projects/:id/edits/:edit_id/reapply` and `.../discard`: what a stale edit offers.
//! - `POST /api/projects/:id/added-findings`: a finding the engine missed.
//! - `POST /api/projects/:id/merge`: merge one site into another (a decision with a reason).
//! - `POST /api/projects/:id/decisions/:decision_id/client-ack`: the client accepted an Ignored
//!   finding (design 5.4).
//! - `GET /api/projects/:id/reviewed-report`: the latest inspection with the overlay applied, which
//!   is what the review screen shows.
//!
//! Request and response types are in `camerata_api_types::edits`, shared with the UI. A damaged
//! edit file or decision file answers 503 `store_corrupt` on every route and is never replaced.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use camerata_api_types::decisions::{DecisionState, NewDecisionReq, Scope};
use camerata_api_types::edits::{
    AddFindingReq, ClientAckRecorded, ClientAckReq, DiscardReq, Edit, EditDiscarded, EditError,
    EditField, EditRecorded, EditsResp, FindingAdded, MergeReq, NewEditReq, ReapplyReq,
    SplitRecorded, SplitReq, StaleEditView,
};

use super::{added_finding_from_request, EditCtx, EditDraft, EditStoreError, DEFAULT_EDITOR};
use crate::decisions::{restored_keys_of, DecisionCtx, DecisionStoreError};
use crate::inspections::InspectionEntry;
use crate::onboard::{Finding, SealedReport};
use crate::report_export::overlay::current_value;
use crate::report_export::{apply_overlay, re_mark_line, ReviewedReport};
use crate::AppState;

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn refusal(status: StatusCode, code: &str, message: impl Into<String>) -> Response {
    (
        status,
        Json(EditError {
            ok: false,
            code: code.to_string(),
            message: message.into(),
        }),
    )
        .into_response()
}

fn store_refusal(e: &EditStoreError) -> Response {
    let status = match e {
        EditStoreError::ReasonRequired(_)
        | EditStoreError::KeyRequired
        | EditStoreError::InvalidValue(_)
        | EditStoreError::NoChange
        | EditStoreError::AddedFindingIncomplete(_) => StatusCode::UNPROCESSABLE_ENTITY,
        EditStoreError::NotFound(_) => StatusCode::NOT_FOUND,
        EditStoreError::AlreadyDiscarded(_) => StatusCode::CONFLICT,
        EditStoreError::Persist(_) => StatusCode::INTERNAL_SERVER_ERROR,
        EditStoreError::Corrupt => StatusCode::SERVICE_UNAVAILABLE,
    };
    refusal(status, e.code(), e.to_string())
}

fn decision_refusal(e: &DecisionStoreError) -> Response {
    let status = match e {
        DecisionStoreError::ReasonRequired(_)
        | DecisionStoreError::KeyRequired
        | DecisionStoreError::MergeTargetInvalid
        | DecisionStoreError::NodeMerge
        | DecisionStoreError::AckNeedsWho => StatusCode::UNPROCESSABLE_ENTITY,
        DecisionStoreError::UnknownNode(_) | DecisionStoreError::NotFound(_) => {
            StatusCode::NOT_FOUND
        }
        DecisionStoreError::EmptyNode(_)
        | DecisionStoreError::AlreadyRevoked(_)
        | DecisionStoreError::AckNeedsIgnored(_)
        | DecisionStoreError::AlreadyAcknowledged(_) => StatusCode::CONFLICT,
        DecisionStoreError::Persist(_) => StatusCode::INTERNAL_SERVER_ERROR,
        DecisionStoreError::Corrupt => StatusCode::SERVICE_UNAVAILABLE,
    };
    refusal(status, e.code(), e.to_string())
}

/// The 503 every edit route answers for a project whose edit file (or decision file) was damaged.
pub(crate) fn corrupt_response() -> Response {
    refusal(
        StatusCode::SERVICE_UNAVAILABLE,
        "store_corrupt",
        "The edit file for this project is damaged and was moved aside (it is kept with a \
         .corrupt ending). Edits are not loaded and nothing will be written over it. Restore the \
         file, then restart.",
    )
}

fn no_inspection() -> Response {
    refusal(
        StatusCode::CONFLICT,
        "no_inspection",
        "This project has no completed inspection to edit. Run an audit first.",
    )
}

/// The checks every route starts with: the project exists and neither store is damaged.
#[allow(clippy::result_large_err)]
fn guard(state: &AppState, id: &str) -> Result<(), Response> {
    if state.projects.get(id).is_none() {
        return Err(refusal(
            StatusCode::NOT_FOUND,
            "no_project",
            "no such project",
        ));
    }
    if state.edits.is_corrupt(id) {
        return Err(corrupt_response());
    }
    if state.decisions.is_corrupt(id) {
        return Err(crate::decisions::routes::corrupt_response());
    }
    Ok(())
}

/// The latest inspection sealed with the project's restores, and the overlay over it.
pub(crate) fn review_of(
    state: &AppState,
    id: &str,
    latest: &InspectionEntry,
) -> (SealedReport, ReviewedReport) {
    let decisions = state.decisions.list(id);
    let restored = restored_keys_of(&state.decisions.active(id));
    let sealed = latest.report.clone().seal_with_restored(&restored);
    // The line the project's settings give now, the same one the exports ship.
    let sealed = state
        .projects
        .get(id)
        .map_or_else(|| sealed.clone(), |p| re_mark_line(&sealed, &p.review));
    let reviewed = apply_overlay(
        &sealed,
        &decisions,
        &state.edits.list(id),
        &state.edits.added(id),
    )
    .with_engine_check(
        state
            .engine_check
            .evidence_for(id, &latest.id, state.edits.active(id)),
    );
    (sealed, reviewed)
}

/// The row with `key` in the latest inspection as the engine sealed it (the fingerprint an edit
/// is made against is the sealed row's), and the same row with the overlay applied.
fn rows_of<'a>(
    sealed: &'a SealedReport,
    reviewed: &'a ReviewedReport,
    key: &str,
) -> Option<(&'a Finding, &'a Finding)> {
    let reviewed_row = reviewed
        .report()
        .findings
        .iter()
        .find(|f| f.site_key == key)?;
    // An added finding has no sealed row: it is its own.
    let sealed_row = sealed
        .findings
        .iter()
        .find(|f| f.site_key == key)
        .unwrap_or(reviewed_row);
    Some((sealed_row, reviewed_row))
}

fn draft_for(
    sealed_row: &Finding,
    reviewed_row: &Finding,
    field: EditField,
    new: &str,
    reason: &str,
    by: &str,
    run_id: &str,
) -> EditDraft {
    EditDraft {
        key: sealed_row.site_key.clone(),
        field,
        old: current_value(reviewed_row, field),
        new: new.trim().to_string(),
        reason: reason.to_string(),
        by: by.to_string(),
        run_id: run_id.to_string(),
        fingerprint: camerata_api_types::fingerprint::fingerprint(
            &sealed_row.rule_id,
            &sealed_row.snippet,
        ),
    }
}

/// The key `edit`'s site has in the latest run, when the engine found it moved.
fn moved_key(state: &AppState, id: &str, latest: &InspectionEntry, edit: &Edit) -> String {
    let Some(prev) = state
        .inspections
        .list(id)
        .into_iter()
        .find(|e| e.id == edit.run_id && e.id != latest.id)
    else {
        return String::new();
    };
    crate::review_tree::match_runs(&prev.report.findings, &latest.report.findings)
        .moved
        .into_iter()
        .find(|l| l.old_key == edit.key)
        .map(|l| l.new_key)
        .unwrap_or_default()
}

/// `GET /api/projects/:id/edits`.
pub(crate) async fn get_edits(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    if let Err(r) = guard(&state, &id) {
        return r;
    }
    let edits = state.edits.list(&id);
    let Some(latest) = state.inspections.latest(&id) else {
        return Json(EditsResp {
            edits,
            stale: Vec::new(),
            latest_run_id: String::new(),
        })
        .into_response();
    };
    let (_, reviewed) = review_of(&state, &id, &latest);
    let stale = reviewed
        .stale_edits()
        .iter()
        .map(|s| StaleEditView {
            current_key: moved_key(&state, &id, &latest, &s.edit),
            current_value: s.current_value.clone(),
            edit: s.edit.clone(),
        })
        .collect();
    Json(EditsResp {
        edits,
        stale,
        latest_run_id: latest.id,
    })
    .into_response()
}

/// `POST /api/projects/:id/edits`.
pub(crate) async fn post_edit(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<NewEditReq>,
) -> Response {
    if let Err(r) = guard(&state, &id) {
        return r;
    }
    let Some(latest) = state.inspections.latest(&id) else {
        return no_inspection();
    };
    let (sealed, reviewed) = review_of(&state, &id, &latest);
    let Some((sealed_row, reviewed_row)) = rows_of(&sealed, &reviewed, req.key.trim()) else {
        return refusal(
            StatusCode::NOT_FOUND,
            "unknown_site",
            "This site is not in the latest inspection.",
        );
    };
    let draft = draft_for(
        sealed_row,
        reviewed_row,
        req.field,
        &req.new,
        &req.reason,
        &req.by,
        &req.run_id,
    );
    let when = now();
    match state.edits.record_edit(
        &id,
        draft,
        EditCtx {
            run_id: &latest.id,
            now: &when,
        },
    ) {
        Ok(edit) => (StatusCode::OK, Json(EditRecorded { edit })).into_response(),
        Err(e) => store_refusal(&e),
    }
}

/// `POST /api/projects/:id/edits/split`.
pub(crate) async fn post_split(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<SplitReq>,
) -> Response {
    if let Err(r) = guard(&state, &id) {
        return r;
    }
    let Some(latest) = state.inspections.latest(&id) else {
        return no_inspection();
    };
    if req.keys.is_empty() {
        return refusal(
            StatusCode::UNPROCESSABLE_ENTITY,
            "key_required",
            "A split names the sites to move.",
        );
    }
    let (sealed, reviewed) = review_of(&state, &id, &latest);
    let mut drafts = Vec::with_capacity(req.keys.len());
    for key in &req.keys {
        let Some((sealed_row, reviewed_row)) = rows_of(&sealed, &reviewed, key.trim()) else {
            return refusal(
                StatusCode::NOT_FOUND,
                "unknown_site",
                format!(
                    "The site {} is not in the latest inspection.",
                    key.replace('\u{0}', " ")
                ),
            );
        };
        drafts.push(draft_for(
            sealed_row,
            reviewed_row,
            EditField::Subgroup,
            &req.subgroup,
            &req.reason,
            &req.by,
            "",
        ));
    }
    let when = now();
    match state.edits.record_split(
        &id,
        drafts,
        EditCtx {
            run_id: &latest.id,
            now: &when,
        },
    ) {
        Ok(edits) => (StatusCode::OK, Json(SplitRecorded { edits })).into_response(),
        Err(e) => store_refusal(&e),
    }
}

/// `POST /api/projects/:id/edits/:edit_id/discard`.
pub(crate) async fn discard_edit(
    State(state): State<AppState>,
    Path((id, edit_id)): Path<(String, String)>,
    body: axum::body::Bytes,
) -> Response {
    if let Err(r) = guard(&state, &id) {
        return r;
    }
    if !body.is_empty() {
        if let Err(e) = serde_json::from_slice::<DiscardReq>(&body) {
            return refusal(StatusCode::UNPROCESSABLE_ENTITY, "bad_body", e.to_string());
        }
    }
    match state.edits.discard(&id, &edit_id, &now()) {
        Ok(edit) => Json(EditDiscarded { edit }).into_response(),
        Err(e) => store_refusal(&e),
    }
}

/// `POST /api/projects/:id/edits/:edit_id/reapply`.
pub(crate) async fn reapply_edit(
    State(state): State<AppState>,
    Path((id, edit_id)): Path<(String, String)>,
    body: axum::body::Bytes,
) -> Response {
    if let Err(r) = guard(&state, &id) {
        return r;
    }
    let req: ReapplyReq = if body.is_empty() {
        ReapplyReq::default()
    } else {
        match serde_json::from_slice(&body) {
            Ok(r) => r,
            Err(e) => return refusal(StatusCode::UNPROCESSABLE_ENTITY, "bad_body", e.to_string()),
        }
    };
    let Some(latest) = state.inspections.latest(&id) else {
        return no_inspection();
    };
    let Some(old) = state.edits.list(&id).into_iter().find(|e| e.id == edit_id) else {
        return store_refusal(&EditStoreError::NotFound(edit_id));
    };
    if old.discarded_at.is_some() {
        return store_refusal(&EditStoreError::AlreadyDiscarded(edit_id));
    }
    let (sealed, reviewed) = review_of(&state, &id, &latest);
    if !reviewed.stale_edits().iter().any(|s| s.edit.id == old.id) {
        return refusal(
            StatusCode::CONFLICT,
            "not_stale",
            "This edit is not stale: it applies to its site as it is.",
        );
    }
    let target_key = if req.key.trim().is_empty() {
        moved_key(&state, &id, &latest, &old)
    } else {
        req.key.trim().to_string()
    };
    if target_key.is_empty() {
        return refusal(
            StatusCode::CONFLICT,
            "no_current_site",
            "The engine did not find where this site went. Name the site to reapply it to.",
        );
    }
    let Some((sealed_row, reviewed_row)) = rows_of(&sealed, &reviewed, &target_key) else {
        return refusal(
            StatusCode::NOT_FOUND,
            "unknown_site",
            "That site is not in the latest inspection.",
        );
    };
    let by = if req.by.trim().is_empty() {
        old.by.clone()
    } else {
        req.by.clone()
    };
    let draft = draft_for(
        sealed_row,
        reviewed_row,
        old.field,
        &old.new,
        &old.reason,
        &by,
        "",
    );
    let when = now();
    match state.edits.reapply(
        &id,
        &edit_id,
        draft,
        EditCtx {
            run_id: &latest.id,
            now: &when,
        },
    ) {
        Ok(edit) => (StatusCode::OK, Json(EditRecorded { edit })).into_response(),
        Err(e) => store_refusal(&e),
    }
}

/// The scanned text of `path` in `repo`, redacted the way the model saw it, for the evidence
/// check of a finding the reviewer adds. `None` when the source cannot be read (deleted, never
/// linked, or the file is not there): the check then holds the row.
async fn source_of(
    state: &AppState,
    project: &str,
    repo: &str,
    path: &str,
) -> Vec<(String, String)> {
    use crate::client_source::read::{source_for_read, SourceRead};
    use crate::onboard::source_file::{read_bytes, Origin};
    let Ok(read) = source_for_read(state, Some(project), repo, None).await else {
        return Vec::new();
    };
    let cap = 8 * 1024 * 1024;
    let bytes = match &read {
        SourceRead::Git { dir, sha, .. } => read_bytes(Origin::Git { dir, sha }, path, cap).await,
        SourceRead::WorkingTree { dir } => read_bytes(Origin::WorkingTree(dir), path, cap).await,
        SourceRead::Deleted { .. } | SourceRead::Unresolved { .. } => return Vec::new(),
    };
    let Ok(bytes) = bytes else {
        return Vec::new();
    };
    let Ok(text) = String::from_utf8(bytes) else {
        return Vec::new();
    };
    crate::onboard::redact::files_for_model(&[(path.to_string(), text)])
}

/// `POST /api/projects/:id/added-findings`.
pub(crate) async fn post_added_finding(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<AddFindingReq>,
) -> Response {
    if let Err(r) = guard(&state, &id) {
        return r;
    }
    let Some(latest) = state.inspections.latest(&id) else {
        return no_inspection();
    };
    let files = source_of(&state, &id, req.repo.trim(), req.path.trim()).await;
    let when = now();
    let (_, reviewed) = review_of(&state, &id, &latest);
    let ctx = EditCtx {
        run_id: &latest.id,
        now: &when,
    };
    let (added, evidence) =
        match added_finding_from_request(&req, &reviewed.report().findings, &files, None, ctx) {
            Ok(done) => done,
            Err(e) => return store_refusal(&e),
        };
    match state.edits.add_finding(&id, added.clone()) {
        Ok(_) => (
            StatusCode::OK,
            Json(FindingAdded {
                key: added.finding.site_key,
                evidence_verified: evidence.verified,
                evidence_note: evidence.note,
            }),
        )
            .into_response(),
        Err(e) => store_refusal(&e),
    }
}

/// `POST /api/projects/:id/merge`.
pub(crate) async fn post_merge(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<MergeReq>,
) -> Response {
    if let Err(r) = guard(&state, &id) {
        return r;
    }
    let Some(latest) = state.inspections.latest(&id) else {
        return no_inspection();
    };
    let (sealed, reviewed) = review_of(&state, &id, &latest);
    for (what, key) in [
        ("merged away", req.key.trim()),
        ("merged into", req.target.trim()),
    ] {
        if rows_of(&sealed, &reviewed, key).is_none() {
            return refusal(
                StatusCode::NOT_FOUND,
                "unknown_site",
                format!("The site {what} is not in the latest inspection."),
            );
        }
    }
    let decision = NewDecisionReq {
        scope: Scope::Site,
        key: req.key.clone(),
        state: DecisionState::MergedInto(req.target.trim().to_string()),
        reason: req.reason.clone(),
        decided_by: if req.by.trim().is_empty() {
            DEFAULT_EDITOR.to_string()
        } else {
            req.by.clone()
        },
        needed_me: None,
        sample_verdict: None,
        except: Vec::new(),
        run_id: String::new(),
        decided_at: String::new(),
    };
    let when = now();
    match state.decisions.record_site(
        &id,
        &decision,
        DecisionCtx {
            run_id: &latest.id,
            engine_version: env!("CARGO_PKG_VERSION"),
            now: &when,
        },
    ) {
        Ok(decision) => (StatusCode::OK, Json(decision)).into_response(),
        Err(e) => decision_refusal(&e),
    }
}

/// `POST /api/projects/:id/decisions/:decision_id/client-ack`.
pub(crate) async fn client_ack(
    State(state): State<AppState>,
    Path((id, decision_id)): Path<(String, String)>,
    Json(req): Json<ClientAckReq>,
) -> Response {
    if let Err(r) = guard(&state, &id) {
        return r;
    }
    // After delivery only: until a package has been handed over there is nothing for a client to
    // have accepted.
    if !state
        .inspections
        .list(&id)
        .iter()
        .any(|e| e.package.is_some())
    {
        return refusal(
            StatusCode::CONFLICT,
            "not_delivered",
            "No package has been delivered for this project yet, so there is nothing for the \
             client to acknowledge.",
        );
    }
    let ack = camerata_api_types::decisions::ClientAck {
        by: req.by,
        at: now(),
        note: req.note,
    };
    match state.decisions.ack_client(&id, &decision_id, ack) {
        Ok((decision, also_acknowledged)) => Json(ClientAckRecorded {
            decision,
            also_acknowledged,
        })
        .into_response(),
        Err(e) => decision_refusal(&e),
    }
}

/// What the review screen shows of the latest inspection: the rows and the tree with the overlay
/// applied, and what the overlay did.
#[derive(Debug, serde::Serialize)]
pub struct ReviewedReportResp {
    pub run_id: String,
    pub findings: Vec<Finding>,
    pub tree: Option<crate::review_tree::ReviewTree>,
    /// Rows held for a ruling of their own (the one hold list).
    pub held: Vec<String>,
    pub applied_edit_ids: Vec<String>,
    pub stale: Vec<StaleEditView>,
    /// Findings the reviewer added whose key an engine row has since taken. Kept whole so the
    /// screen can offer to reapply them as edits or discard them.
    pub superseded: Vec<SupersededView>,
    pub merges: Vec<ReviewedMerge>,
}

/// One reviewer-added finding an engine row took the key of.
#[derive(Debug, serde::Serialize)]
pub struct SupersededView {
    pub added: super::AddedFinding,
    pub engine_key: String,
}

/// One merge, as the screen shows it.
#[derive(Debug, serde::Serialize)]
pub struct ReviewedMerge {
    pub key: String,
    pub target: String,
    pub reason: String,
}

/// `GET /api/projects/:id/reviewed-report`: the UI's inspection fetch. Every read of an
/// inspection for review goes through the overlay.
pub(crate) async fn get_reviewed_report(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Response {
    if let Err(r) = guard(&state, &id) {
        return r;
    }
    let Some(latest) = state.inspections.latest(&id) else {
        return no_inspection();
    };
    let (_, reviewed) = review_of(&state, &id, &latest);
    let report = reviewed.report();
    let mut held: Vec<String> = crate::decisions::held_keys(
        &report.findings,
        reviewed.records(),
        reviewed.engine_check(),
    )
    .into_iter()
    .collect();
    held.sort();
    Json(ReviewedReportResp {
        run_id: latest.id.clone(),
        findings: report.findings.clone(),
        tree: report.tree.clone(),
        held,
        applied_edit_ids: reviewed
            .applied_edits()
            .iter()
            .map(|a| a.id.clone())
            .collect(),
        stale: reviewed
            .stale_edits()
            .iter()
            .map(|s| StaleEditView {
                current_key: moved_key(&state, &id, &latest, &s.edit),
                current_value: s.current_value.clone(),
                edit: s.edit.clone(),
            })
            .collect(),
        superseded: reviewed
            .superseded()
            .iter()
            .map(|s| SupersededView {
                added: s.added.clone(),
                engine_key: s.engine_key.clone(),
            })
            .collect(),
        merges: reviewed
            .merges()
            .iter()
            .map(|m| ReviewedMerge {
                key: m.key.clone(),
                target: m.target.clone(),
                reason: m.reason.clone(),
            })
            .collect(),
    })
    .into_response()
}
