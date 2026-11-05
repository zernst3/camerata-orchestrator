//! The decision routes (design 3.5 and 3.6):
//!
//! - `GET /api/projects/:id/decisions`: every record, and what they say about the latest
//!   inspection's sites.
//! - `POST /api/projects/:id/decisions`: one new decision (site or node scope), or the one-time
//!   migration of the UI draft's dispositions (`migrated_from_draft: true`).
//! - `POST /api/projects/:id/decisions/:decision_id/revoke`.
//! - `POST /api/projects/:id/decisions/:decision_id/needed-me`: the "Needed me" flag on an
//!   escalated site's decision (review UI spec section 6).
//!
//! Request and response types are in `camerata_api_types::decisions`, shared with the UI.

use std::collections::BTreeMap;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;

use super::{
    apply_to, disposition_state, translate_draft, Decision, DecisionCtx, DecisionError,
    DecisionRecorded, DecisionState, DecisionStoreError, DecisionsResp, DraftDisposition,
    MigrateDraftReq, MigrationOutcome, NewDecisionReq, PostDecisionsReq, RevokeReq, RevokeResp,
    Scope, MIGRATED_BY,
};
use crate::AppState;

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn refusal(status: StatusCode, code: &str, message: impl Into<String>) -> Response {
    (
        status,
        Json(DecisionError {
            ok: false,
            code: code.to_string(),
            message: message.into(),
        }),
    )
        .into_response()
}

fn store_refusal(e: &DecisionStoreError) -> Response {
    let status = match e {
        DecisionStoreError::ReasonRequired(_)
        | DecisionStoreError::KeyRequired
        | DecisionStoreError::MergeTargetInvalid
        | DecisionStoreError::NodeMerge
        | DecisionStoreError::AckNeedsWho
        | DecisionStoreError::NeededMeNeedsSite(_) => StatusCode::UNPROCESSABLE_ENTITY,
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

fn no_project() -> Response {
    refusal(StatusCode::NOT_FOUND, "no_project", "no such project")
}

/// The 503 every decision route answers for a project whose file was damaged. The UI reads a
/// 5xx as "decisions not loaded" and never draws them as Unresolved.
pub(crate) fn corrupt_response() -> Response {
    refusal(
        StatusCode::SERVICE_UNAVAILABLE,
        "store_corrupt",
        "The decision file for this project is damaged and was moved aside (it is kept with a \
         .corrupt ending). Decisions are not loaded and nothing will be written over it. Restore \
         the file, then restart.",
    )
}

fn no_inspection() -> Response {
    refusal(
        StatusCode::CONFLICT,
        "no_inspection",
        "This project has no completed inspection to decide against. Run an audit first.",
    )
}

/// `GET /api/projects/:id/decisions`.
pub(crate) async fn get_decisions(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Response {
    if state.projects.get(&id).is_none() {
        return no_project();
    }
    if state.decisions.is_corrupt(&id) {
        return corrupt_response();
    }
    let decisions = state.decisions.list(&id);
    let standings = match state.inspections.latest(&id) {
        Some(latest) => match latest.report.tree.as_ref() {
            Some(tree) => {
                let links = moved_links(&state, &id, &latest);
                apply_to(tree, &state.decisions.active(&id), &links).sites
            }
            None => Vec::new(),
        },
        None => Vec::new(),
    };
    Json(DecisionsResp {
        decisions,
        standings,
        migrated_from_draft: state.decisions.migrated_from_draft_at(&id).is_some(),
        latest_run_id: state
            .inspections
            .latest(&id)
            .map(|e| e.id)
            .unwrap_or_default(),
    })
    .into_response()
}

/// The moved sites between each earlier run a live decision was made on and the latest run.
fn moved_links(
    state: &AppState,
    project_id: &str,
    latest: &crate::inspections::InspectionEntry,
) -> Vec<crate::review_tree::KeyLink> {
    let mut runs: Vec<String> = state
        .decisions
        .active(project_id)
        .into_iter()
        .map(|d| d.run_id)
        .filter(|r| !r.is_empty() && *r != latest.id)
        .collect();
    runs.sort();
    runs.dedup();
    if runs.is_empty() {
        return Vec::new();
    }
    let entries = state.inspections.list(project_id);
    let mut links: Vec<crate::review_tree::KeyLink> = Vec::new();
    for run in runs {
        let Some(prev) = entries.iter().find(|e| e.id == run) else {
            continue;
        };
        for link in
            crate::review_tree::match_runs(&prev.report.findings, &latest.report.findings).moved
        {
            if !links.iter().any(|l| l.old_key == link.old_key) {
                links.push(link);
            }
        }
    }
    links
}

/// `POST /api/projects/:id/decisions`.
pub(crate) async fn post_decisions(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<PostDecisionsReq>,
) -> Response {
    if state.projects.get(&id).is_none() {
        return no_project();
    }
    if state.decisions.is_corrupt(&id) {
        return corrupt_response();
    }
    match req {
        PostDecisionsReq::Migrate(m) => migrate(&state, &id, m),
        PostDecisionsReq::Decide(d) => decide(&state, &id, &d),
    }
}

fn decide(state: &AppState, id: &str, req: &NewDecisionReq) -> Response {
    let Some(latest) = state.inspections.latest(id) else {
        return no_inspection();
    };
    // Putting back a finding is only for a finding the engine threw out: there must be a row
    // under that key in the inspection's thrown-out list (A12 item 2).
    if req.state == DecisionState::EngineRestored {
        let thrown_out = matches!(req.scope, Scope::Site)
            && latest
                .report
                .discarded
                .iter()
                .any(|f| f.site_key == req.key.trim());
        if !thrown_out {
            return refusal(
                StatusCode::CONFLICT,
                "not_thrown_out",
                "Only a finding the engine threw out can be put back, and this one is not in the \
                 inspection's thrown-out list.",
            );
        }
    }
    // A row the engine threw out, and the reviewer has not put back, is in no package. Only two
    // decisions settle it: put it back, or call it a false positive (the throw-out was right).
    // Any other ruling would clear the Engine Check's hold on it while the row stayed out, a real
    // problem nobody reported and nothing left to say so. Refused, the hold stands (fail closed).
    if matches!(req.scope, Scope::Site)
        && !matches!(
            req.state,
            DecisionState::EngineRestored | DecisionState::FalsePositive
        )
    {
        let key = req.key.trim();
        let thrown_out = latest.report.discarded.iter().any(|f| f.site_key == key);
        let active = state.decisions.active(id);
        if thrown_out && !camerata_api_types::decisions::restored_keys(&active).contains(key) {
            return refusal(
                StatusCode::CONFLICT,
                "thrown_out_decision",
                "The engine threw this finding out, so it is in no package. Put it back into \
                 review to rule on it, or mark it a false positive to confirm the throw-out was \
                 right. Any other ruling would clear the Engine Check's hold on it and leave a \
                 real problem unreported.",
            );
        }
    }
    let when = now();
    let ctx = DecisionCtx {
        run_id: &latest.id,
        engine_version: env!("CARGO_PKG_VERSION"),
        now: &when,
    };
    let recorded = match &req.scope {
        Scope::Site => state
            .decisions
            .record_site(id, req, ctx)
            .map(|decision| DecisionRecorded {
                decision,
                inherited_count: 0,
            }),
        Scope::Node(node_id) => {
            let Some(tree) = latest.report.tree.as_ref() else {
                return store_refusal(&DecisionStoreError::UnknownNode(node_id.clone()));
            };
            if tree.node(node_id).is_none() {
                return store_refusal(&DecisionStoreError::UnknownNode(node_id.clone()));
            }
            let members = tree.batch_members(node_id);
            // Held rows are never covered by a batch: the gate needs a ruling on them by name,
            // and an inherited record would hide that nobody looked.
            let mut with_held = req.clone();
            let held = super::held_keys(
                &latest.report.findings,
                &state.decisions.active(id),
                &state
                    .engine_check
                    .evidence_for(id, &latest.id, state.edits.active(id)),
            );
            with_held.except.extend(
                members
                    .iter()
                    .filter(|m| held.contains(m.as_str()))
                    .cloned(),
            );
            state
                .decisions
                .record_node(id, node_id, &members, &with_held, ctx)
                .map(|(decision, inherited_count)| DecisionRecorded {
                    decision,
                    inherited_count,
                })
        }
    };
    match recorded {
        Ok(done) => (StatusCode::OK, Json(done)).into_response(),
        Err(e) => store_refusal(&e),
    }
}

fn migrate(state: &AppState, id: &str, req: MigrateDraftReq) -> Response {
    if !req.migrated_from_draft {
        return refusal(
            StatusCode::UNPROCESSABLE_ENTITY,
            "not_a_migration",
            "migrated_from_draft must be true for a migration",
        );
    }
    // Once, ever. A project that was migrated before writes nothing. Its draft is left alone: it
    // now carries the changes the UI has not yet been able to store.
    if state.decisions.migrated_from_draft_at(id).is_some() {
        return Json(MigrationOutcome {
            already_migrated: true,
            migrated: 0,
            unmatched: Vec::new(),
            skipped: 0,
            dispositions: BTreeMap::new(),
        })
        .into_response();
    }
    let mut rows = state
        .get_last_scan(id)
        .map(|r| r.findings)
        .unwrap_or_default();
    // With no rows there is nothing to translate the old keys through: every one would be stored
    // under a key that matches nothing, and the draft would be blanked for nothing. Wait.
    if rows.is_empty() {
        return refusal(
            StatusCode::CONFLICT,
            "no_rows",
            "There are no scan rows yet to translate the saved decisions through. They stay in \
             your saved review and are moved once the project has a scan.",
        );
    }
    if rows.iter().any(|f| f.site_key.is_empty()) {
        crate::inspections::backfill_site_keys(&mut rows);
    }
    let run_id = state
        .inspections
        .latest(id)
        .map(|e| e.id)
        .unwrap_or_default();
    let translation = translate_draft(&rows, &req.dispositions);
    let when = now();
    let record = |key: &str, d: &DraftDisposition| -> Option<Decision> {
        Some(Decision {
            id: String::new(),
            key: key.to_string(),
            state: disposition_state(d)?,
            reason: d.reason.trim().to_string(),
            scope: Scope::Site,
            applies_to: Vec::new(),
            except: Vec::new(),
            inherited_from: None,
            decided_by: MIGRATED_BY.to_string(),
            decided_at: when.clone(),
            run_id: run_id.clone(),
            engine_version: env!("CARGO_PKG_VERSION").to_string(),
            needed_me: None,
            sample_verdict: None,
            confirmed_by_client: None,
            revoked_at: None,
        })
    };
    let records: Vec<Decision> = translation
        .matched
        .iter()
        .chain(translation.unmatched.iter())
        .filter_map(|(key, d)| record(key, d))
        .collect();
    let migrated = records.len();
    match state.decisions.record_migration(id, records, &when) {
        // Saved: only now is it safe to blank the draft.
        Ok(true) => {
            blank_draft_dispositions(state, id);
            Json(MigrationOutcome {
                already_migrated: false,
                migrated,
                unmatched: translation
                    .unmatched
                    .iter()
                    .map(|(k, _)| k.clone())
                    .collect(),
                skipped: translation.skipped,
                dispositions: translation.matched.into_iter().collect(),
            })
            .into_response()
        }
        Ok(false) => Json(MigrationOutcome {
            already_migrated: true,
            migrated: 0,
            unmatched: Vec::new(),
            skipped: 0,
            dispositions: BTreeMap::new(),
        })
        .into_response(),
        Err(e) => store_refusal(&e),
    }
}

/// Blank `dispositions` in the project's saved onboarding draft, leaving the rest of the draft
/// as it is. The draft is the UI's blob; only this one field is touched.
fn blank_draft_dispositions(state: &AppState, project_id: &str) {
    let Some(mut draft) = state.draft.load(project_id) else {
        return;
    };
    let Some(fields) = draft.as_object_mut() else {
        return;
    };
    if fields.contains_key("dispositions") {
        fields.insert("dispositions".to_string(), serde_json::json!({}));
        state.draft.save(project_id, draft);
    }
}

/// `POST /api/projects/:id/decisions/:decision_id/revoke`.
pub(crate) async fn revoke_decision(
    State(state): State<AppState>,
    Path((id, decision_id)): Path<(String, String)>,
    body: axum::body::Bytes,
) -> Response {
    if state.projects.get(&id).is_none() {
        return no_project();
    }
    if state.decisions.is_corrupt(&id) {
        return corrupt_response();
    }
    // The body is optional: `{}` or nothing at all both mean "revoke".
    let _req: RevokeReq = if body.is_empty() {
        RevokeReq::default()
    } else {
        match serde_json::from_slice(&body) {
            Ok(r) => r,
            Err(e) => return refusal(StatusCode::UNPROCESSABLE_ENTITY, "bad_body", e.to_string()),
        }
    };
    let when = now();
    match state.decisions.revoke(&id, &decision_id, &when) {
        Ok(revoked) => Json(RevokeResp {
            revoked,
            revoked_at: when,
        })
        .into_response(),
        Err(e) => store_refusal(&e),
    }
}

/// `POST /api/projects/:id/decisions/:decision_id/needed-me`: set the "Needed me" flag on one
/// site's decision, in place. Answers the updated record.
pub(crate) async fn set_needed_me(
    State(state): State<AppState>,
    Path((id, decision_id)): Path<(String, String)>,
    Json(req): Json<camerata_api_types::decisions::NeededMeReq>,
) -> Response {
    if state.projects.get(&id).is_none() {
        return no_project();
    }
    if state.decisions.is_corrupt(&id) {
        return corrupt_response();
    }
    match state
        .decisions
        .set_needed_me(&id, &decision_id, req.needed_me)
    {
        Ok(decision) => Json(decision).into_response(),
        Err(e) => store_refusal(&e),
    }
}
