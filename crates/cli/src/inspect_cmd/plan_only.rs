//! `camerata inspect --plan-only`: what a real run would take on, printed as figures, with no
//! model call, no network, no credential read and nothing saved.
//!
//! It runs the front half of a real run (the checks, the admission walk, the rule selection and
//! the pre-flight) and the room building of the agent finder, then stops before the first model
//! call and before the run is saved. It never opens the state folder for writing: no run, no
//! ledger, no rooms on disk, no copy of client code. What it prints is counts, sizes and paths
//! relative to the repository, never file text.
//!
//! The spend-limit check is the run's own: the checkpoint plan the pre-flight fills is handed to
//! a meter under the same controls, and the meter's `projection_stops_piece` decides, exactly as
//! it does before a real run's first piece.

use std::fmt::Write as _;
use std::sync::{Arc, Mutex};

use camerata_server::ai_audit::agent_finder::run::{piece_figures, PieceFigures};
use camerata_server::ai_audit::agent_finder::FinderChoice;
use camerata_server::ai_audit::{ScanMode, UsageMeter};
use camerata_server::llm::{resolve_backend, BackendResolution};
use camerata_server::onboard::count::RepoCount;

use super::{
    cli_version_refusal, curated_rule_selection, preflight, validate_args, write_start_warnings,
    InspectArgs, InspectError,
};

/// The basis every cost figure here is printed under.
const BASIS: &str = "projected, API prices";

/// `n` with thousands separators.
fn n(v: usize) -> String {
    let digits = v.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// The files a repo's walk left out, by the reason Coverage gives and in its order, counts only.
/// The groups are `camerata count`'s, from the same walk, so a reason Coverage names (a type
/// opened for declared versions only, a name that is not UTF-8 text) is never folded into
/// another or dropped here.
fn left_out(count: &RepoCount) -> Vec<(String, usize)> {
    count
        .not_read
        .iter()
        .filter(|g| !g.files.is_empty())
        .map(|g| (g.reason.clone(), g.files.len()))
        .collect()
}

/// Whether an incremental-scan baseline exists for this run: a read only look, never the removal
/// of an outdated one that a real run's loader does.
fn prior_baseline_exists(path: &std::path::Path) -> bool {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| {
            serde_json::from_slice::<camerata_server::scan_cache::ScanManifest>(&bytes).ok()
        })
        .is_some_and(|m| m.is_current())
}

/// One piece's line.
fn piece_line(i: usize, count: usize, p: &PieceFigures) -> String {
    format!(
        "  piece {} of {} ({}): {} files, {} lines, {} bytes; context {} files, {} bytes; \
         checklist {} bytes; first request about {} tokens in (plus {} output reserve); at least {} tokens \
         once every file is read, output reserve included (lower bound), against a guard of {}; \
         turn limit {}",
        i + 1,
        count,
        p.unit,
        n(p.files),
        n(p.lines),
        n(p.bytes),
        n(p.context_files),
        n(p.context_bytes),
        n(p.checklist_bytes),
        n(p.first_request_tokens),
        n(p.output_reserve_tokens),
        n(p.peak_tokens),
        n(p.guard_tokens),
        p.turn_limit
    )
}

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

#[allow(clippy::too_many_lines)]
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
    // What a real run prints to stderr here is part of the plan's own warnings.
    let mut start_warnings: Vec<u8> = Vec::new();
    write_start_warnings(&mut start_warnings, &warnings);
    let mut flags: Vec<String> = String::from_utf8_lossy(&start_warnings)
        .lines()
        .map(str::to_string)
        .collect();

    let scan = camerata_server::onboard::scan_repos(&sources, Vec::new(), &controls.scope).await;
    let selected = curated_rule_selection(&scan.proposed_rules);
    let (_ai, _mechanical, _preview, corpus, corpus_load_failures) =
        camerata_server::split_scannable_rules(selected.clone()).await;

    let backend_resolution = resolve_backend(args.backend, has_api_key);
    let requested = if args.batch {
        Some(ScanMode::Batch)
    } else if args.realtime {
        Some(ScanMode::Parallel)
    } else {
        None
    };
    let mode_choice = camerata_server::ai_audit::resolve_scan_mode(
        requested,
        &backend_resolution,
        controls.efficiency,
    );
    if let Some(notice) = &mode_choice.notice {
        flags.push(format!("note: {notice}"));
    }
    let batch = mode_choice.mode == ScanMode::Batch;
    let model = args.model.as_deref().filter(|m| !m.trim().is_empty());
    let calibration_model = args
        .calibration_model
        .as_deref()
        .filter(|m| !m.trim().is_empty());

    // The refusals a real run makes before its estimate, in the same order, from the same
    // functions. A baseline is looked for read only.
    let (manifest_path, _) = args.state_paths();
    let incremental = !args.full && prior_baseline_exists(&manifest_path);
    camerata_server::ai_audit::agent_finder::run::refuse_unsupported_finder_settings(
        &controls,
        incremental,
        !args.client_engagement,
    )
    .map_err(InspectError::Failed)?;
    camerata_server::ai_audit::agent_finder::run::refuse_unloadable_rules(
        &controls,
        !matches!(backend_resolution, BackendResolution::Blocked { .. }),
        &corpus_load_failures,
    )
    .map_err(InspectError::Failed)?;
    if matches!(backend_resolution, BackendResolution::Blocked { .. }) {
        return Err(InspectError::Failed(
            "no model can be called on this backend, so a real run would not start".to_string(),
        ));
    }
    let ids: Vec<&str> = selected.iter().map(|r| r.id.as_str()).collect();
    let models = camerata_server::cost_cap::models_the_run_will_use(
        model,
        calibration_model,
        false,
        &ids,
        &controls,
    );
    if let Some(why) = controls.refuse_if_cap_unenforceable(&models) {
        return Err(InspectError::Failed(why));
    }
    if let Some(why) = cli_version_refusal(
        matches!(backend_resolution, BackendResolution::Cli),
        false,
        &models,
        camerata_server::claude_version::check_run_models,
    ) {
        return Err(InspectError::Failed(why));
    }

    // The facts the pre-flight prices, and the coverage reasons for what was left out.
    let facts = camerata_server::preflight::gather_scan_facts(
        &sources,
        &selected,
        corpus.as_ref(),
        &controls.scope,
    )
    .await;
    if let Some(bad) = facts.iter().find(|f| f.read_error.is_some()) {
        return Err(InspectError::Failed(format!(
            "could not read {}: {}",
            bad.repo,
            bad.read_error.as_deref().unwrap_or("unknown error")
        )));
    }
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
    writeln!(out, "\n== ADMISSION (what a real run would read) ==").ok();
    let (mut t_files, mut t_lines, mut t_bytes) = (0usize, 0usize, 0usize);
    for ((spec, dir), fact) in sources.iter().zip(&facts) {
        let repo_scope = controls.scope.for_repo(spec);
        let dir = dir.clone();
        let walk = tokio::task::spawn_blocking(move || {
            camerata_server::onboard::count::count_repo(&dir, &repo_scope)
        })
        .await
        .map_err(|e| InspectError::Failed(format!("could not read {spec}: {e}")))?
        .map_err(|e| InspectError::Failed(format!("could not read {spec}: {e}")))?;
        let lines: usize = fact.files.iter().map(|(_, t)| t.lines().count()).sum();
        writeln!(
            out,
            "  {}: {} files admitted, {} lines, {} bytes",
            fact.repo,
            n(fact.files.len()),
            n(lines),
            n(fact.code_chars)
        )
        .ok();
        let rows = left_out(&walk);
        if rows.is_empty() {
            writeln!(out, "    left out: none").ok();
        } else {
            for (reason, count) in rows {
                writeln!(out, "    left out: {} {reason}", n(count)).ok();
            }
        }
        t_files += fact.files.len();
        t_lines += lines;
        t_bytes += fact.code_chars;
    }
    writeln!(
        out,
        "  total: {} files admitted, {} lines, {} bytes",
        n(t_files),
        n(t_lines),
        n(t_bytes)
    )
    .ok();

    // The pre-flight, built from the same inputs a real run gives it, with no exact count (that
    // is a network call).
    let audit_model = camerata_server::ai_audit::resolve_audit_model(model);
    let judge_selected = models
        .iter()
        .any(|(roles, _)| roles.contains("the clone judge"));
    let inputs = preflight::PreflightInputs {
        backend: args.backend,
        batch,
        fix_model: Some(camerata_server::ai_audit::resolve_audit_model(
            controls.fix_model_for(model, calibration_model),
        )),
        clone_judge_model: judge_selected.then(|| controls.clone_judge_model_for()),
        calibration_model: camerata_server::ai_audit::resolve_calibration_model(
            calibration_model,
            &audit_model,
        ),
        group_review_model: controls.group_review_model_for(),
        model: audit_model,
        repos: &facts,
        incremental,
        cap_usd: controls.max_cost_usd,
        exact: None,
        terse_output: controls.efficiency.terse_output,
        consensus: controls.consensus,
    };
    let agent = controls.finder == FinderChoice::Agent;
    let pf = if agent {
        preflight::build_agent_finder(&inputs, controls.finder_force_pieces)
    } else {
        preflight::build(&inputs)
    };

    let mut verdict: Result<(), InspectError> = Ok(());
    flags.extend(
        pf.text
            .lines()
            .map(str::trim)
            .filter(|l| l.starts_with("WARNING") || l.starts_with("note:"))
            .map(str::to_string),
    );
    if agent {
        let finder_model = camerata_server::ai_audit::agent_finder::finder_model_for(&controls);
        let empty = camerata_rules::RuleSet::default();
        writeln!(out, "\n== PIECES (the rooms a real run would build) ==").ok();
        for fact in &facts {
            let audit_rules =
                camerata_server::scan_routing::audit_rules(&fact.semantic, corpus.as_ref());
            let figures = piece_figures(
                &fact.repo,
                &fact.files,
                &audit_rules,
                corpus.as_ref().unwrap_or(&empty),
                controls.finder_force_pieces,
                controls.efficiency.applies_to_routing,
                &finder_model,
            );
            writeln!(out, " {}:", fact.repo).ok();
            for (i, p) in figures.iter().enumerate() {
                writeln!(out, "{}", piece_line(i, figures.len(), p)).ok();
                if p.over_guard() {
                    flags.push(format!(
                        "WARNING: {} piece {} would hold at least {} tokens once every file is \
                         read, over the context guard of {}; a real run would stop it and cut it \
                         in two on a resume",
                        fact.repo,
                        i + 1,
                        n(p.peak_tokens),
                        n(p.guard_tokens)
                    ));
                }
            }
        }
    }
    writeln!(out, "\n== PRE-FLIGHT (as a real run prints it) ==").ok();
    out.push_str(&pf.text);

    // The spend limit, decided by the run's own check.
    match (controls.max_cost_usd, pf.estimate_usd) {
        (Some(cap), projected) => {
            writeln!(
                out,
                "\nspend limit it would run under: ${cap:.2} (cost basis: {BASIS})"
            )
            .ok();
            if agent {
                // The real run's own plan, from the function the real run builds it with.
                if let Some(plan) =
                    preflight::agent_finder_checkpoint_plan(&facts, controls.finder_force_pieces)
                {
                    controls.checkpoint.plan = Some(plan);
                }
                let paused: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
                let slot = paused.clone();
                controls.checkpoint.run_folder = None;
                controls.checkpoint.sink =
                    camerata_server::cost_checkpoint::CheckpointSink::new(move |cp| {
                        slot.lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .get_or_insert_with(|| cp.pause_message());
                    });
                let meter = UsageMeter::with_controls(controls.clone());
                // The order a real run checks in: the clone judge's stage boundary (it comes
                // before the finder when a clone rule is selected, and prices the stages without
                // the joints calls), then the finder's first piece. The first pause is the
                // run's.
                let first_repo = facts.first().map_or("", |f| f.repo.as_str());
                let judge_stops = judge_selected
                    && meter.stage_blocked_unless_stored(
                        first_repo,
                        camerata_server::ai_audit::STAGE_CLONE_JUDGE,
                        false,
                    );
                if judge_stops || meter.projection_stops_piece() {
                    let why = paused
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .clone()
                        .unwrap_or_default();
                    flags.push(format!(
                        "WARNING: the spend limit is under the projection ({BASIS}); a real run \
                         would stop before its first piece. {why}"
                    ));
                    verdict = Err(InspectError::Failed(format!(
                        "a real run would refuse to go on: the spend limit is under the \
                         projection ({BASIS}). {why}"
                    )));
                }
            } else if batch {
                // The engine finder on a batch: the run's own gate, checked before a submission,
                // with the pre-flight estimate as its projection (no exact count is made here).
                controls.estimate_hint_usd = projected;
                let meter = UsageMeter::with_controls(controls.clone());
                if let Some(why) = meter.batch_refusal_reason(None) {
                    flags.push(format!("WARNING: {why}"));
                    verdict = Err(InspectError::Failed(format!(
                        "a real run would refuse to go on ({BASIS}): {why}"
                    )));
                }
            } else if projected
                .is_some_and(|p| camerata_server::cost_checkpoint::over_limit(p, cap))
            {
                flags.push(format!(
                    "WARNING: the estimate ({BASIS}) is above the spend limit; a real run is \
                     expected to stop before it finishes"
                ));
            }
        }
        (None, _) => {
            writeln!(out, "\nspend limit it would run under: none").ok();
        }
    }
    if flags.is_empty() {
        writeln!(out, "\nwarnings: none").ok();
    } else {
        writeln!(out, "\nwarnings ({}):", flags.len()).ok();
        for f in &flags {
            writeln!(out, "  {f}").ok();
        }
    }
    Ok((out, verdict))
}
