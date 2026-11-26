//! The rules an export or a review screen reads: the ones the inspection ran with (#560).
//!
//! Only the inspection is pinned to an engine build; triage and export can run on any newer
//! build. The rule text, fix text, citations, severities and sources a client package carries
//! must be exactly the inspection's, so every export route and review screen takes its rules
//! from here, and nothing here reads the live library (`camerata_rules::corpus_path()`; the
//! guard test `export_paths_never_read_the_live_rule_library` holds every such route to it).
//!
//! The rules come from the inspection's kept copy of its run's snapshot
//! ([`crate::inspections::InspectionEntry::rule_digest`]), or, for an inspection recorded before
//! the copy was kept (or a run backfilled since), from the run's own folder, which is then copied
//! beside the inspection. Either way they are verified against the run's pinned rule digest on
//! every read ([`crate::run_store::rule_snapshot::decode`]).
//!
//! An export refuses (fail closed, [`refusal_response`]) when the rules are not there or do not
//! verify. A review screen shows no rule data instead ([`rules_for_review`]): it never shows the
//! live library's, and the export it leads to refuses with the reason.

use axum::response::{IntoResponse, Response};
use axum::Json;

use crate::run_store::rule_snapshot::{self, RefusalKind, SnapshotRefusal};

/// The rules the inspection behind `report` (the project's current report) ran with, verified.
pub(crate) fn rules_for_export(
    state: &crate::AppState,
    project_id: &str,
    report: &crate::onboard::ScanReport,
) -> Result<camerata_rules::RuleSet, SnapshotRefusal> {
    rules_for_run(state, project_id, report.run_id.as_deref())
}

/// [`rules_for_export`] for the inspection made from the run `run_id`.
pub(crate) fn rules_for_run(
    state: &crate::AppState,
    project_id: &str,
    run_id: Option<&str>,
) -> Result<camerata_rules::RuleSet, SnapshotRefusal> {
    let refuse = |kind| SnapshotRefusal::new(run_id, kind);
    if let Some(digest) = state.inspections.rule_digest_for_run(project_id, run_id) {
        match state.inspections.rule_snapshot_bytes(&digest) {
            Ok(Some(bytes)) => return rule_snapshot::decode(&bytes, &digest).map_err(refuse),
            // The kept copy is gone: the run's own folder may still hold it.
            Ok(None) => {}
            Err(e) => return Err(refuse(RefusalKind::Unreadable(e.to_string()))),
        }
    }
    let Some(run_id) = run_id else {
        return Err(refuse(RefusalKind::NoRun));
    };
    let snapshot = rule_snapshot::read_run(run_id)?;
    // Keep a copy beside the inspection, so it outlives the run's folder from now on. A failure
    // to keep it does not stop this export, which has verified rules in hand.
    match state
        .inspections
        .keep_rule_snapshot(&snapshot.digest, &snapshot.bytes)
    {
        Ok(()) => {
            if let Err(e) =
                state
                    .inspections
                    .set_rule_digest_for_run(project_id, run_id, &snapshot.digest)
            {
                tracing::warn!(project = project_id, run = run_id, error = %e, "could not record the kept rules on the inspection");
            }
        }
        Err(e) => {
            tracing::warn!(project = project_id, run = run_id, error = %e, "could not keep the rules beside the inspection");
        }
    }
    Ok(snapshot.rules)
}

/// The rules a review screen shows for the project's latest inspection: its snapshot, or `None`
/// (logged) when it has none that verifies. Never the live library.
pub(crate) fn rules_for_review(
    state: &crate::AppState,
    project_id: &str,
    run_id: Option<&str>,
) -> Option<camerata_rules::RuleSet> {
    match rules_for_run(state, project_id, run_id) {
        Ok(rules) => Some(rules),
        Err(refusal) => {
            tracing::warn!(project = project_id, code = refusal.code(), "{refusal}");
            None
        }
    }
}

/// The 422 every export route returns when the inspection's rules cannot be used:
/// `{ ok: false, gate: "rule_snapshot", reason, message }`, the message naming what to do.
pub(crate) fn refusal_response(refusal: &SnapshotRefusal) -> Response {
    (
        axum::http::StatusCode::UNPROCESSABLE_ENTITY,
        Json(serde_json::json!({
            "ok": false,
            "gate": "rule_snapshot",
            "reason": refusal.code(),
            "message": refusal.to_string(),
        })),
    )
        .into_response()
}

#[cfg(test)]
#[path = "export_rules_test_support.rs"]
pub(crate) mod test_support;
