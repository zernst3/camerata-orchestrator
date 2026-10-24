//! `delete-preview` and `delete`: the operator-facing flow around the guard and the purge.
//!
//! Preview runs the guard (minus the typed confirmation) and builds the plan WITHOUT removing
//! anything; delete runs the whole guard, executes the plan, clears the in-memory copies, writes
//! the deletion record (even when some item could not be removed: the record is the truth) and
//! reports. Calling delete again on a repo that is already deleted is not an error and writes
//! no second record.

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;

use super::guard::{guard, GuardedTarget, Intent};
use super::purge::{execute, plan};
use super::records::{
    now_rfc3339, DeletionRecord, PurgeOutcome, PurgedItem, PurgedKind, RecordNotSaved,
    METHOD_SENTENCE, RESIDUAL_BACKUPS, RESIDUAL_EXCERPTS, RESIDUAL_MODEL_SESSIONS,
    RESIDUAL_UNCOMMITTED,
};
use super::status::{inspected_entry, source_status, RepoSourceStatus};
use super::Refusal;
use crate::AppState;

/// Body of `POST .../source/delete`.
#[derive(Debug, Deserialize)]
pub struct DeleteRequest {
    /// The repository name exactly as the dialog displays it.
    pub confirm: String,
    /// The second, explicit tick: remove uncommitted changes and unpushed commits too.
    #[serde(default)]
    pub include_uncommitted: bool,
}

/// What a delete call ended in.
pub enum DeleteOutcome {
    /// The repo was already deleted; the existing record is returned, nothing was touched.
    AlreadyDeleted(Box<DeletionRecord>),
    /// The purge ran. `partial` when at least one item could not be removed.
    Done {
        record: Box<DeletionRecord>,
        partial: bool,
        /// Set when the record could not be persisted (it is still returned).
        record_not_saved: Option<String>,
    },
}

/// Delete the source of `repo` in `project_id`. See the module docs.
pub async fn delete_source(
    state: &AppState,
    project_id: &str,
    repo_param: &str,
    req: &DeleteRequest,
) -> Result<DeleteOutcome, Refusal> {
    let project = state.projects.get(project_id).ok_or_else(|| {
        Refusal::not_found(
            "unknown_project",
            format!("project not found: {project_id}"),
        )
    })?;
    if let Some(repo) = project
        .repos
        .iter()
        .find(|r| r.eq_ignore_ascii_case(repo_param.trim()))
    {
        if let RepoSourceStatus::Deleted { record_id, .. } =
            source_status(state, &project.id, repo).await
        {
            if let Some(record) = state.source_records.deletion(&project.id, &record_id) {
                return Ok(DeleteOutcome::AlreadyDeleted(Box::new(record)));
            }
        }
    }

    let requested_at = now_rfc3339();
    let target = guard(
        state,
        project_id,
        repo_param,
        Intent::Delete {
            confirm: &req.confirm,
            include_uncommitted: req.include_uncommitted,
        },
    )
    .await?;

    let purge_plan = plan(state, &target).await;
    let had_recorded_sessions = purge_plan
        .steps
        .iter()
        .any(|s| s.item.kind == PurgedKind::ModelSessionFile);
    let items = execute(state, &target, purge_plan).await;

    // The session files are gone (or were never there): forget the ids so the next deletion of
    // another repo of this project does not list them again.
    let cleared_sessions: Vec<String> = state
        .inspections
        .list(project_id)
        .into_iter()
        .flat_map(|e| e.model_sessions)
        .map(|s| s.session_id)
        .collect();
    if !cleared_sessions.is_empty() && items_clear(&items, PurgedKind::ModelSessionFile) {
        if let Err(e) = state
            .inspections
            .forget_model_sessions(project_id, &cleared_sessions)
        {
            tracing::warn!(error = %e, "could not persist the cleared model-session ids");
        }
    }

    let completed_at = now_rfc3339();
    let record = build_record(
        state,
        &target,
        req,
        requested_at,
        completed_at,
        items,
        had_recorded_sessions,
    )
    .await;
    let partial = record.items.iter().any(|i| !i.outcome.is_clear());
    match state.source_records.add_deletion(project_id, record) {
        Ok(record) => Ok(DeleteOutcome::Done {
            record: Box::new(record),
            partial,
            record_not_saved: None,
        }),
        Err(unsaved) => {
            let RecordNotSaved { record, error } = *unsaved;
            tracing::error!(error = %error, "could not persist the deletion record");
            Ok(DeleteOutcome::Done {
                record: Box::new(record),
                partial,
                record_not_saved: Some(error.to_string()),
            })
        }
    }
}

/// Whether every item of `kind` ended up clear (removed or already absent).
fn items_clear(items: &[PurgedItem], kind: PurgedKind) -> bool {
    items
        .iter()
        .filter(|i| i.kind == kind)
        .all(|i| i.outcome.is_clear())
}

async fn build_record(
    state: &AppState,
    target: &GuardedTarget,
    req: &DeleteRequest,
    requested_at: String,
    completed_at: String,
    items: Vec<PurgedItem>,
    had_recorded_sessions: bool,
) -> DeletionRecord {
    let removed: Vec<&PurgedItem> = items
        .iter()
        .filter(|i| i.outcome == PurgeOutcome::Removed)
        .collect();
    let total_files = removed.iter().map(|i| i.files).sum();
    let total_bytes = removed.iter().map(|i| i.bytes).sum();

    let entry = inspected_entry(state, &target.project_id, &target.repo);
    let mut residuals = vec![RESIDUAL_BACKUPS.to_string(), RESIDUAL_EXCERPTS.to_string()];
    if had_recorded_sessions {
        residuals.push(RESIDUAL_MODEL_SESSIONS.to_string());
    }
    if req.include_uncommitted {
        residuals.push(RESIDUAL_UNCOMMITTED.to_string());
    }
    let sanitised: String = target
        .repo
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let compact: String = completed_at.chars().filter(char::is_ascii_digit).collect();
    DeletionRecord {
        id: format!("del-{}-{sanitised}-{compact}", target.project_id),
        seq: 0,
        project_id: target.project_id.clone(),
        repo: target.repo.clone(),
        origin_url: target.origin_url.clone(),
        path: target.path.to_string_lossy().into_owned(),
        inspected_sha: entry
            .as_ref()
            .and_then(|e| e.sha_for(&target.repo))
            .map(str::to_string),
        inspection_id: entry.as_ref().map(|e| e.id.clone()),
        head_sha_at_deletion: target.head_sha.clone(),
        requested_at,
        completed_at,
        confirmed_with: req.confirm.clone(),
        include_uncommitted: req.include_uncommitted,
        items,
        total_files,
        total_bytes,
        method: METHOD_SENTENCE.to_string(),
        residuals,
        camerata_version: env!("CARGO_PKG_VERSION").to_string(),
        machine: machine_name().await,
    }
}

/// This machine's host name, for the client's record.
async fn machine_name() -> String {
    let from_command = tokio::process::Command::new("hostname")
        .kill_on_drop(true)
        .output()
        .await
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty());
    from_command
        .or_else(|| std::env::var("HOSTNAME").ok())
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .unwrap_or_else(|| "unknown".to_string())
}

/// `POST /api/projects/:id/repos/:repo/source/delete-preview` — run the guard and show the plan.
pub async fn delete_preview(
    State(state): State<AppState>,
    Path((id, repo)): Path<(String, String)>,
) -> Response {
    if let Some(project) = state.projects.get(&id) {
        if let Some(r) = project
            .repos
            .iter()
            .find(|r| r.eq_ignore_ascii_case(repo.trim()))
        {
            if source_status(&state, &project.id, r)
                .await
                .deleted()
                .is_some()
            {
                return Refusal::conflict(
                    "already_deleted",
                    format!("The source of {r} was already deleted from this machine."),
                )
                .into_response();
            }
        }
    }
    let target = match guard(&state, &id, &repo, Intent::Preview).await {
        Ok(t) => t,
        Err(refusal) => return refusal.into_response(),
    };
    let purge_plan = plan(&state, &target).await;
    let items = purge_plan.items();
    let total_files: u64 = items.iter().map(|i| i.files).sum();
    let total_bytes: u64 = items.iter().map(|i| i.bytes).sum();

    let mut warnings = Vec::new();
    let uncommitted = target.dirty_files + target.stashes;
    if uncommitted > 0 {
        warnings.push(format!(
            "This checkout has uncommitted changes ({uncommitted} {})",
            if uncommitted == 1 { "item" } else { "items" }
        ));
    }
    for u in &target.unpushed {
        warnings.push(format!(
            "{} has {} commits not on GitHub",
            u.branch, u.ahead
        ));
    }
    Json(serde_json::json!({
        "ok": true,
        "plan": {
            "repo": target.repo,
            "path": target.path.to_string_lossy(),
            "origin_url": target.origin_url,
            "head_sha": target.head_sha,
            "dirty": uncommitted > 0,
            "dirty_files": target.dirty_files,
            "stashes": target.stashes,
            "unpushed": target.unpushed,
            "items": items,
            "total_files": total_files,
            "total_bytes": total_bytes,
            "warnings": warnings,
            "method": METHOD_SENTENCE,
        },
    }))
    .into_response()
}

/// `POST /api/projects/:id/repos/:repo/source/delete` — delete the source. See the module docs.
pub async fn delete(
    State(state): State<AppState>,
    Path((id, repo)): Path<(String, String)>,
    Json(req): Json<DeleteRequest>,
) -> Response {
    match delete_source(&state, &id, &repo, &req).await {
        Err(refusal) => refusal.into_response(),
        Ok(DeleteOutcome::AlreadyDeleted(record)) => Json(serde_json::json!({
            "ok": true,
            "code": "already_deleted",
            "record": record,
        }))
        .into_response(),
        Ok(DeleteOutcome::Done {
            record,
            partial,
            record_not_saved,
        }) => {
            let status = source_status(&state, &id, &record.repo).await;
            let failed = record
                .items
                .iter()
                .filter(|i| !i.outcome.is_clear())
                .count();
            if partial {
                return Json(serde_json::json!({
                    "ok": false,
                    "code": "partial",
                    "record": record,
                    "status": status,
                    "message": format!(
                        "{failed} {} could not be removed; see the record.",
                        if failed == 1 { "location" } else { "locations" }
                    ),
                }))
                .into_response();
            }
            Json(serde_json::json!({
                "ok": true,
                "record": record,
                "status": status,
                "record_not_saved": record_not_saved,
            }))
            .into_response()
        }
    }
}
