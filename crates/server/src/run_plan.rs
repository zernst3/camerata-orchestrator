//! The plan of an inspection: what a real run would take on, as figures, with no model call, no
//! network, no credential read and nothing saved. One computation for `camerata inspect
//! --plan-only` and the app's pre-flight (`POST /api/onboard/plan`), so the two never diverge.
//!
//! It runs the front half of a real run (the refusals a run makes before its estimate, the
//! admission walk and the pre-flight) and the room building of the agent finder, then stops
//! before the first model call. It never opens the state folder for writing. What it reports is
//! counts, sizes and paths relative to the repository, never file text.
//!
//! The spend-limit check is the run's own: the checkpoint plan the pre-flight fills is handed to
//! a meter under the same controls, and the meter's `projection_stops_piece` decides, exactly as
//! it does before a real run's first piece.
//!
//! Each caller resolves its own inputs (the command line from its flags, the app from the active
//! project) and then hands them here; everything from the first refusal to the last warning is
//! this module's.

use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::ai_audit::agent_finder::run::{piece_figures, PieceFigures};
use crate::ai_audit::agent_finder::FinderChoice;
use crate::ai_audit::{ScanMode, UsageMeter};
use crate::llm::{BackendResolution, ProjectBackend};
use crate::onboard::files::ExtractedRepo;
use crate::preflight::full_scan::FullScanEstimate;
use crate::preflight::wording;
use crate::scan_ledger::SkipReason;

/// The basis every cost figure here is printed under.
pub const BASIS: &str = "projected, API prices";

/// What a caller resolved before the plan: the repos, the rules, the controls and the models.
pub struct PlanInputs {
    /// Each repo's name and local folder.
    pub sources: Vec<(String, PathBuf)>,
    /// The rules the run checks.
    pub selected: Vec<crate::onboard::SelectedRule>,
    /// The loaded rule library, when it loaded.
    pub corpus: Option<camerata_rules::RuleSet>,
    /// The rule files that did not load.
    pub corpus_load_failures: Vec<crate::CorpusLoadFailure>,
    /// The run's controls: the spend limit, the exclusion list, the finder and its settings.
    pub controls: crate::cost_cap::RunControls,
    pub backend: ProjectBackend,
    pub backend_resolution: BackendResolution,
    /// The mode the caller asked for (`None` lets the run decide).
    pub requested_mode: Option<ScanMode>,
    /// The audit and calibration models the caller picked (`None` or blank for the default).
    pub model: Option<String>,
    pub calibration_model: Option<String>,
    /// An earlier inspection's baseline exists and the run would build on it.
    pub incremental: bool,
    pub client_engagement: bool,
    /// The warnings the start checks gave (a file over the read limit, for one), each printed as
    /// `warning: <text>`.
    pub start_warnings: Vec<String>,
}

/// The files of one repo a run would leave out for one reason.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LeftOut {
    pub reason: String,
    pub files: usize,
}

/// What a real run would read of one repo.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RepoAdmission {
    pub repo: String,
    pub files: usize,
    pub lines: usize,
    pub bytes: usize,
    pub left_out: Vec<LeftOut>,
}

/// The plan: the figures, the wording a real run prints, every warning and the refusal a real run
/// would make, when it would make one.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RunPlan {
    pub repos: Vec<RepoAdmission>,
    pub total_files: usize,
    pub total_lines: usize,
    pub total_bytes: usize,
    /// The projected cost of the run (`None` when a model has no list price).
    pub projected_usd: Option<f64>,
    /// The full-scan estimate `camerata count --estimate` prints, with the suggested limit.
    pub full_scan: FullScanEstimate,
    /// The spend limit the run would go under (`None` for no limit).
    pub limit_usd: Option<f64>,
    pub warnings: Vec<String>,
    /// Why a real run would not start or would stop before its first piece.
    pub refusal: Option<String>,
    /// The whole plan as `camerata inspect --plan-only` prints it after its two header lines.
    pub text: String,
}

/// `n` with thousands separators.
pub fn n(v: usize) -> String {
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

/// The files a repo's walk left out, by the reason Coverage gives, counts only.
fn left_out(extracted: &ExtractedRepo) -> Vec<LeftOut> {
    let mut rows = vec![
        (SkipReason::Oversize, extracted.skipped_oversize.len()),
        (SkipReason::LongLine, extracted.skipped_long_line.len()),
        (SkipReason::NotText, extracted.skipped_unreadable.len()),
        (
            SkipReason::BeyondFileCap,
            extracted.skipped_beyond_cap.len(),
        ),
        (
            SkipReason::ExcludedByAgreement,
            extracted.excluded_by_agreement.len(),
        ),
        (SkipReason::Generated, extracted.generated.len()),
        (SkipReason::NotInspectedType, extracted.not_inspected.len()),
        (SkipReason::Symlink, extracted.symlinks.len()),
    ];
    rows.retain(|(_, count)| *count > 0);
    rows.into_iter()
        .map(|(reason, files)| LeftOut {
            reason: reason.label(),
            files,
        })
        .collect()
}

/// Whether an incremental-scan baseline exists for this run: a read only look, never the removal
/// of an outdated one that a real run's loader does.
pub fn prior_baseline_exists(path: &std::path::Path) -> bool {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<crate::scan_cache::ScanManifest>(&bytes).ok())
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

/// The plan for `inputs`. `Err` names the refusal a real run makes before its estimate (the run
/// would not start and there are no figures to show); `Ok` carries the figures and, in
/// [`RunPlan::refusal`], the refusal a real run would make once it has them.
#[allow(clippy::too_many_lines)]
pub async fn plan(inputs: PlanInputs) -> Result<RunPlan, String> {
    let PlanInputs {
        sources,
        selected,
        corpus,
        corpus_load_failures,
        mut controls,
        backend,
        backend_resolution,
        requested_mode,
        model,
        calibration_model,
        incremental,
        client_engagement,
        start_warnings,
    } = inputs;
    // What a real run prints to stderr at its start is part of the plan's own warnings.
    let mut flags: Vec<String> = start_warnings
        .iter()
        .flat_map(|w| {
            format!("warning: {w}")
                .lines()
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .collect();

    let mode_choice = crate::ai_audit::resolve_scan_mode(
        requested_mode,
        &backend_resolution,
        controls.efficiency,
    );
    if let Some(notice) = &mode_choice.notice {
        flags.push(format!("note: {notice}"));
    }
    let batch = mode_choice.mode == ScanMode::Batch;
    let model = model.as_deref().filter(|m| !m.trim().is_empty());
    let calibration_model = calibration_model
        .as_deref()
        .filter(|m| !m.trim().is_empty());

    // The refusals a real run makes before its estimate, in the same order, from the same
    // functions.
    crate::ai_audit::agent_finder::run::refuse_unsupported_finder_settings(
        &controls,
        incremental,
        !client_engagement,
    )?;
    crate::ai_audit::agent_finder::run::refuse_unloadable_rules(
        &controls,
        !matches!(backend_resolution, BackendResolution::Blocked { .. }),
        &corpus_load_failures,
    )?;
    if matches!(backend_resolution, BackendResolution::Blocked { .. }) {
        return Err(
            "no model can be called on this backend, so a real run would not start".to_string(),
        );
    }
    let ids: Vec<&str> = selected.iter().map(|r| r.id.as_str()).collect();
    let models =
        crate::cost_cap::models_the_run_will_use(model, calibration_model, false, &ids, &controls);
    if let Some(why) = controls.refuse_if_cap_unenforceable(&models) {
        return Err(why);
    }
    // The cli backend launches the local Claude Code, which refuses a model it predates.
    if matches!(backend_resolution, BackendResolution::Cli) {
        crate::claude_version::check_run_models(&models)?;
    }

    // The facts the pre-flight prices, and the coverage reasons for what was left out.
    let facts =
        crate::preflight::gather_scan_facts(&sources, &selected, corpus.as_ref(), &controls.scope)
            .await;
    if let Some(bad) = facts.iter().find(|f| f.read_error.is_some()) {
        return Err(format!(
            "could not read {}: {}",
            bad.repo,
            bad.read_error.as_deref().unwrap_or("unknown error")
        ));
    }
    let mut out = String::new();
    writeln!(out, "\n== ADMISSION (what a real run would read) ==").ok();
    let mut repos = Vec::new();
    let (mut t_files, mut t_lines, mut t_bytes, mut counted) = (0usize, 0usize, 0usize, 0usize);
    for ((spec, dir), fact) in sources.iter().zip(&facts) {
        let repo_scope = controls.scope.for_repo(spec);
        let dir = dir.clone();
        let walk = tokio::task::spawn_blocking(move || {
            crate::onboard::files::read_local_repo_files(&dir, &repo_scope)
        })
        .await
        .map_err(|e| format!("could not read {spec}: {e}"))?
        .map_err(|e| format!("could not read {spec}: {e}"))?;
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
            for row in &rows {
                writeln!(out, "    left out: {} {}", n(row.files), row.reason).ok();
            }
        }
        // The lines `camerata count` counts: the non-blank lines of the files the walk admits.
        counted += crate::onboard::propose::count_source_lines(&walk.files).0;
        repos.push(RepoAdmission {
            repo: fact.repo.clone(),
            files: fact.files.len(),
            lines,
            bytes: fact.code_chars,
            left_out: rows,
        });
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
    let audit_model = crate::ai_audit::resolve_audit_model(model);
    let judge_selected = models
        .iter()
        .any(|(roles, _)| roles.contains("the clone judge"));
    let pf_inputs = wording::PreflightInputs {
        backend,
        batch,
        fix_model: Some(crate::ai_audit::resolve_audit_model(
            controls.fix_model_for(model, calibration_model),
        )),
        clone_judge_model: judge_selected.then(|| controls.clone_judge_model_for()),
        calibration_model: crate::ai_audit::resolve_calibration_model(
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
        wording::build_agent_finder(&pf_inputs, controls.finder_force_pieces)
    } else {
        wording::build(&pf_inputs)
    };

    let mut refusal: Option<String> = None;
    flags.extend(
        pf.text
            .lines()
            .map(str::trim)
            .filter(|l| l.starts_with("WARNING") || l.starts_with("note:"))
            .map(str::to_string),
    );
    if agent {
        let finder_model = crate::ai_audit::agent_finder::finder_model_for(&controls);
        let empty = camerata_rules::RuleSet::default();
        writeln!(out, "\n== PIECES (the rooms a real run would build) ==").ok();
        for fact in &facts {
            let audit_rules = crate::scan_routing::audit_rules(&fact.semantic, corpus.as_ref());
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

    // The full-scan estimate and its suggested limit, as `camerata count --estimate` prints them.
    let full_scan = FullScanEstimate::of(counted, None);
    writeln!(
        out,
        "\n== FULL-SCAN ESTIMATE (as camerata count --estimate prints it) =="
    )
    .ok();
    writeln!(out, "  {}", full_scan.text("Full-scan estimate")).ok();

    // The spend limit, decided by the run's own check.
    match (controls.max_cost_usd, pf.estimate_usd) {
        (Some(cap), projected) => {
            writeln!(
                out,
                "\nspend limit it would run under: ${cap:.2} (cost basis: {BASIS})"
            )
            .ok();
            if agent {
                let parts: Vec<_> = facts
                    .iter()
                    .map(|r| {
                        crate::preflight::agent_finder_estimate(r, controls.finder_force_pieces)
                    })
                    .collect();
                let total = camerata_ui_core::scan::sum_agent_finder(&parts);
                if total.pieces > 0 {
                    let joints = total.joints_calls as f64
                        * camerata_ui_core::scan::AGENT_FINDER_USD_PER_JOINTS_CALL;
                    controls.checkpoint.plan = Some(crate::cost_checkpoint::CheckpointPlan {
                        pieces: total.pieces,
                        per_piece_usd: ((total.expected_usd - joints) / total.pieces as f64)
                            .max(0.0),
                        joints_usd: joints,
                        ..crate::cost_checkpoint::CheckpointPlan::default()
                    });
                }
                let paused: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
                let slot = paused.clone();
                controls.checkpoint.run_folder = None;
                controls.checkpoint.sink = crate::cost_checkpoint::CheckpointSink::new(move |cp| {
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
                        crate::ai_audit::STAGE_CLONE_JUDGE,
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
                    refusal = Some(format!(
                        "a real run would refuse to go on: the spend limit is under the \
                         projection ({BASIS}). {why}"
                    ));
                }
            } else if batch {
                // The engine finder on a batch: the run's own gate, checked before a submission,
                // with the pre-flight estimate as its projection (no exact count is made here).
                controls.estimate_hint_usd = projected;
                let meter = UsageMeter::with_controls(controls.clone());
                if let Some(why) = meter.batch_refusal_reason(None) {
                    flags.push(format!("WARNING: {why}"));
                    refusal = Some(format!("a real run would refuse to go on ({BASIS}): {why}"));
                }
            } else if projected.is_some_and(|p| crate::cost_checkpoint::over_limit(p, cap)) {
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
    Ok(RunPlan {
        repos,
        total_files: t_files,
        total_lines: t_lines,
        total_bytes: t_bytes,
        projected_usd: pf.estimate_usd,
        full_scan,
        limit_usd: controls.max_cost_usd,
        warnings: flags,
        refusal,
        text: out,
    })
}
