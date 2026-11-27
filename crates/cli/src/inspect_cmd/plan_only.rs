//! `camerata inspect --plan-only`: what a real run would take on, printed as figures, with no
//! model call, no network, no credential read and nothing saved.
//!
//! The command line resolves its flags into the plan's inputs (the checks, the rule selection,
//! the backend and the baseline) and hands them to [`camerata_server::run_plan::plan`], the one
//! computation the app's pre-flight route calls too, so the two never diverge. It never opens the
//! state folder for writing: no run, no ledger, no rooms on disk, no copy of client code. What it
//! prints is counts, sizes and paths relative to the repository, never file text.

use std::fmt::Write as _;

use camerata_server::ai_audit::ScanMode;
use camerata_server::llm::resolve_backend;
use camerata_server::run_plan::{PlanInputs, BASIS};

use super::{curated_rule_selection, validate_args, InspectArgs, InspectError};

/// The figures, then `Ok` when a real run would start and an error naming why it would not.
/// The text goes to `sink` in one piece, before the result is returned, so a refusal still prints
/// what it was measured on. No model seam is taken: nothing here has a model to call.
pub async fn run_plan_only_with(
    args: InspectArgs,
    has_api_key: bool,
    sink: &(dyn Fn(&str) + Sync),
) -> Result<(), InspectError> {
    let (text, verdict) = plan(args, has_api_key).await?;
    sink(&text);
    verdict
}

/// Production entry point. The key is not looked up: `--plan-only` never reads the credential
/// store, so a missing key is not reported here (a real run refuses it).
pub async fn run_plan_only(args: InspectArgs) -> Result<(), InspectError> {
    super::refuse_unfit_build_for_client(
        args.client_engagement,
        &camerata_server::run_store::engine_pin::BuildId::current(),
    )?;
    run_plan_only_with(args, true, &|text| print!("{text}")).await
}

async fn plan(
    args: InspectArgs,
    has_api_key: bool,
) -> Result<(String, Result<(), InspectError>), InspectError> {
    validate_args(&args, has_api_key)?;
    let mut controls = args.run_controls()?;
    let sources = args.resolve_sources()?;
    let names: Vec<String> = sources.iter().map(|(n, _)| n.clone()).collect();
    controls.scope = controls
        .scope
        .clone()
        .check_names(&names)
        .map_err(|why| InspectError::Failed(format!("--exclude: {why}")))?;
    let (refusal, warnings) =
        camerata_server::onboard::scope::start_checks(&sources, &controls.scope).await;
    if let Some(why) = refusal {
        return Err(InspectError::Failed(why));
    }

    let scan = camerata_server::onboard::scan_repos(&sources, Vec::new(), &controls.scope).await;
    let selected = curated_rule_selection(&scan.proposed_rules);
    let (_ai, _mechanical, _preview, corpus, corpus_load_failures) =
        camerata_server::split_scannable_rules(selected.clone()).await;

    let requested = if args.batch {
        Some(ScanMode::Batch)
    } else if args.realtime {
        Some(ScanMode::Parallel)
    } else {
        None
    };
    // A baseline is looked for read only.
    let (manifest_path, _) = args.state_paths();
    let incremental =
        !args.full && camerata_server::run_plan::prior_baseline_exists(&manifest_path);
    let plan = camerata_server::run_plan::plan(PlanInputs {
        sources,
        selected,
        corpus,
        corpus_load_failures,
        controls,
        backend: args.backend,
        backend_resolution: resolve_backend(args.backend, has_api_key),
        requested_mode: requested,
        model: args.model.clone(),
        calibration_model: args.calibration_model.clone(),
        incremental,
        client_engagement: args.client_engagement,
        start_warnings: warnings,
    })
    .await
    .map_err(InspectError::Failed)?;

    let mut out = String::new();
    writeln!(
        out,
        "camerata inspect --plan-only: figures only. No model call, no network, no credential \
         read, nothing saved."
    )
    .ok();
    writeln!(
        out,
        "cost basis: {BASIS}. API key: not checked (a real run refuses without one)."
    )
    .ok();
    out.push_str(&plan.text);
    let verdict = match plan.refusal {
        Some(why) => Err(InspectError::Failed(why)),
        None => Ok(()),
    };
    Ok((out, verdict))
}
