//! `camerata repeatability`: run the full AI pass twice over one repo, with every answer cache
//! bypassed, and report how the two runs differ (H5 item 2).
//!
//! A model is not a pure function, so two runs of the same scan can disagree. This command
//! measures by how much: finding counts by tier, rule and severity in each run, and every
//! finding one run reported and the other did not, matched by
//! [`camerata_server::repeatability::finding_match_key`] (repo, path and the content
//! fingerprint baselines use). Output is text or JSON.
//!
//! It spends real money twice, so it is guarded harder than `camerata inspect`:
//! - the pre-flight estimate is printed first, before any model call;
//! - it refuses to run without an explicit `--max-cost-usd`, which covers both runs together
//!   and is split evenly (each run gets half, so neither run can starve the other);
//! - it refuses when one run's estimate is above its half, because a run the limit stops part
//!   way would make the comparison meaningless.
//!
//! "Every answer cache bypassed" means the two caches that can hand a run a previous run's
//! answers: the incremental-scan manifest (never loaded; this command also never writes one)
//! and the result cache (`result_cache` is forced off whatever the flags or environment say).
//! The provider's prompt cache stays as configured: it stores the processed prompt prefix,
//! not an answer, so it cannot make two runs agree.
//!
//! The pipeline glue is the same as `camerata inspect`'s Phases 1 to 3 (stack detection, the
//! curated rule selection, the corpus, `audit_repos`), then the corpus severity bounds, so the
//! severities compared are the ones a report would show. The preview-tool, dependency and export
//! phases are deterministic and are not run. Nothing is written to disk.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;

use camerata_server::ai_audit::ScanMode;
use camerata_server::llm::{resolve_backend, BackendResolution, ProjectBackend};
use camerata_server::repeatability::{diff_runs, CountRow, RunDiff, UnmatchedFinding};
use serde::Serialize;

use crate::inspect_cmd::{curated_rule_selection, preflight};

/// Exit code when either run did not complete (a pass failed, or the spend limit stopped it):
/// the comparison is printed but cannot be trusted. Same value `camerata inspect` uses for a
/// run the spend limit stopped.
pub const EXIT_INCOMPLETE: i32 = 4;

/// Failed-pass names that cannot change WHICH findings a run reports, only the effort estimate
/// or the fix text attached to them. Every other failed pass (a scan pass that gave no answer,
/// the alternatives pass that picks which option a rule is judged against, anything this list
/// has never heard of) makes the run incomplete, so an unknown failure errs toward a flagged
/// comparison rather than a silently trusted one.
const FINDING_NEUTRAL_PASSES: &[&str] = &["hour estimation", "fix-specific generation"];

/// Output format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Text,
    Json,
}

/// clap value parser for `--format`.
pub fn parse_format(s: &str) -> Result<OutputFormat, String> {
    match s.trim().to_ascii_lowercase().as_str() {
        "text" => Ok(OutputFormat::Text),
        "json" => Ok(OutputFormat::Json),
        other => Err(format!("unknown format `{other}`; use text or json")),
    }
}

/// Parsed arguments for `camerata repeatability`.
#[derive(Debug, Clone)]
pub struct RepeatabilityArgs {
    pub repo: PathBuf,
    pub backend: ProjectBackend,
    pub model: Option<String>,
    pub calibration_model: Option<String>,
    /// The spend limit for BOTH runs together. Required: `None` is refused after the estimate.
    pub max_cost_usd: Option<f64>,
    /// `--efficiency name=on|off` entries, applied over `CAMERATA_EFFICIENCY`. `result_cache`
    /// is forced off afterwards regardless.
    pub efficiency: Vec<String>,
    pub format: OutputFormat,
}

/// The test seams. Production passes no model and prints to stdout.
pub struct Seams<'a> {
    /// A scripted model to run both passes over instead of the real `Llm` (no network).
    pub llm: Option<&'a dyn camerata_server::llm::LlmPort>,
    /// Receives the pre-flight estimate BEFORE the first model call.
    pub preflight_sink: &'a (dyn Fn(&str) + Sync),
    /// Called with the run number (1 or 2) just before that run starts.
    pub on_run_start: &'a (dyn Fn(usize) + Sync),
}

/// One run's headline facts.
#[derive(Debug, Clone, Serialize)]
pub struct RunSummary {
    pub run: usize,
    pub findings: usize,
    /// Actual spend, when the run reported one.
    pub cost_usd: Option<f64>,
    /// False when the AI review failed, the spend limit stopped something, or a pass that can
    /// change which findings are reported failed (see [`FINDING_NEUTRAL_PASSES`]).
    pub complete: bool,
    /// Every failed pass, one line each, including the ones that leave the run complete.
    pub notes: Vec<String>,
}

/// Everything the command reports.
#[derive(Debug, Clone, Serialize)]
pub struct RepeatabilityOutcome {
    pub runs: Vec<RunSummary>,
    /// The efficiency set both runs used (`result_cache=off` always).
    pub efficiency: String,
    pub identical: bool,
    pub complete: bool,
    pub diff: RunDiff,
}

impl RepeatabilityOutcome {
    fn new(runs: Vec<RunSummary>, efficiency: String, diff: RunDiff) -> Self {
        Self {
            complete: runs.iter().all(|r| r.complete),
            identical: diff.identical(),
            runs,
            efficiency,
            diff,
        }
    }

    /// The process exit code: 0 when both runs completed (identical or not), else
    /// [`EXIT_INCOMPLETE`].
    pub fn exit_code(&self) -> i32 {
        if self.complete {
            0
        } else {
            EXIT_INCOMPLETE
        }
    }

    /// Pretty JSON.
    pub fn render_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
    }

    /// The plain-text report.
    pub fn render_text(&self) -> String {
        let mut out = String::new();
        writeln!(
            out,
            "== REPEATABILITY: the full AI pass run twice, every answer cache bypassed =="
        )
        .ok();
        for r in &self.runs {
            let cost = r
                .cost_usd
                .map(|c| format!("${c:.2}"))
                .unwrap_or_else(|| "cost unavailable".to_string());
            let state = if r.complete { "complete" } else { "INCOMPLETE" };
            writeln!(
                out,
                "  run {}: {} findings, {cost}, {state}",
                r.run, r.findings
            )
            .ok();
            for note in &r.notes {
                writeln!(out, "    {note}").ok();
            }
        }
        writeln!(out, "  efficiency: {}", self.efficiency).ok();
        let d = &self.diff;
        writeln!(
            out,
            "  matched by fingerprint: {}; only in run 1: {}; only in run 2: {}; {}",
            d.matched,
            d.only_in_first.len(),
            d.only_in_second.len(),
            if self.identical {
                "the runs agree"
            } else {
                "the runs DIFFER"
            }
        )
        .ok();
        if !self.complete {
            writeln!(
                out,
                "  !! at least one run is incomplete, so part of this difference may be coverage, \
                 not drift"
            )
            .ok();
        }
        render_table(&mut out, "By tier", &d.by_tier, false);
        render_table(&mut out, "By severity", &d.by_severity, false);
        render_table(
            &mut out,
            "By rule (rules whose count changed)",
            &d.by_rule,
            true,
        );
        render_unmatched(&mut out, "Only in run 1", &d.only_in_first);
        render_unmatched(&mut out, "Only in run 2", &d.only_in_second);
        out
    }
}

fn render_table(out: &mut String, title: &str, rows: &[CountRow], changed_only: bool) {
    let shown: Vec<&CountRow> = rows
        .iter()
        .filter(|r| !changed_only || r.delta() != 0)
        .collect();
    let width = shown.iter().map(|r| r.key.len()).max().unwrap_or(0).max(8);
    writeln!(out, "\n{title}").ok();
    if shown.is_empty() {
        writeln!(out, "  (none)").ok();
        return;
    }
    writeln!(
        out,
        "  {:<width$}  {:>6}  {:>6}  {:>6}",
        "", "run 1", "run 2", "change"
    )
    .ok();
    for r in shown {
        writeln!(
            out,
            "  {:<width$}  {:>6}  {:>6}  {:>+6}",
            r.key,
            r.first,
            r.second,
            r.delta()
        )
        .ok();
    }
    if changed_only {
        let unchanged = rows.len() - rows.iter().filter(|r| r.delta() != 0).count();
        writeln!(
            out,
            "  ({unchanged} rules with the same count in both runs)"
        )
        .ok();
    }
}

fn render_unmatched(out: &mut String, title: &str, findings: &[UnmatchedFinding]) {
    writeln!(out, "\n{title}").ok();
    if findings.is_empty() {
        writeln!(out, "  (none)").ok();
    }
    for f in findings {
        writeln!(
            out,
            "  [{}] {} {} {}/{}:{} (fingerprint {})",
            f.tier, f.severity, f.rule_id, f.repo, f.path, f.line, f.fingerprint
        )
        .ok();
    }
}

/// Production entry point: the key saved in the app (with `ANTHROPIC_API_KEY` as the
/// fallback), the real model, output to stdout.
pub async fn run_repeatability(args: RepeatabilityArgs) -> Result<RepeatabilityOutcome, String> {
    let store = camerata_server::credentials::KeyringCredentialStore;
    let has_api_key = crate::inspect_cmd::api_key_present(&store);
    crate::inspect_cmd::require_api_key_for_client_inspection(args.backend, has_api_key)
        .map_err(|e| e.to_string())?;
    run_repeatability_with(
        args,
        has_api_key,
        &store,
        Seams {
            llm: None,
            preflight_sink: &|text| print!("{text}"),
            on_run_start: &|run| eprintln!("camerata repeatability: starting run {run} of 2"),
        },
    )
    .await
}

/// The refusal when no spend limit was given.
pub const NO_CAP_REFUSAL: &str = "refusing to run: `camerata repeatability` runs the full AI \
    pass twice and needs an explicit --max-cost-usd (the limit for both runs together; each run \
    gets half). The estimate above is a guide to the amount.";

/// The command with its seams explicit. See the module docs for the order of the guards.
pub async fn run_repeatability_with(
    args: RepeatabilityArgs,
    has_api_key: bool,
    creds: &dyn camerata_server::credentials::CredentialStore,
    seams: Seams<'_>,
) -> Result<RepeatabilityOutcome, String> {
    if !args.repo.is_dir() {
        return Err(format!(
            "--repo {} does not exist or is not a directory",
            args.repo.display()
        ));
    }
    let mut efficiency = camerata_server::efficiency::EfficiencyConfig::from_env()?;
    for entry in &args.efficiency {
        efficiency
            .apply_entry(entry)
            .map_err(|e| format!("--efficiency: {e}"))?;
    }
    // Never reuse a previous run's answers, whatever the flags say.
    efficiency.result_cache = false;
    let cap = args
        .max_cost_usd
        .map(camerata_server::cost_cap::RunControls::validate_cap)
        .transpose()?;

    // ── The same Phases 1 and 2 as `camerata inspect` ─────────────────────────────────────
    let repo_spec = args
        .repo
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("repo")
        .to_string();
    let sources = vec![(repo_spec, args.repo.clone())];
    let scan = camerata_server::onboard::scan_repos(&sources, Vec::new()).await;
    let selected = curated_rule_selection(&scan.proposed_rules);
    let (_ai_only, _excluded, _preview, corpus, _load_failures) =
        camerata_server::split_scannable_rules(selected.clone()).await;

    let backend_resolution = resolve_backend(args.backend, has_api_key);
    if let BackendResolution::Blocked { message } = &backend_resolution {
        return Err(format!(
            "the AI review cannot run with this backend ({message}), so there is nothing to repeat"
        ));
    }
    let model = args.model.as_deref().filter(|m| !m.trim().is_empty());
    let calibration_model = args
        .calibration_model
        .as_deref()
        .filter(|m| !m.trim().is_empty());

    // ── The estimate, printed first ──────────────────────────────────────────────────────
    let facts =
        camerata_server::preflight::gather_scan_facts(&sources, &selected, corpus.as_ref()).await;
    let exact = if seams.llm.is_none() && matches!(backend_resolution, BackendResolution::Api) {
        let llm =
            camerata_server::onboard::build_llm(creds, camerata_server::llm::Backend::Api, None);
        camerata_server::preflight::exact_sample_tokens(
            &llm,
            &facts,
            ScanMode::Parallel,
            model,
            efficiency,
        )
        .await
    } else {
        None
    };
    let audit_model = camerata_server::ai_audit::resolve_audit_model(model);
    let pf = preflight::build(&preflight::PreflightInputs {
        backend: args.backend,
        batch: false,
        calibration_model: calibration_model
            .map(str::to_string)
            .unwrap_or_else(|| audit_model.clone()),
        model: audit_model,
        repos: &facts,
        incremental: false,
        cap_usd: cap,
        exact,
        terse_output: efficiency.terse_output,
        consensus: false,
    });
    let mut text = pf.text.clone();
    match pf.estimate_usd {
        Some(one) => writeln!(
            text,
            "  repeatability: two full runs, about ${:.2} together",
            one * 2.0
        )
        .ok(),
        None => writeln!(text, "  repeatability: two full runs").ok(),
    };
    (seams.preflight_sink)(&text);

    // ── The spend guards ─────────────────────────────────────────────────────────────────
    let cap = cap.ok_or_else(|| NO_CAP_REFUSAL.to_string())?;
    let per_run = cap / 2.0;
    if let Some(one) = pf.estimate_usd {
        if one > per_run {
            return Err(format!(
                "refusing to run: one full run is estimated at about ${one:.2}, more than its half \
                 of --max-cost-usd (${per_run:.2}). A run the limit stops part way would make \
                 the comparison meaningless; set --max-cost-usd to at least {:.2}.",
                one * 2.0
            ));
        }
    }
    let controls = camerata_server::cost_cap::RunControls {
        efficiency,
        max_cost_usd: Some(per_run),
        estimate_hint_usd: pf.estimate_usd,
        // No explicit fix model: the fix stage follows the `fix_on_haiku` measure, as in a default run.
        fix_model: None,
        // Repeatability measures the drift of the default configuration.
        consensus: false,
    };
    let ids: Vec<&str> = selected.iter().map(|r| r.id.as_str()).collect();
    let models = camerata_server::cost_cap::models_the_run_will_use(
        model,
        calibration_model,
        false,
        &ids,
        &controls,
    );
    if let Some(why) = controls.refuse_if_cap_unenforceable(&models) {
        return Err(why);
    }

    // ── Two runs, nothing carried between them ───────────────────────────────────────────
    let mut summaries = Vec::new();
    let mut finding_sets = Vec::new();
    for run in 1..=2 {
        (seams.on_run_start)(run);
        let (mut report, _manifest) = match seams.llm {
            Some(llm) => {
                camerata_server::onboard::audit_repos_with_llm_controlled(
                    llm,
                    &sources,
                    &selected,
                    Vec::new(),
                    model,
                    calibration_model,
                    ScanMode::Parallel,
                    false,
                    None,
                    None,
                    None, // incremental_prior: never a previous run's answers
                    false,
                    true,
                    true,
                    true,
                    backend_resolution.clone(),
                    corpus.as_ref(),
                    &HashMap::new(),
                    controls.clone(),
                )
                .await
            }
            None => {
                camerata_server::onboard::audit_repos_controlled(
                    creds,
                    &sources,
                    &selected,
                    Vec::new(),
                    model,
                    calibration_model,
                    ScanMode::Parallel,
                    false,
                    None,
                    None,
                    None, // incremental_prior: never a previous run's answers
                    false,
                    true,
                    true,
                    true,
                    Some(Arc::new(camerata_server::usage_ledger::UsageLedger::new())),
                    backend_resolution.clone(),
                    corpus.as_ref(),
                    &HashMap::new(),
                    controls.clone(),
                )
                .await
            }
        };
        camerata_server::severity_bounds::apply_scan_severity_bounds(
            &mut report.findings,
            corpus.as_ref(),
        );
        let mut notes: Vec<String> = report
            .failed_passes
            .iter()
            .map(|p| format!("{}: {}: {}", p.repo, p.pass, p.reason))
            .collect();
        if let Some(e) = &report.ai_error {
            notes.push(format!("AI review failed: {e}"));
        }
        let complete = report.ai_error.is_none()
            && !report
                .ledger
                .cost_cap()
                .is_some_and(|c| c.stopped_the_run())
            && report
                .failed_passes
                .iter()
                .all(|p| FINDING_NEUTRAL_PASSES.contains(&p.pass.as_str()));
        summaries.push(RunSummary {
            run,
            findings: report.findings.len(),
            cost_usd: report.actual_usage.as_ref().map(|u| u.cost_usd),
            complete,
            notes,
        });
        finding_sets.push(report.findings);
    }

    let diff = diff_runs(&finding_sets[0], &finding_sets[1]);
    Ok(RepeatabilityOutcome::new(
        summaries,
        efficiency.render(),
        diff,
    ))
}
