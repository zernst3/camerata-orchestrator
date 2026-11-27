//! The app's pre-flight and the spend checks of its inspection start.
//!
//! `POST /api/onboard/plan` shows, before Start, what `camerata inspect --plan-only` prints for the
//! same inspection: the lines, the files admitted and left out by reason, the projected cost, the
//! suggested spend limit and every warning. It is [`crate::run_plan::plan`], the computation the
//! command line calls, fed from the active project instead of from flags. It makes no model call
//! and reads no credential (the backend is resolved as if a key were saved, as plan-only does).
//!
//! The start of a fresh inspection ([`crate::onboard_audit_start`] and the synchronous
//! `onboard_audit`) runs [`start_spend_refusal`] before anything is spawned: a client inspection
//! with a blank or zero spend limit does not start, and a limit under the projection refuses for
//! the reason a real run gives. The server is the authority; the scan screen only shows it first.

use axum::extract::State;
use axum::Json;

use crate::*;

/// The refusal of a client inspection that has no usable spend limit: none was given, or the one
/// given is not a positive number of US dollars. `None` when the run may go on to its other
/// checks. A run that is not a client engagement may start without a limit.
pub fn client_limit_refusal(client_engagement: bool, max_cost_usd: Option<f64>) -> Option<String> {
    if !client_engagement {
        return None;
    }
    let usable = max_cost_usd.is_some_and(|cap| cap.is_finite() && cap > 0.0);
    if usable {
        return None;
    }
    Some(
        "This client inspection did not start: it needs a spend limit. Enter a spend limit \
         above zero in US dollars (the pre-flight suggests one), then start again. Nothing was \
         read and no model was called."
            .to_string(),
    )
}

/// The plan's inputs for `req` on `project`, resolved the way the app's inspection job resolves
/// them: the project's models, finder, exclusion list and baseline, the request's rules and
/// limit. `Err` names why there is nothing to plan (no repo folder, an exclusion list that
/// cannot be used, an unknown mode).
pub(crate) async fn app_plan_inputs(
    state: &AppState,
    project: Option<&crate::project::Project>,
    req: &AuditReq,
) -> Result<crate::run_plan::PlanInputs, String> {
    let repos: Vec<String> = req
        .repos
        .iter()
        .filter(|r| !r.trim().is_empty())
        .cloned()
        .collect();
    if repos.is_empty() {
        return Err("there is no repository to inspect".to_string());
    }
    let requested_mode = crate::ai_audit::ScanMode::parse(req.mode.as_deref())?;
    let controls = req.run_controls_for_project(project);
    let LocalSources {
        sources,
        unresolved,
        ..
    } = resolve_local_sources(state, &repos);
    if let Some(why) = partial_run_refusal(&unresolved, repos.len(), req.allow_partial) {
        return Err(why);
    }
    let (_, run_deterministic, _) = effective_scan_modes(req.run_ai_review, req.run_deterministic);
    let (refusal, start_warnings) =
        crate::onboard::scope::start_check(project, &sources, run_deterministic).await;
    if let Some(why) = refusal {
        return Err(why);
    }
    let selected: Vec<crate::onboard::SelectedRule> = req
        .rules
        .iter()
        .filter(|r| !r.id.trim().is_empty())
        .map(|r| crate::onboard::SelectedRule {
            id: r.id.clone(),
            directive: r.directive.clone(),
            repos: r.repos.clone(),
        })
        .collect();
    let (_, _, _, corpus, corpus_load_failures) = split_scannable_rules(selected.clone()).await;
    let model = project_step_model_or(
        project,
        crate::project::StepKind::Audit,
        req.model.as_deref(),
    );
    let calibration_model = project_step_model_or(
        project,
        crate::project::StepKind::Calibration,
        req.calibration_model.as_deref(),
    );
    let backend = project.map(|p| p.backend).unwrap_or_default();
    let incremental = uses_prior_manifest(req.incremental, controls.review.finder)
        && project.is_some_and(|p| state.scan_cache.get(&p.id).is_some());
    Ok(crate::run_plan::PlanInputs {
        sources,
        selected,
        corpus,
        corpus_load_failures,
        controls,
        backend,
        // No credential is read: the plan is the run's with a key saved, as plan-only's is.
        backend_resolution: crate::llm::resolve_backend(backend, true),
        requested_mode,
        model: Some(model),
        calibration_model: Some(calibration_model),
        incremental,
        // The routes treat no project as a client engagement.
        client_engagement: project.is_none_or(|p| p.client_engagement),
        start_warnings,
    })
}

/// The plan of `req` on `project`.
pub(crate) async fn app_plan(
    state: &AppState,
    project: Option<&crate::project::Project>,
    req: &AuditReq,
) -> Result<crate::run_plan::RunPlan, String> {
    crate::run_plan::plan(app_plan_inputs(state, project, req).await?).await
}

/// What Start says for the spend of `req` once its plan is known (`plan` is `None` when it could
/// not be built): a client inspection with no usable limit, or a limit a real run would refuse
/// under its projection. `None` lets the start go on to its other checks. A plan that cannot be
/// built is not a refusal here: the start makes those checks itself, in its own words. A run
/// with no model call (the AI review off) has nothing to project.
pub(crate) fn spend_refusal_given_plan(
    client_engagement: bool,
    req: &AuditReq,
    plan: Option<&crate::run_plan::RunPlan>,
) -> Option<String> {
    if let Some(why) = client_limit_refusal(client_engagement, req.max_cost_usd) {
        return Some(why);
    }
    req.max_cost_usd?;
    let (run_ai_review, _, _) = effective_scan_modes(req.run_ai_review, req.run_deterministic);
    if !run_ai_review {
        return None;
    }
    plan?.refusal.as_ref().map(|why| {
        format!(
            "This inspection did not start. The pre-flight says {why} Raise the spend limit, \
             then start again. Nothing was sent to a model."
        )
    })
}

/// Why a fresh inspection of `req` on `project` must not start for its spend (see
/// [`spend_refusal_given_plan`]). The missing limit is refused before any file is read.
pub(crate) async fn start_spend_refusal(
    state: &AppState,
    project: Option<&crate::project::Project>,
    req: &AuditReq,
) -> Option<String> {
    let client_engagement = project.is_none_or(|p| p.client_engagement);
    if let Some(why) = client_limit_refusal(client_engagement, req.max_cost_usd) {
        return Some(why);
    }
    req.max_cost_usd?;
    let plan = app_plan(state, project, req).await.ok();
    spend_refusal_given_plan(client_engagement, req, plan.as_ref())
}

/// `POST /api/onboard/plan`: the pre-flight of the inspection `req` describes, on the active
/// project. `{ ok: true, plan, start_refusal }`, where `start_refusal` is what Start would say
/// for the spend (`null` when the spend checks pass), or `{ ok: false, error }` when there is
/// nothing to plan.
pub(crate) async fn onboard_plan(
    State(state): State<AppState>,
    Json(req): Json<AuditReq>,
) -> Json<serde_json::Value> {
    let project = state.projects.active();
    let client_engagement = project.as_ref().is_none_or(|p| p.client_engagement);
    match app_plan(&state, project.as_ref(), &req).await {
        Ok(plan) => {
            let start_refusal = spend_refusal_given_plan(client_engagement, &req, Some(&plan));
            Json(serde_json::json!({
                "ok": true,
                "client_engagement": client_engagement,
                "plan": plan,
                "start_refusal": start_refusal,
            }))
        }
        Err(error) => Json(serde_json::json!({
            "ok": false,
            "client_engagement": client_engagement,
            "error": error,
        })),
    }
}

#[cfg(test)]
#[path = "scan_plan_tests.rs"]
mod tests;
