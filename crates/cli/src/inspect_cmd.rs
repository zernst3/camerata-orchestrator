//! `camerata inspect` — Step 0 of the refinement loop (see
//! `<workspace>/brownfield-audits/CAMERATA_LOOP_HANDOFF.md`): a HEADLESS
//! brownfield inspection that runs the exact pipeline the cockpit's "Run audit" + "product
//! export" buttons drive, with no UI and no running BFF, so it can be scripted from a shell
//! (CI, batch scans, and the refinement loop that needs to grade a scan without a browser).
//!
//! Deliberately thin: every heavy-lifting call goes straight into `camerata_server`'s own
//! functions —
//! - [`camerata_server::onboard::scan_repos`] (stack detection + rule proposal),
//! - [`camerata_server::split_scannable_rules`] / [`camerata_server::merge_scan_preview`] (the
//!   same rule-tiering + scan-time mechanical-preview pass `onboard_audit`'s HTTP handler
//!   runs),
//! - [`camerata_server::onboard::audit_repos`] (the two-tier scan: the deterministic floor +
//!   the AI review, calibration, and per-rule alternative recommendation whenever a corpus is
//!   supplied — see that function's own doc comment on `corpus`/`chosen_options`),
//! - [`camerata_server::dep_audit::run_dep_audit`] (the dependency-vulnerability pass),
//! - [`camerata_server::report_export::build_report_json`] / `compile_pdf`,
//!   [`camerata_server::xlsx_export::build_workbook`] / `build_findings_export`, and
//!   [`camerata_server::report_filename_stem`] / `product_export_readme` / `build_product_zip`
//!   — the SAME functions `POST /api/projects/:id/product-export` (`export_product` in
//!   `camerata_server::lib`) calls, in the same order, producing the same zip.
//!
//! Nothing here re-implements any of that. This module supplies only the headless GLUE a
//! project-backed HTTP request would otherwise supply: turning a bare filesystem path into
//! the `(repo, PathBuf)` "sources" the server functions expect, picking the CURATION-DEFAULT
//! rule selection a fresh onboarding pre-checks (`ProposedRule::is_auto_recommended`, with no
//! human override — see [`curated_rule_selection`]), a tiny on-disk incremental-scan cache
//! (there is no project/database to key one to headlessly, so it is keyed by repo path in a
//! per-user state directory — see [`manifest_cache_path`] and [`camerata_state_dir`]), and
//! printing the run's real token usage + cost at the end.
//!
//! Neither the cache nor the integrity ledger is written into the scanned repository. They
//! were until 2026-10-18, and the engine then collected its own cache as source on the next
//! scan — see [`camerata_state_dir`] for the full account.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub(crate) mod preflight;

use camerata_server::ai_audit::{ActualUsage, FailedPass, ScanMode};
use camerata_server::llm::{resolve_backend, BackendResolution, ProjectBackend};
use camerata_server::onboard::{ProposedRule, SelectedRule};
use camerata_server::report_export::ReportOptions;
use camerata_server::scan_cache::ScanManifest;
// The per-user bookkeeping location and the per-repo key live in `camerata_server` (ONE
// implementation, shared with source deletion, which must find exactly the files this command
// wrote). See `camerata_server::scan_cache::state_dir` for the account of why they are kept
// out of the scanned repository.
pub use camerata_server::scan_cache::state_dir::ledger_path;
#[cfg(test)]
use camerata_server::scan_cache::state_dir::repo_state_key;
use camerata_server::scan_cache::state_dir::{dir as camerata_state_dir, manifest_cache_path};
use camerata_server::scan_ledger::{RuleLedgerEntry, RuleTier, ScanLedger};
use camerata_server::usage_ledger::UsageLedger;

/// Parsed + validated arguments for `camerata inspect`. Kept separate from clap's `Command`
/// enum variant so the pipeline ([`run_inspect`]) and its tests never depend on clap at all.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct InspectArgs {
    /// The repo's local working tree to inspect. Must exist and be a directory
    /// ([`validate_args`] checks eagerly).
    pub repo: PathBuf,
    /// Further repos of the SAME project (`--repo` given more than once). They are inspected
    /// together with [`Self::repo`]: one scan, one report, one cache, one ledger.
    pub also_repos: Vec<PathBuf>,
    /// `--repo-name OWNER/REPO`, one per repo in the order the repos were given (the first
    /// names [`Self::repo`]). Empty is allowed only for a single repo, which is then named for
    /// its folder as it always was. Each name goes through
    /// [`camerata_api_types::uow::validate_repo_name`] and must be unique: the name tags every
    /// finding and keys the incremental cache, so two repos sharing one would merge.
    pub repo_names: Vec<String>,
    /// Where to write the product-export ZIP (PDF + xlsx + findings.json + README.txt). Its
    /// parent directory must already exist.
    pub export: PathBuf,
    /// The compliance-safety backend setting (`docs/design/2026-09-22_per-project-backend.md`)
    /// — resolved against API-key presence via [`resolve_backend`], exactly like a real
    /// project's `backend` field. The `camerata inspect` flag defaults to `Api` (a client inspection
    /// runs on the Anthropic API); `Cli` is an explicit choice for internal benchmark runs.
    pub backend: ProjectBackend,
    /// `--batch`: FORCE the Anthropic Message Batches path (`ScanMode::Batch`): ~50% cheaper,
    /// latency is not returned until the whole batch completes. Requires `backend: Api` plus a
    /// configured key ([`validate_args`] rejects the combination eagerly otherwise). With
    /// neither this nor [`Self::realtime`] set the run's default applies
    /// ([`camerata_server::ai_audit::resolve_scan_mode`]): batch on the API backend unless
    /// `batch_default` is off.
    pub batch: bool,
    /// `--realtime`: FORCE real-time (parallel) calls. Exclusive with [`Self::batch`].
    pub realtime: bool,
    /// Explicit model override for the audit pass. `None`/blank falls back to
    /// [`camerata_server::llm::DEFAULT_MODEL`] — the same floor `step_model` uses for the
    /// project-less edge.
    pub model: Option<String>,
    /// Explicit model override for the calibration pass. Same fallback as `model`.
    pub calibration_model: Option<String>,
    /// `--fix-model`: the fix stage's explicit model. `None`/blank = not set, and the fix stage
    /// follows the `fix_on_haiku` measure (see `camerata_server::ai_audit::resolve_fix_model`).
    pub fix_model: Option<String>,
    /// Ignore the on-disk incremental-scan cache (see [`manifest_cache_path`]) and force a
    /// full re-scan of every file.
    pub full: bool,
    /// Additionally print the per-rule detail section of the pipeline-integrity ledger
    /// summary (see [`render_ledger_summary`]) — rule id, ran?, files evaluated, findings, one
    /// line per rule. Default output stays at the concise stage/family/not-run/failed-passes
    /// summary.
    pub verbose: bool,
    /// `--max-cost-usd`: stop between passes once actual spend reaches this many dollars. See
    /// [`camerata_server::cost_cap`].
    pub max_cost_usd: Option<f64>,
    /// `--efficiency name=on|off` entries (applied over `CAMERATA_EFFICIENCY`). See
    /// [`camerata_server::efficiency`].
    pub efficiency: Vec<String>,
    /// `--client`: who the inspection is for, printed in the export's confidentiality line on
    /// every page and sheet (the headless stand-in for a project's inspection-setup field).
    /// Required and checked by [`validate_args`] before anything is read or spent, so a run
    /// can never pay for an inspection whose export would then be refused.
    pub client: String,
    /// The run is a client engagement ([`camerata_server::client_lock`]): ON unless the operator
    /// passed `--internal` (the maintainer's own development). A client engagement refuses the
    /// `cli` backend and any non-Anthropic model before any file is read ([`validate_args`]),
    /// and skips every step that executes the client's own code unless `run_client_tooling`.
    pub client_engagement: bool,
    /// `--run-client-tooling`: the operator's explicit opt-in to run steps that execute the
    /// client's own code (today the `cargo clippy` scan preview, which builds the crate) in a
    /// client engagement. Recorded in the ledger and stated in the report either way.
    pub run_client_tooling: bool,
    /// `--consensus`: run the AI audit three times and keep what two runs agree on (see
    /// [`camerata_server::consensus`]). Off by default.
    pub consensus: bool,
}

impl InspectArgs {
    /// Every repo folder of the run, in the order given.
    pub fn repo_dirs(&self) -> Vec<PathBuf> {
        std::iter::once(self.repo.clone())
            .chain(self.also_repos.iter().cloned())
            .collect()
    }

    /// The `(name, folder)` sources the scan reads, validated: every folder exists, the names
    /// are valid `OWNER/REPO` coordinates and unique, and no folder is given twice.
    ///
    /// One repo with no `--repo-name` keeps the old behaviour (named for its folder). Several
    /// repos each need a `--repo-name`: a folder name is not a repository name, and two clients'
    /// repos are routinely both called `api`.
    pub fn resolve_sources(&self) -> Result<Vec<(String, PathBuf)>, String> {
        let dirs = self.repo_dirs();
        for dir in &dirs {
            if !dir.is_dir() {
                return Err(format!(
                    "--repo {} does not exist or is not a directory",
                    dir.display()
                ));
            }
        }
        if !self.repo_names.is_empty() && self.repo_names.len() != dirs.len() {
            return Err(format!(
                "--repo-name was given {} time(s) for {} --repo; give one name per repo, in the \
                 same order as the repos",
                self.repo_names.len(),
                dirs.len()
            ));
        }
        if dirs.len() > 1 && self.repo_names.is_empty() {
            return Err(format!(
                "{} repos were given with --repo, so each needs a --repo-name OWNER/REPO (a folder \
                 name is not a repository name, and two repos can share one)",
                dirs.len()
            ));
        }
        let mut sources: Vec<(String, PathBuf)> = Vec::with_capacity(dirs.len());
        for (i, dir) in dirs.iter().enumerate() {
            let name = match self.repo_names.get(i) {
                Some(name) => {
                    camerata_api_types::uow::validate_repo_name(name)
                        .map_err(|e| format!("--repo-name: {e}"))?;
                    name.clone()
                }
                // A single unnamed repo: the folder's own name, as before.
                None => dir
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("repo")
                    .to_string(),
            };
            if let Some((_, other)) = sources.iter().find(|(n, _)| *n == name) {
                return Err(format!(
                    "--repo-name {name} is used for both {} and {}; each repo needs its own name",
                    other.display(),
                    dir.display()
                ));
            }
            let canonical = dir.canonicalize().unwrap_or_else(|_| dir.clone());
            if let Some((other, _)) = sources
                .iter()
                .find(|(_, d)| d.canonicalize().unwrap_or_else(|_| d.clone()) == canonical)
            {
                return Err(format!(
                    "{} was given more than once (as {other} and as {name})",
                    dir.display()
                ));
            }
            sources.push((name, dir.clone()));
        }
        Ok(sources)
    }

    /// Where this run's incremental-scan cache and integrity ledger live: the single repo's own
    /// pair (so every key already on disk stays valid), or ONE pair for the whole set of repos.
    pub fn state_paths(&self) -> (PathBuf, PathBuf) {
        let dirs = self.repo_dirs();
        if dirs.len() == 1 {
            (manifest_cache_path(&dirs[0]), ledger_path(&dirs[0]))
        } else {
            (
                camerata_server::scan_cache::state_dir::project_manifest_cache_path(&dirs),
                camerata_server::scan_cache::state_dir::project_ledger_path(&dirs),
            )
        }
    }

    /// The client-tooling decision this run carries into the preview pass, the ledger and the
    /// report.
    pub fn client_tooling(&self) -> camerata_server::client_lock::ClientTooling {
        camerata_server::client_lock::ClientTooling::new(
            self.client_engagement,
            self.run_client_tooling,
        )
    }

    /// The run's controls: the environment's efficiency switches, then the flags over them, and
    /// the validated cap. `Err` on an unknown measure, a malformed entry or a bad cap, before
    /// anything is read or spent.
    pub fn run_controls(&self) -> Result<camerata_server::cost_cap::RunControls, String> {
        let mut efficiency = camerata_server::efficiency::EfficiencyConfig::from_env()?;
        for entry in &self.efficiency {
            efficiency
                .apply_entry(entry)
                .map_err(|e| format!("--efficiency: {e}"))?;
        }
        let max_cost_usd = self
            .max_cost_usd
            .map(camerata_server::cost_cap::RunControls::validate_cap)
            .transpose()?;
        Ok(camerata_server::cost_cap::RunControls {
            efficiency,
            max_cost_usd,
            estimate_hint_usd: None,
            fix_model: self.fix_model.clone(),
            consensus: self.consensus,
        })
    }
}

/// What [`run_inspect`] reports back to its caller. `main.rs`'s handler prints this; tests
/// assert on the struct directly rather than parsing stdout.
#[derive(Debug, Clone)]
pub struct InspectOutcome {
    pub export_path: PathBuf,
    pub export_zip_bytes: usize,
    pub backend: ProjectBackend,
    /// Whether the scan ran as a Message Batch (the resolved mode, forced or default).
    pub batch: bool,
    /// Real token usage + cost for this run's AI passes (audit + calibration), when the AI
    /// review actually ran. `None` on an AI-off / compliance-blocked (floor-only) run — see
    /// `ScanReport::actual_usage`'s own doc comment.
    pub actual_usage: Option<ActualUsage>,
    pub findings_count: usize,
    /// The pipeline-integrity ledger summary (see [`render_ledger_summary`]) — stages,
    /// detection families, not-run rules, and any `FailedPass` disclosures, rendered from
    /// `report.ledger`/`report.failed_passes` exactly as they stood after the scan's final
    /// reconciliation pass. Printed by [`format_outcome_report`]; tests assert on this field
    /// directly rather than parsing stdout, matching this module's existing convention.
    pub ledger_summary: String,
    /// Where this run's pipeline-integrity ledger was written, for passing to
    /// `camerata integrity-gate --ledger`.
    ///
    /// Reported because it is no longer guessable. It used to be
    /// `<repo>/.camerata-scan-ledger.json`, which an operator could type from memory; it now
    /// lives in a per-user state directory under a hashed key (see [`ledger_path`]). A path
    /// nobody can derive and nothing prints is a gate nobody runs.
    pub ledger_path: PathBuf,
    /// The release gate's verdict line (`release gate: passed (6 checks)`): proof, in the
    /// operator's output, that the gate ran over the zip that was just written.
    pub release_gate: String,
    /// Per-stage token, cache and cost telemetry for this run (also persisted on the operator
    /// ledger at [`ledger_path`]). Printed as a table at the end of the run; deliberately NOT
    /// part of the export. `None` when no model call ran.
    pub stage_telemetry: Option<camerata_server::stage_telemetry::StageTelemetry>,
    /// The pre-flight estimate that was printed before the first model call (`None` when no
    /// model call was going to be made).
    pub preflight: Option<String>,
    /// Which token-efficiency measures were on for this run.
    pub efficiency: camerata_server::efficiency::EfficiencyConfig,
    /// What the spend limit did, when one was set. Operator-only: it carries dollar figures.
    pub cost_cap: Option<camerata_server::cost_cap::CostCapRecord>,
    /// What an outage stop left undone, when the run stopped because the model provider could not
    /// be reached. Operator-only.
    pub outage: Option<camerata_server::run_health::OutageRecord>,
    /// Why the run did not complete ([`camerata_server::onboard::ScanReport::incomplete_reasons`]);
    /// empty for a complete run. Non-empty only on an `--internal` run, because a client run
    /// that is incomplete writes no export and returns [`InspectError::IncompleteRun`] instead.
    pub incomplete_reasons: Vec<String>,
    /// One structured line per failed part, for stderr (see
    /// [`camerata_server::failure_report`]). The full report is at the path the ledger summary
    /// names.
    pub failure_lines: Vec<String>,
    /// The saved run this inspection is (`camerata inspect --resume <id>` runs what remains).
    /// `None` when the run could not be saved.
    pub run_id: Option<String>,
}

impl InspectOutcome {
    /// True when the run stopped because the model provider could not be reached, so the
    /// inspection is incomplete and can be run again later.
    pub fn outage_stopped(&self) -> bool {
        self.outage.is_some()
    }

    /// The process exit code this outcome calls for: [`EXIT_OUTAGE_STOPPED`] when an outage
    /// stopped the run, [`EXIT_COST_CAPPED`] when the spend limit did, else 0.
    pub fn exit_code(&self) -> i32 {
        if self.outage_stopped() {
            EXIT_OUTAGE_STOPPED
        } else if self.cost_capped() {
            EXIT_COST_CAPPED
        } else if !self.incomplete_reasons.is_empty() {
            EXIT_INCOMPLETE_RUN
        } else {
            0
        }
    }

    /// True when the spend limit stopped part of the run, so the export is incomplete.
    pub fn cost_capped(&self) -> bool {
        self.cost_cap
            .as_ref()
            .is_some_and(camerata_server::cost_cap::CostCapRecord::stopped_the_run)
    }
}

/// The exit code `camerata inspect` uses when the release gate refuses the export (the
/// integrity gate's is 2). The zip is never written.
pub const EXIT_RELEASE_BLOCKED: i32 = 3;

/// The exit code when the spend limit (`--max-cost-usd`) stopped the run. A client export is
/// refused and nothing is written; an `--internal` export is written, marked incomplete.
pub const EXIT_COST_CAPPED: i32 = 4;

/// The exit code when the run stopped because the model provider could not be reached (three
/// transport failures in a row after retries, or one auth or billing failure). Every part that
/// was never sent is on the ledger as not run with class `outage_stop`, and the run can be
/// triggered again when the provider is back. A client export is refused for such a run; an
/// `--internal` export is written, marked INTERNAL INCOMPLETE. Distinct from 3 (release gate
/// refused) and 4 (spend limit).
pub const EXIT_OUTAGE_STOPPED: i32 = 5;

/// The exit code when a CLIENT export was refused, or an internal one was marked, because the
/// run did not complete for a reason other than the two above (a part failed after its retries
/// and its second try). Nothing was written for a client. Distinct from 3, 4 and 5.
pub const EXIT_INCOMPLETE_RUN: i32 = 6;

/// The exit code for an incomplete run: the outage code when an outage stopped it, the spend-limit
/// code when the limit did, else the plain incomplete code.
fn incomplete_exit_code(outage: bool, cost_capped: bool) -> i32 {
    if outage {
        EXIT_OUTAGE_STOPPED
    } else if cost_capped {
        EXIT_COST_CAPPED
    } else {
        EXIT_INCOMPLETE_RUN
    }
}

/// Why an inspection produced no export.
#[derive(Debug)]
pub enum InspectError {
    /// A pipeline or argument failure, with its plain-language message.
    Failed(String),
    /// The release gate refused the export: a secret value (or an inconsistency) was found in
    /// the package it just built. Nothing was written.
    ReleaseBlocked(camerata_server::release_gate::GateFailure),
    /// A client export was refused because the run did not complete (raised by the package
    /// assembly; the pipeline wraps it as [`InspectError::IncompleteRun`] with the run's report).
    Incomplete(camerata_server::report_export::RunIncomplete),
    /// The same refusal, with what the run did and did not do. No zip was written.
    IncompleteRun(Box<IncompleteRun>),
    /// `--resume` was given a run that is already complete: nothing to do, and it says so.
    AlreadyComplete(String),
}

/// A client export refused because the run did not complete, with the run's own account.
#[derive(Debug)]
pub struct IncompleteRun {
    /// The refusal and its reasons.
    pub refusal: camerata_server::report_export::RunIncomplete,
    /// The ledger summary (including FAILED PASSES), as for a run that exported.
    pub ledger_summary: String,
    /// Where the operator ledger was written.
    pub ledger_path: PathBuf,
    /// One structured line per failure, for stderr.
    pub failure_lines: Vec<String>,
    /// The saved run, so the operator can resume it.
    pub run_id: Option<String>,
}

impl InspectError {
    /// The process exit code for this failure.
    pub fn exit_code(&self) -> i32 {
        match self {
            InspectError::Failed(_) => 1,
            InspectError::ReleaseBlocked(_) => EXIT_RELEASE_BLOCKED,
            InspectError::Incomplete(refusal) => {
                incomplete_exit_code(refusal.outage, refusal.cost_capped)
            }
            InspectError::IncompleteRun(run) => {
                incomplete_exit_code(run.refusal.outage, run.refusal.cost_capped)
            }
            InspectError::AlreadyComplete(_) => 0,
        }
    }
}

impl std::fmt::Display for InspectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InspectError::Failed(message) => write!(f, "{message}"),
            InspectError::ReleaseBlocked(failure) => write!(f, "{failure}"),
            InspectError::Incomplete(refusal) => write!(f, "{refusal}"),
            InspectError::IncompleteRun(run) => write!(
                f,
                "{}\n\n{}\nintegrity ledger: {}",
                run.refusal,
                run.ledger_summary,
                run.ledger_path.display()
            ),
            InspectError::AlreadyComplete(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for InspectError {}

impl From<String> for InspectError {
    fn from(message: String) -> Self {
        InspectError::Failed(message)
    }
}

/// Validate the arguments BEFORE touching the filesystem for real work or resolving a
/// backend — every case here is a user-fixable mistake, never a model call, so it's cheap to
/// check eagerly with a clear message rather than surfacing a deep-stack I/O error later.
///
/// `has_api_key` is passed in (rather than this function reading the environment itself) so
/// callers — including tests — can exercise the api+key / api+no-key / batch branches
/// deterministically without mutating real process env state. [`run_inspect`] is the only
/// production caller, and it passes the presence the shared resolver finds (the key saved in
/// the app first, `ANTHROPIC_API_KEY` as the fallback; see [`api_key_present`]).
pub fn validate_args(args: &InspectArgs, has_api_key: bool) -> Result<(), String> {
    args.run_controls()?;
    require_client_lock(args)?;
    let probe = ReportOptions {
        client_name: args.client.clone(),
        ..ReportOptions::default()
    };
    camerata_server::report_export::require_client_name(&probe)
        .map_err(|e| format!("--client: {e}"))?;
    args.resolve_sources()?;
    let export_dir: &Path = match args.export.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    if !export_dir.is_dir() {
        return Err(format!(
            "--export directory {} does not exist",
            export_dir.display()
        ));
    }
    if args.batch && args.realtime {
        return Err("--batch and --realtime cannot both be given (pick one mode)".to_string());
    }
    if args.batch && !matches!(args.backend, ProjectBackend::Api) {
        return Err(
            "--batch requires --backend api (Anthropic Message Batches is an API-only feature; \
             the CLI/subscription backend has no batches endpoint)"
                .to_string(),
        );
    }
    if args.batch && !has_api_key {
        return Err(
            "--batch requires an Anthropic API key, either the one saved in the app (Settings, \
             Chat backend, API) or ANTHROPIC_API_KEY (Message Batches needs the API backend WITH \
             a configured key)"
                .to_string(),
        );
    }
    Ok(())
}

/// The client-engagement lock ([`camerata_server::client_lock::refusal`]) for this run: the
/// backend and every model the scan may use (each resolved exactly as the scan resolves it,
/// environment overrides included). Pure apart from those environment reads; runs before any
/// file is read.
pub fn require_client_lock(args: &InspectArgs) -> Result<(), String> {
    let models = camerata_server::client_lock::models_a_scan_may_use(
        args.model.as_deref(),
        args.calibration_model.as_deref(),
        false,
        args.fix_model.as_deref(),
    );
    match camerata_server::client_lock::refusal(args.client_engagement, args.backend, &models) {
        None => Ok(()),
        Some(why) => Err(format!(
            "{why} Run it with `--backend api` and Anthropic models (`--model claude-...`), or \
             pass `--internal` if this is your own development and not client work."
        )),
    }
}

/// The rule ids/directives a FRESH onboarding scan pre-checks with no human override — i.e.
/// every [`ProposedRule`] with `is_auto_recommended` set (grounded/verified rules; see that
/// field's own doc comment — `draft`/`needs_recheck` rules are listed but require an explicit
/// opt-in, so they're excluded here exactly like an untouched onboarding table would leave
/// them unchecked). This is the "curation defaults" half of the brownfield flow.
///
/// Mirrors `crates/ui/src/cockpit/rules.rs`'s `resolve_directive` closure evaluated with an
/// EMPTY "chosen" map (nothing manually picked) — precisely the state a first, human-untouched
/// scan is in. The model's OWN automatic alternative recommendation (deciding WHICH
/// alternative actually applies, e.g. by reading the codebase) still happens downstream,
/// inside `audit_repos`, whenever it is given the loaded corpus (see this module's own doc
/// comment); this function only decides which rules enter the scan at all, using each rule's
/// pre-declared `default_option` as the starting directive — exactly the directive a human who
/// changed nothing in the table would have sent.
pub fn curated_rule_selection(proposed: &[ProposedRule]) -> Vec<SelectedRule> {
    proposed
        .iter()
        .filter(|r| r.is_auto_recommended)
        .map(|r| SelectedRule {
            id: r.id.clone(),
            directive: resolve_default_directive(r),
            repos: r.repos.clone(),
        })
        .collect()
}

/// A single proposed rule's default directive: its default option's text when it has options,
/// else its title (mechanical/content rules with no alternatives carry no directive text of
/// their own). Falls back to the title if the declared `default_option` id doesn't resolve to
/// a real option or resolves to blank text — never an empty directive.
fn resolve_default_directive(r: &ProposedRule) -> String {
    if r.options.is_empty() {
        return r.title.clone();
    }
    r.default_option
        .as_ref()
        .and_then(|oid| r.options.iter().find(|o| &o.id == oid))
        .map(|o| o.directive.clone())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| r.title.clone())
}

/// Remove a pre-2026-10-18 bookkeeping file from the scanned repository, if one is there.
///
/// Only removes a file that PARSES as the thing we claim it is. The `.camerata-` prefix is
/// ours by convention, but a convention is not proof, and deleting an unreadable file from a
/// client's repository on the strength of its name is not a trade worth making. Anything that
/// does not parse is left alone and reported, so an operator sees it rather than losing it.
fn clean_legacy_repo_file<T: serde::de::DeserializeOwned>(repo: &Path, basename: &str) {
    let legacy = repo.join(basename);
    if !legacy.exists() {
        return;
    }
    let parses = std::fs::read(&legacy)
        .ok()
        .and_then(|b| serde_json::from_slice::<T>(&b).ok())
        .is_some();
    if !parses {
        eprintln!(
            "camerata inspect: {} exists in the scanned repo but is not a readable Camerata \
             {basename}; leaving it alone rather than deleting a file we cannot identify.",
            legacy.display()
        );
        return;
    }
    match std::fs::remove_file(&legacy) {
        Ok(()) => eprintln!(
            "camerata inspect: removed {} — Camerata's bookkeeping now lives in {} and is no \
             longer written into the scanned repository.",
            legacy.display(),
            camerata_state_dir().display()
        ),
        Err(e) => eprintln!(
            "camerata inspect: could not remove the legacy {} ({e}); it is harmless (the scan \
             excludes it) but can be deleted by hand.",
            legacy.display()
        ),
    }
}

/// Best-effort load: any error (missing file, corrupt JSON, a schema from a future/older
/// binary version) degrades to `None` — exactly `audit_repos`'s own "no cache" full-scan path,
/// never a hard failure over a cache file that is, by construction, disposable.
///
/// A manifest from an older schema (`ScanManifest::is_current`) is never reused AND is deleted
/// on the spot: a version 1 manifest holds findings stored before construction-time secret
/// redaction, so it can hold a raw secret value, and "ignored" is not the same as "gone"
/// (docs/design/2026-10-23_secret-redaction-and-release-gate.md, path E2).
fn load_prior_manifest(path: &Path) -> Option<ScanManifest> {
    let bytes = std::fs::read(path).ok()?;
    let manifest: ScanManifest = serde_json::from_slice(&bytes).ok()?;
    if manifest.is_current() {
        return Some(manifest);
    }
    match std::fs::remove_file(path) {
        Ok(()) => eprintln!(
            "camerata inspect: removed the incremental-scan cache at {} (written by an older              version; the next run does a full scan)",
            path.display()
        ),
        Err(e) => eprintln!(
            "camerata inspect: could not remove the outdated incremental-scan cache at {}              ({e}); it will not be used, but delete it by hand",
            path.display()
        ),
    }
    None
}

/// Best-effort save. Failing to persist the cache never fails the inspection itself — it just
/// means the NEXT run does a full scan instead of an incremental one.
fn save_manifest(path: &Path, manifest: &ScanManifest) {
    let Ok(bytes) = serde_json::to_vec_pretty(manifest) else {
        return;
    };
    // Best-effort: a failure here surfaces as the write error below, which is already
    // non-fatal (the next run does a full scan instead).
    let _ = std::fs::create_dir_all(camerata_state_dir());
    if let Err(e) = std::fs::write(path, bytes) {
        eprintln!(
            "camerata inspect: could not persist the incremental-scan cache at {}: {e} (the \
             next run will do a full scan instead)",
            path.display()
        );
    }
}

/// Best-effort save of the completed scan's pipeline-integrity ledger, at [`ledger_path`]
/// (re-exported above; beside the manifest cache, likewise disposable and never in the repo).
///
/// # Why this file exists (W6 / I2)
///
/// Before this, a `ScanLedger` lived only in memory: `render_ledger_summary` printed a
/// human-readable digest and the ledger itself was dropped with the process. So the one
/// artifact that records whether everything which was SUPPOSED to run actually ran could not
/// be re-examined, diffed against a previous scan, or checked by anything other than a person
/// reading the digest at the moment it scrolled past. `camerata integrity-gate` reads this
/// file. A ledger is a few kB of JSON; the scan that produced it is dollars and hours.
///
/// A persistence failure never fails the
/// inspection — but unlike the scan cache (whose absence costs one full re-scan) the absence
/// of this file makes `camerata integrity-gate` exit `2` (gate unavailable) rather than `0`,
/// so a silent write failure can never read as a passing gate.
fn save_ledger(path: &Path, ledger: &camerata_server::scan_ledger::ScanLedger) {
    let Ok(bytes) = serde_json::to_vec_pretty(ledger) else {
        return;
    };
    // Unlike the scan cache, a missing ledger makes `camerata integrity-gate` exit 2 rather
    // than 0, so the directory must exist before the write is attempted.
    if let Err(e) = std::fs::create_dir_all(camerata_state_dir()) {
        eprintln!(
            "camerata inspect: could not create {} ({e}); the pipeline-integrity ledger will \
             not be persisted and `camerata integrity-gate` will report itself unavailable.",
            camerata_state_dir().display()
        );
        return;
    }
    if let Err(e) = std::fs::write(path, bytes) {
        eprintln!(
            "camerata inspect: could not persist the pipeline-integrity ledger at {}: {e} \
             (`camerata integrity-gate` will report itself unavailable rather than passing)",
            path.display()
        );
    }
}

/// Build the product-export ZIP bytes for `report`, via the EXACT SAME `report_export` /
/// `xlsx_export` / zip-assembly functions the `POST /api/projects/:id/product-export` HTTP
/// handler (`export_product`) calls, in the same order, with the same "nothing manually
/// triaged yet" defaults a headless run implies: an empty client-disposition map and
/// `ReportOptions::default()` folded through `apply_env_defaults()` — exactly like that
/// handler. Pure aside from the `compile_pdf` shell-out to `typst`; no filesystem writes.
/// Returns `(filename_stem, zip_bytes)`.
///
/// `report: &SealedReport` (Pass-1 W1 seal fix) — the caller, [`run_inspect_with_key_presence`],
/// seals the report AFTER its own row-adding passes (the preview-linter merge, the dep-audit,
/// and the evidence-of-violation gate) have all already run, exactly mirroring the server's
/// `export_product` handler's own sealing point. See `ScanReport::seal`'s doc comment.
pub async fn build_product_export_bytes(
    report: &camerata_server::onboard::SealedReport,
    corpus: Option<&camerata_rules::RuleSet>,
    client: &str,
) -> Result<(String, Vec<u8>), InspectError> {
    let released = release_product_export(report, corpus, None, client, false).await?;
    Ok((released.stem, released.bytes))
}

/// [`build_product_export_bytes`] with the gate's record of what it checked kept, for the
/// operator-facing summary line.
async fn release_product_export(
    report: &camerata_server::onboard::SealedReport,
    corpus: Option<&camerata_rules::RuleSet>,
    model: Option<camerata_server::ModelRecord>,
    client: &str,
    internal: bool,
) -> Result<camerata_server::release_gate::ReleasedZip, InspectError> {
    let dispositions: HashMap<String, camerata_server::report_export::DispositionWire> =
        HashMap::new();
    let mut options = ReportOptions::default();
    options.apply_env_defaults();
    options.model = model;
    options.client_name = client.to_string();
    // `--internal` is the operator's own review: only then may an export come from a run that
    // did not complete, and it is marked INTERNAL INCOMPLETE.
    options.internal = internal;

    let package = camerata_server::release_gate::build_product_package(
        report,
        &dispositions,
        corpus,
        &options,
    )
    .await
    .map_err(|e| {
        // A client export of a run that did not complete is refused before anything is built.
        let e = match e.downcast::<camerata_server::report_export::RunIncomplete>() {
            Ok(incomplete) => return InspectError::Incomplete(incomplete),
            Err(e) => e,
        };
        match e.downcast::<camerata_server::release_gate::ReleaseError>() {
            // The assembly checks the PDF's data before it compiles it.
            Ok(camerata_server::release_gate::ReleaseError::Blocked(failure)) => {
                InspectError::ReleaseBlocked(failure)
            }
            Ok(other) => InspectError::Failed(other.to_string()),
            Err(e) => InspectError::Failed(e.to_string()),
        }
    })?;
    let released = if internal {
        camerata_server::release_gate::release_internal(package)
    } else {
        camerata_server::release_gate::release(package)
    };
    released.map_err(|e| match e {
        camerata_server::release_gate::ReleaseError::Blocked(failure) => {
            InspectError::ReleaseBlocked(failure)
        }
        other => InspectError::Failed(other.to_string()),
    })
}

/// Render the run's actual token usage + cost for the CLI's final stdout line — the number
/// the refinement loop's budget depends on (see this module's doc comment). Sourced entirely
/// from [`ScanReport::actual_usage`] (`ai_audit::ActualUsage`), the SAME per-run meter
/// `audit_repos` stamps onto every report; this function only formats it, it does no
/// accounting of its own.
pub fn format_usage_report(usage: Option<&ActualUsage>, batch: bool) -> String {
    let mode = if batch {
        "batch (Anthropic Message Batches, ~50% off real-time pricing)"
    } else {
        "real-time"
    };
    match usage {
        None => format!("Token usage: none recorded (no AI review ran this scan) — mode: {mode}"),
        Some(u) => {
            let cost = if u.cost_complete {
                format!("${:.4}", u.cost_usd)
            } else if u.calls == 0 {
                "$0.0000".to_string()
            } else {
                format!(
                    "${:.4} (PARTIAL — not every call reported a cost)",
                    u.cost_usd
                )
            };
            format!(
                "Token usage — input: {}, output: {}, cache read: {}, cache write: {}, calls: \
                 {}, cost: {cost}, mode: {mode}",
                u.input_tokens,
                u.output_tokens,
                u.cache_read_input_tokens,
                u.cache_creation_input_tokens,
                u.calls,
            )
        }
    }
}

/// The real pipeline, parameterized by API-key presence so tests never depend on (or mutate)
/// process env state — see [`validate_args`]'s doc comment for the same rationale.
/// [`run_inspect`] is the thin, production-only wrapper that resolves the key from the real
/// Keychain (env fallback) and delegates to the same pipeline.
///
/// This entry point builds NO credential store of its own: it hands the pipeline an EMPTY
/// in-memory one, so a test that injects `has_api_key` never touches the real Keychain. (The
/// shared resolver still falls back to `ANTHROPIC_API_KEY`, exactly as it does everywhere.)
pub async fn run_inspect_with_key_presence(
    args: InspectArgs,
    has_api_key: bool,
) -> Result<InspectOutcome, InspectError> {
    run_inspect_pipeline(
        args,
        has_api_key,
        &camerata_server::credentials::MemoryCredentialStore::new(),
    )
    .await
}

/// The pipeline body shared by [`run_inspect`] and [`run_inspect_with_key_presence`]. `creds`
/// is the store the audit's `Llm` reads its Anthropic key from when it is built
/// (`credentials::anthropic_api_key`), so the key the app saved is the key this run uses.
async fn run_inspect_pipeline(
    args: InspectArgs,
    has_api_key: bool,
    creds: &dyn camerata_server::credentials::CredentialStore,
) -> Result<InspectOutcome, InspectError> {
    run_inspect_pipeline_with(
        args,
        has_api_key,
        creds,
        Seams {
            resume: None,
            llm: None,
            preflight_sink: &|text| print!("{text}"),
        },
    )
    .await
}

/// The test seams of the pipeline. Production passes no model (the real `Llm` is built from the
/// credential store) and prints the pre-flight estimate to stdout the moment it is ready.
pub struct Seams<'a> {
    /// A scripted model to run the audit over instead of the real `Llm` (no network).
    pub llm: Option<&'a dyn camerata_server::llm::LlmPort>,
    /// Receives the pre-flight estimate text BEFORE the first model call is made.
    pub preflight_sink: &'a (dyn Fn(&str) + Sync),
    /// The id of a saved run to resume instead of starting a new one (see [`resume_inspect`]).
    pub resume: Option<String>,
}

/// [`run_inspect_pipeline`] with its seams explicit.
pub async fn run_inspect_pipeline_with(
    args: InspectArgs,
    has_api_key: bool,
    creds: &dyn camerata_server::credentials::CredentialStore,
    seams: Seams<'_>,
) -> Result<InspectOutcome, InspectError> {
    validate_args(&args, has_api_key)?;
    let mut controls = args.run_controls()?;

    // The `(name, folder)` sources: one per `--repo`, named by `--repo-name` (a lone repo with
    // no name keeps its folder's name). The name tags every finding and provenance entry, the
    // role `spec` plays in `resolve_local_sources`. Several repos are ONE project: one scan,
    // one report, one cache and one ledger.
    let sources = args.resolve_sources()?;
    let (manifest_path, ledger_file) = args.state_paths();
    // A multi-repo run's one cache and one ledger cover several folders; record which, so that
    // deleting any one of them can find and remove the files (see `project_files_covering`).
    // Written before anything is spent: a cache that deletion could not find must not exist.
    if args.repo_dirs().len() > 1 {
        camerata_server::scan_cache::state_dir::write_project_repos(&args.repo_dirs()).map_err(
            |e| {
                InspectError::Failed(format!(
                    "could not record which folders this run covers in {}: {e}",
                    camerata_state_dir().display()
                ))
            },
        )?;
    }

    // ── Phase 1: stack detection + rule proposal ────────────────────────────────────────
    let scan = camerata_server::onboard::scan_repos(&sources, Vec::new()).await;
    let selected = curated_rule_selection(&scan.proposed_rules);

    // ── Phase 2: load the corpus ONCE + derive the preview list ─────────────────────────
    // W4: `split_scannable_rules` no longer excludes anything on CI-tier grounds —
    // `excluded_mechanical` is always empty now (kept only for tuple/JSON back-compat). It
    // still derives `preview_rules` (the scan-time preview linter pass). `audit_repos` gets
    // the FULL curated `selected` — it needs every id both to arm the deterministic
    // architectural engine (`repo_selected_ids`) and to build its own semantic/AI prompt,
    // which now includes every selected, code-auditable rule (CI-tier or not).
    // W13: the fifth element is the corpus-load failure list this function used to discard.
    // It MUST be consumed — see `record_corpus_load_failures` below. This matters most here
    // of all three scan entry points, because `camerata inspect` is what WRITES the ledger
    // `camerata integrity-gate` later evaluates: dropping it here would leave the gate
    // evaluating a ledger from which the unloadable rules are absent.
    let (_ai_scannable_only, excluded_mechanical, preview_rules, corpus, corpus_load_failures) =
        camerata_server::split_scannable_rules(selected.clone()).await;

    // ── Backend resolution + the process-local usage ledger ────────────────────────────
    let backend_resolution = resolve_backend(args.backend, has_api_key);
    // The mode: forced by `--batch` / `--realtime`, else the run's default (rank 2: a Message
    // Batch on the API backend). The CLI backend cannot batch; that is said once, here, before
    // any model call.
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
        (seams.preflight_sink)(&format!("camerata inspect: {notice}\n"));
    }
    let mode = mode_choice.mode;
    let batch = mode == ScanMode::Batch;
    let model = args.model.as_deref().filter(|m| !m.trim().is_empty());
    let calibration_model = args
        .calibration_model
        .as_deref()
        .filter(|m| !m.trim().is_empty());
    let ledger = Arc::new(UsageLedger::new());

    // Sweep up bookkeeping files left in the scanned repo by a pre-2026-10-18 binary. Done
    // before the scan so this run's file collection never sees them, independently of
    // `files::is_engine_artifact` — belt and braces on the defect that put 174,236 characters
    // of Camerata's own output into an audit prompt.
    for dir in args.repo_dirs() {
        clean_legacy_repo_file::<ScanManifest>(&dir, ".camerata-scan-cache.json");
        clean_legacy_repo_file::<camerata_server::scan_ledger::ScanLedger>(
            &dir,
            ".camerata-scan-ledger.json",
        );
    }

    // ── The run store: a new run, or the saved run being resumed ───────────────────────────
    let audit_model_resolved = camerata_server::ai_audit::resolve_audit_model(model);
    let ai_scan_key = camerata_server::scan_cache::AiScanKey::new(
        &audit_model_resolved,
        calibration_model,
        &selected,
    );
    let models_record = camerata_server::run_store::ModelsRecord {
        audit: audit_model_resolved.clone(),
        calibration: calibration_model.unwrap_or_default().to_string(),
        fix: args.fix_model.clone().unwrap_or_default(),
        backend: format!("{:?}", args.backend).to_ascii_lowercase(),
        mode: format!("{mode:?}").to_ascii_lowercase(),
    };
    let project_key = if args.repo_dirs().len() == 1 {
        camerata_server::scan_cache::state_dir::repo_state_key(&args.repo)
    } else {
        camerata_server::scan_cache::state_dir::project_state_key(&args.repo_dirs())
    };
    let resumed: Option<(Arc<camerata_server::run_store::RunStore>, String)> = match &seams.resume {
        Some(run_id) => Some(open_for_resume(
            run_id,
            &sources,
            &ai_scan_key,
            &models_record,
        )?),
        None => None,
    };
    let incremental_prior = match &resumed {
        // The baseline the run started from, not the one an attempt since may have replaced.
        Some((store, _)) => store.load_prior_manifest(),
        None if args.full => None,
        None => load_prior_manifest(&manifest_path),
    };

    // A spend limit needs every model priced; refuse before the estimate, before any model call.
    if !matches!(backend_resolution, BackendResolution::Blocked { .. }) {
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
    }

    // ── Pre-flight: the estimate, printed BEFORE the first model call ──────────────────────
    // Skipped when no model call will be made (the compliance gate blocked the AI review).
    let mut preflight_text: Option<String> = None;
    if let Some((_, estimate_text)) = &resumed {
        // A resume prices only what remains.
        (seams.preflight_sink)(estimate_text);
        preflight_text = Some(estimate_text.clone());
    } else if !matches!(backend_resolution, BackendResolution::Blocked { .. }) {
        let facts =
            camerata_server::preflight::gather_scan_facts(&sources, &selected, corpus.as_ref())
                .await;
        // A repo that cannot be read is a repo the inspection would silently leave out. Refuse
        // before any model call, naming it, rather than price (and later run) a smaller job.
        if let Some(bad) = facts.iter().find(|f| f.read_error.is_some()) {
            return Err(InspectError::Failed(format!(
                "could not read {}: {}",
                bad.repo,
                bad.read_error.as_deref().unwrap_or("unknown error")
            )));
        }
        // The exact size of one pass: API backend only, and never for a scripted model.
        let exact = if seams.llm.is_none() && matches!(backend_resolution, BackendResolution::Api) {
            let llm = camerata_server::onboard::build_llm(
                creds,
                camerata_server::llm::Backend::Api,
                None,
            );
            camerata_server::preflight::exact_sample_tokens(
                &llm,
                &facts,
                mode,
                model,
                controls.efficiency,
            )
            .await
        } else {
            None
        };
        let audit_model = camerata_server::ai_audit::resolve_audit_model(model);
        let pf = preflight::build(&preflight::PreflightInputs {
            backend: args.backend,
            batch,
            calibration_model: calibration_model
                .map(str::to_string)
                .unwrap_or_else(|| audit_model.clone()),
            model: audit_model,
            repos: &facts,
            incremental: incremental_prior.is_some(),
            cap_usd: controls.max_cost_usd,
            exact,
            terse_output: controls.efficiency.terse_output,
            consensus: controls.consensus,
        });
        (seams.preflight_sink)(&pf.text);
        controls.estimate_hint_usd = pf.estimate_usd;
        preflight_text = Some(pf.text);
    }

    // ── Phase 3: the two-tier scan (deterministic floor + AI review), calibration, and
    // per-rule alternative recommendation (when the corpus resolves a rule's alternatives —
    // the "investigation" of which option actually applies) — ALL inside `audit_repos`, the
    // exact function `onboard_audit`'s HTTP handler calls. `chosen_options` stays empty: no
    // project-level override exists headlessly, so the model recommends fully automatically —
    // the curation-default state. ──
    // The run is saved from here on: every model reply is stored as it returns, so an
    // interruption or a failed part costs only that part (see `camerata_server::run_store`).
    let store: Option<Arc<camerata_server::run_store::RunStore>> = match resumed {
        Some((store, _)) => Some(store),
        None => {
            match camerata_server::run_store::RunStore::create(camerata_server::run_store::NewRun {
                project_key: project_key.clone(),
                repos: sources.clone(),
                models: models_record.clone(),
                ai_scan_key: ai_scan_key.clone(),
                args: serde_json::to_value(&args).unwrap_or(serde_json::Value::Null),
                estimate_usd: controls.estimate_hint_usd,
                include_alternatives: corpus.is_some(),
            }) {
                Ok(store) => {
                    if let Some(prior) = &incremental_prior {
                        store.save_prior_manifest(prior);
                    }
                    let _ = camerata_server::run_store::prune_runs(
                        &project_key,
                        camerata_server::run_store::DEFAULT_KEEP_RUNS,
                    );
                    Some(store)
                }
                Err(e) => {
                    eprintln!(
                        "camerata inspect: this run cannot be saved ({e}); it will run, but it \
                         cannot be resumed if it is interrupted or a part fails"
                    );
                    None
                }
            }
        }
    };
    // Delimiter tokens drawn from the run's own seed, so a resumed run asks the model the same
    // questions as the run it resumes and a stored reply still matches its prompt.
    let token_seed = store.as_ref().map(|s| s.nonce_seed());

    let (mut report, manifest) = {
        // The model this run asks: the scripted one a test hands in, else the real client for the
        // resolved transport (EXACTLY the resolution's Cli/Api choice, as `audit_repos_controlled`
        // builds it; every `claude -p` it spawns is remembered so source deletion can find the
        // session files).
        let sessions = camerata_server::llm::SessionSink::new();
        let llm_backend = match &backend_resolution {
            BackendResolution::Api => camerata_server::llm::Backend::Api,
            BackendResolution::Cli | BackendResolution::Blocked { .. } => {
                camerata_server::llm::Backend::Cli
            }
        };
        let real = match seams.llm {
            Some(_) => None,
            None => Some(
                camerata_server::onboard::build_llm(creds, llm_backend, Some(ledger.clone()))
                    .with_session_sink(sessions.clone()),
            ),
        };
        let base: &dyn camerata_server::llm::LlmPort = match (seams.llm, real.as_ref()) {
            (Some(llm), _) => llm,
            (None, Some(real)) => real,
            (None, None) => {
                return Err(InspectError::Failed(
                    "no model client could be built for this run".to_string(),
                ))
            }
        };
        let journaled = store
            .as_ref()
            .map(|s| camerata_server::run_store::journal::wrap(base, s.clone()));
        let model_seam: &dyn camerata_server::llm::LlmPort = match &journaled {
            Some(j) => j.as_ref(),
            None => base,
        };
        let (mut report, manifest) = camerata_server::prompt_frame::with_seed(
            token_seed,
            camerata_server::onboard::audit_repos_with_llm_controlled(
                model_seam,
                &sources,
                &selected,
                Vec::new(),
                model,
                calibration_model,
                mode,
                false, // thorough: not exposed on Step 0's flag surface; matches AuditReq's default
                None,  // feedback: no live transcript sink headlessly
                None,  // job: no job store headlessly (this call blocks until done)
                incremental_prior.as_ref(),
                false, // deep: the opt-in compliance tier; not exposed on Step 0's flag surface
                true,  // soc2_enabled: irrelevant with `deep` off
                true,  // run_ai_review
                true,  // run_deterministic
                backend_resolution,
                corpus.as_ref(),
                &HashMap::new(),
                controls.clone(),
            ),
        )
        .await;
        if seams.llm.is_none() {
            report.provenance.model_sessions = sessions.snapshot();
        }
        (report, manifest)
    };
    report.excluded_mechanical_rules = excluded_mechanical;
    // W13: mirrors `onboard_audit`/`onboard_audit_start`'s own call site in
    // `camerata_server::lib` exactly — every corpus file that would not parse is disclosed
    // into `report.ledger` (and so into the ledger `save_ledger` persists below, which is
    // what `camerata integrity-gate` reads), into one concise client-facing coverage line,
    // and into the operator log with its full path and parser message.
    camerata_server::record_corpus_load_failures(&mut report, &corpus_load_failures);

    // ── Phase 4: the SAME scan-time preview-linter + dep-audit passes `onboard_audit` runs ──
    camerata_server::merge_scan_preview(
        &mut report,
        &sources,
        &preview_rules,
        corpus.as_ref(),
        None,
        args.client_tooling(),
    )
    .await;
    // Pass-1 W1 seal fix: one ledger stage entry for this loop's net additions — mirrors
    // `onboard_audit`'s synchronous path in `camerata_server::lib` exactly (see
    // `camerata_server::scan_ledger::record_addition_stage`'s doc comment).
    // Runtime and framework end of life and known runtime vulnerabilities, before the dependency
    // pass so that the KEV / EPSS enrichment below covers these rows too.
    let runtime_rows_in = report.findings.len();
    camerata_server::runtime_audit::run_runtime_audit(&mut report, &sources).await;
    let dep_audit_rows_in = report.findings.len();
    let mut osv_runs: Vec<camerata_server::vuln_intel::OsvRun> = Vec::new();
    for (spec, dir) in &sources {
        let (dep_findings, dep_note) = camerata_server::dep_audit::run_dep_audit(spec, dir).await;
        osv_runs.push(camerata_server::vuln_intel::OsvRun::from_note(
            spec,
            dep_note.as_ref(),
        ));
        if !dep_findings.is_empty() {
            report.findings.extend(dep_findings);
        }
        if let Some(note) = dep_note {
            report.coverage_notes.push(note);
        }
    }
    // KEV / EPSS enrichment and the "vulnerability data as of" stamp: the SAME single
    // implementation `onboard_audit` calls, once per run over the rows the loop above added.
    // ORDER: before `apply_scan_severity_bounds` below, never after (see `vuln_intel::apply`).
    camerata_server::vuln_intel::enrich_dependency_findings(
        &mut report,
        runtime_rows_in,
        &osv_runs,
    )
    .await;
    camerata_server::scan_ledger::record_addition_stage(
        &mut report.ledger,
        "dependency-audit-merge",
        dep_audit_rows_in,
        report.findings.len(),
        "dep_audit_findings",
    );

    // ── Pass-1 W1 seal fix: evidence-of-violation gate — LAST row-adding pass ──────────
    // Mirrors `onboard_audit`/`onboard_audit_start`'s own call site in `camerata_server::lib`
    // exactly — see `camerata_server::report_export::apply_evidence_gate`'s doc comment. Must
    // run before the ledger summary below (so a fired gate's stage shows up in it) and before
    // `report` is sealed.
    camerata_server::report_export::apply_evidence_gate(&mut report);
    // Corpus severity bounds over the FINAL row set (preview, dep-audit and evidence-gate rows
    // are appended after `audit_repos`' own bounding) — mirrors `onboard_audit`'s call site.
    camerata_server::severity_bounds::apply_scan_severity_bounds(
        &mut report.findings,
        corpus.as_ref(),
    );

    // The incremental baseline becomes reusable only when the scan is complete: a run with a
    // failed or unrun part leaves the previous baseline in place, and its own stored replies are
    // what a resume uses.
    if report.incomplete_reasons().is_empty() {
        save_manifest(&manifest_path, &manifest);
    }
    // The scan's own tools (a build) can leave files in the checkout. Record the tree as it stands
    // now, so a run killed from here on (during the export, say) still resumes against it.
    if let Some(store) = &store {
        store.record_tree_end();
    }

    // The ledger summary is rendered from `report.ledger`/`report.failed_passes` AS THEY STAND
    // right here — after `audit_repos` (Phase 3), `merge_scan_preview` (Phase 4, which runs
    // `reconcile_external_tool_ledger` and is the LAST thing that corrects the ledger's
    // `ExternalTool` entries from speculative to real; see that function's own doc comment), the
    // dependency audit, and the evidence gate — i.e. after every row-adding pass this pipeline
    // runs. Computed before export so a PDF/typst failure below still leaves `report` fully
    // reconciled, even though the summary itself is only surfaced via the returned
    // `InspectOutcome`, never written into the export.
    let ledger_summary = render_ledger_summary(&report.ledger, &report.failed_passes, args.verbose);

    // W6 / I2: persist the ledger at the SAME point the summary is computed — fully
    // reconciled, before sealing — so `camerata integrity-gate` evaluates exactly the facts
    // this run's summary describes, never a different snapshot. See `ledger_path`.
    // The engagement this run was made under, with the models that actually ran.
    let model_record = camerata_server::ModelRecord::anthropic(args.backend)
        .with_client_engagement(args.client_engagement)
        .with_models_from(&report.provenance);
    report
        .ledger
        .record_engagement(camerata_server::client_lock::EngagementRecord::new(
            &model_record,
            args.client_tooling(),
        ));
    save_ledger(&ledger_file, &report.ledger);
    // The operator's failure report, next to the ledger (never in the export): every failed part
    // with every attempt's status, provider error, request id and waits.
    let failure_report = camerata_server::failure_report::build(&report);
    let failure_report_path =
        match camerata_server::failure_report::write(&ledger_file, &failure_report) {
            Ok(paths) => paths.map(|(json, _md)| json),
            Err(e) => {
                eprintln!("camerata inspect: could not write the failure report: {e}");
                None
            }
        };
    let failure_lines = failure_report.stderr_lines();
    let ledger_summary = match &failure_report_path {
        Some(path) => format!("{ledger_summary}\nfailure report: {}", path.display()),
        None => ledger_summary,
    };

    let run_id = store.as_ref().map(|s| s.run_id());
    let ledger_summary = match &run_id {
        Some(id) => format!(
            "{ledger_summary}\nrun: {id} (camerata inspect --resume {id} runs only what remains)"
        ),
        None => ledger_summary,
    };

    // Pass-1 W1 seal fix: THE sealing point — every row-adding pass above has already run, and
    // every exporter the rest of this function calls now takes `&SealedReport`, never
    // `&ScanReport` (see `ScanReport::seal`'s doc comment for the compile-time guarantee this
    // establishes).
    report.run_id.clone_from(&run_id);
    let report = report.seal();

    // ── Phase 5: export — the exact `export_product` HTTP handler's assembly ───────────
    // `release_product_export` is the release gate: it returns bytes only when every check
    // passed, and `std::fs::write` below runs only on `Ok`, so a blocked export is never on disk.
    let released = match release_product_export(
        &report,
        corpus.as_ref(),
        Some(model_record),
        &args.client,
        !args.client_engagement,
    )
    .await
    {
        Ok(released) => released,
        Err(e) => {
            finish_run(store.as_ref(), &report, false);
            return Err(match e {
                // The refusal still tells the operator what the run did and did not do.
                InspectError::Incomplete(refusal) => {
                    InspectError::IncompleteRun(Box::new(IncompleteRun {
                        refusal,
                        ledger_summary: ledger_summary.clone(),
                        ledger_path: ledger_file.clone(),
                        failure_lines: failure_lines.clone(),
                        run_id: run_id.clone(),
                    }))
                }
                other => other,
            });
        }
    };
    let zip_bytes = released.bytes;
    let release_gate = format!(
        "release gate: passed ({} checks)",
        released.report.checks.len()
    );
    let written = std::fs::write(&args.export, &zip_bytes);
    finish_run(store.as_ref(), &report, written.is_ok());
    written.map_err(|e| format!("could not write {}: {e}", args.export.display()))?;

    Ok(InspectOutcome {
        export_path: args.export,
        export_zip_bytes: zip_bytes.len(),
        backend: args.backend,
        batch,
        actual_usage: report.actual_usage.clone(),
        findings_count: report.findings.len(),
        ledger_summary,
        ledger_path: ledger_file,
        release_gate,
        stage_telemetry: report.ledger.stage_telemetry().cloned(),
        preflight: preflight_text,
        efficiency: controls.efficiency,
        cost_cap: report.ledger.cost_cap().cloned(),
        outage: report.ledger.outage().cloned(),
        incomplete_reasons: report.incomplete_reasons(),
        failure_lines,
        run_id,
    })
}

/// Open the saved run `run_id` to resume it (the one implementation lives in
/// [`camerata_server::run_store::open_for_resume`]; the cockpit's button uses it too).
fn open_for_resume(
    run_id: &str,
    sources: &[(String, PathBuf)],
    ai_scan_key: &camerata_server::scan_cache::AiScanKey,
    models: &camerata_server::run_store::ModelsRecord,
) -> Result<(Arc<camerata_server::run_store::RunStore>, String), InspectError> {
    camerata_server::run_store::open_for_resume(run_id, sources, ai_scan_key, models).map_err(|e| {
        match e {
            camerata_server::run_store::RunStoreError::AlreadyComplete(m) => {
                InspectError::AlreadyComplete(m)
            }
            other => InspectError::Failed(other.to_string()),
        }
    })
}

/// The end of an attempt: tell the store which parts failed and whether the run is complete.
fn finish_run(
    store: Option<&Arc<camerata_server::run_store::RunStore>>,
    report: &camerata_server::onboard::ScanReport,
    export_written: bool,
) {
    if let Some(store) = store {
        camerata_server::run_store::finish_run(store, report, export_written);
    }
}

/// Production entry point for `camerata inspect --resume`: the key and the credential store are
/// the real ones, as for [`run_inspect`].
pub async fn run_resume(
    run: &str,
    repos: &[PathBuf],
    export: Option<PathBuf>,
    max_cost_usd: Option<f64>,
) -> Result<InspectOutcome, InspectError> {
    let store = &camerata_server::credentials::KeyringCredentialStore;
    let has_api_key = api_key_present(store);
    resume_inspect(
        run,
        repos,
        export,
        max_cost_usd,
        has_api_key,
        store,
        Seams {
            resume: None,
            llm: None,
            preflight_sink: &|text| print!("{text}"),
        },
    )
    .await
}

/// Resume the saved run `run` (an id, or `latest`): run only the parts that are not done and
/// export once everything is. `args_override` carries what a resume may change: where the export
/// goes and the spend limit for the work that remains.
pub async fn resume_inspect(
    run: &str,
    repos: &[PathBuf],
    export: Option<PathBuf>,
    max_cost_usd: Option<f64>,
    has_api_key: bool,
    creds: &dyn camerata_server::credentials::CredentialStore,
    seams: Seams<'_>,
) -> Result<InspectOutcome, InspectError> {
    use camerata_server::run_store as rs;
    let project_key = match repos {
        [] => None,
        [one] => Some(camerata_server::scan_cache::state_dir::repo_state_key(one)),
        many => Some(camerata_server::scan_cache::state_dir::project_state_key(
            many,
        )),
    };
    let run_id = rs::resolve_run(run, project_key.as_deref())
        .map_err(|e| InspectError::Failed(e.to_string()))?;
    let record = rs::read_record(&run_id).map_err(|e| InspectError::Failed(e.to_string()))?;
    let mut args: InspectArgs = serde_json::from_value(record.args.clone()).map_err(|e| {
        InspectError::Failed(format!(
            "run {run_id} did not save the arguments it was started with ({e}); start a new run"
        ))
    })?;
    if let Some(export) = export {
        args.export = export;
    }
    if max_cost_usd.is_some() {
        args.max_cost_usd = max_cost_usd;
    }
    if seams.llm.is_none() {
        require_api_key_for_client_inspection(args.backend, has_api_key)?;
    }
    run_inspect_pipeline_with(
        args,
        has_api_key,
        creds,
        Seams {
            resume: Some(run_id),
            ..seams
        },
    )
    .await
}

/// Render the scan's pipeline-integrity ledger (`crate::scan_ledger::ScanLedger`) into a
/// concise, human-scannable block for `camerata inspect`'s final stdout — see this module's
/// doc comment and the ledger module's own: a scan that completes "successfully" while an
/// entire detection layer (e.g. the external-tool/taint pass) silently never ran was, before
/// this, invisible from the console. The ledger already recorded the facts; nothing printed
/// them to a human.
///
/// Pure function — deterministic given `ledger`/`failed_passes`, no I/O, no wall-clock —
/// which is what makes it unit-testable directly against hand-built ledgers (see this module's
/// `tests` below) rather than only through a full scan.
///
/// Four sections, always in this order, concise mode (`verbose == false`):
/// 1. **PIPELINE STAGES** — every stage's `rows_in -> rows_out` plus its accounted
///    dispositions; any stage with `unaccounted > 0` gets a loud `!!` marker (never silently
///    folded into a clean-looking line).
/// 2. **DETECTION FAMILIES** — one line per [`RuleTier`] (`RuleTier::all()`, so a family with
///    NO recorded rules still gets a line, not silent omission), each showing how many rules
///    ran/were skipped and how many findings it emitted, plus the most common skip reasons. A
///    family with zero rules recorded OR zero rules that actually ran is flagged with `!!` —
///    the exact "an entire detection layer never executed" shape this was built to catch.
/// 3. **NOT RUN** — every rule [`ScanLedger::excluded_rules`] confirms did not run, with its
///    real reason, so a dark layer can never hide even when every stage happens to reconcile
///    cleanly.
/// 4. **FAILED PASSES** — every `FailedPass` disclosure, printed plainly (repo, pass name,
///    reason), verbatim — never summarized or truncated.
///
/// `verbose == true` appends a fifth PER-RULE DETAIL section: one line per rule (id, ran?,
/// files evaluated, findings), sorted by rule id for stable output.
pub fn render_ledger_summary(
    ledger: &ScanLedger,
    failed_passes: &[FailedPass],
    verbose: bool,
) -> String {
    let mut out = String::new();

    writeln!(out, "== PIPELINE STAGES ==").ok();
    if ledger.stages().is_empty() {
        writeln!(out, "  (no stages recorded this scan)").ok();
    } else {
        for stage in ledger.stages() {
            let marker = if stage.unaccounted > 0 {
                "  !! UNACCOUNTED"
            } else {
                ""
            };
            writeln!(
                out,
                "  {:<28} rows {:>5} -> {:<5}  merged={:<3} held={:<3} informational={:<3} \
                 deduped={:<3} unaccounted={}{}",
                stage.stage,
                stage.rows_in,
                stage.rows_out,
                stage.merged_into.len(),
                stage.routed_held,
                stage.routed_informational,
                stage.deduped,
                stage.unaccounted,
                marker,
            )
            .ok();
        }
    }

    writeln!(out, "\n== DETECTION FAMILIES ==").ok();
    for tier in RuleTier::all() {
        let rules: Vec<&RuleLedgerEntry> =
            ledger.rules_merged().filter(|r| r.tier == tier).collect();
        let ran = rules.iter().filter(|r| r.ran).count();
        let skipped = rules.len() - ran;
        let findings: usize = rules.iter().map(|r| r.findings_emitted).sum();

        // W13: `RuleTier::Unloaded` is the ONE member of this enum that is not an engine — it
        // holds rules whose own corpus file would not parse (see its doc comment). For it, and
        // only for it, ZERO recorded rules is the GOOD state, so the `!!` alarm polarity is
        // inverted: an empty bucket is a positive disclosure ("the rule library loaded
        // cleanly") and a NON-empty one is the alarm. Printing "this family never ran at all"
        // here would be the gate-cries-wolf failure mode on every single correct scan.
        if tier == RuleTier::Unloaded {
            if rules.is_empty() {
                writeln!(
                    out,
                    "  {:<40} none — the rule library loaded cleanly",
                    tier.family_label(),
                )
                .ok();
            } else {
                writeln!(
                    out,
                    "  {:<40} !! {} rule file(s) FAILED TO PARSE — those rules did not enter \
                     this scan at all; see NOT RUN below and the operator log for each path",
                    tier.family_label(),
                    rules.len(),
                )
                .ok();
            }
            continue;
        }
        if rules.is_empty() {
            writeln!(
                out,
                "  {:<40} !! ZERO RULES RECORDED — this family never ran at all this scan",
                tier.family_label(),
            )
            .ok();
            continue;
        }
        if ran == 0 {
            writeln!(
                out,
                "  {:<40} !! ran=0 skipped={skipped} findings={findings} — NO rule in this \
                 family ran this scan",
                tier.family_label(),
            )
            .ok();
        } else {
            writeln!(
                out,
                "  {:<40} ran={ran} skipped={skipped} findings={findings}",
                tier.family_label(),
            )
            .ok();
        }
        if skipped > 0 {
            let mut reason_counts: HashMap<&str, usize> = HashMap::new();
            for r in rules.iter().filter(|r| !r.ran) {
                *reason_counts
                    .entry(r.skip_reason.as_deref().unwrap_or("(no reason given)"))
                    .or_insert(0) += 1;
            }
            let mut reasons: Vec<(&str, usize)> = reason_counts.into_iter().collect();
            reasons.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
            for (reason, count) in reasons.into_iter().take(3) {
                writeln!(out, "      skipped x{count}: {reason}").ok();
            }
        }
    }

    let excluded = ledger.excluded_rules();
    writeln!(out, "\n== NOT RUN ({}) ==", excluded.len()).ok();
    if excluded.is_empty() {
        writeln!(out, "  (every selected rule ran this scan)").ok();
    } else {
        for (id, reason) in &excluded {
            writeln!(out, "  {id:<40} {reason}").ok();
        }
    }

    writeln!(out, "\n== FAILED PASSES ({}) ==", failed_passes.len()).ok();
    if failed_passes.is_empty() {
        writeln!(out, "  (none)").ok();
    } else {
        for fp in failed_passes {
            writeln!(out, "  [{}] {}: {}", fp.repo, fp.pass, fp.reason).ok();
        }
    }

    if verbose {
        // One line per (rule, repo): a rule that ran on one repo and not on another is two
        // lines, each naming its repo.
        let mut rules: Vec<&RuleLedgerEntry> = ledger.rules().collect();
        rules.sort_by(|a, b| (&a.rule_id, &a.repo).cmp(&(&b.rule_id, &b.repo)));
        writeln!(out, "\n== PER-RULE DETAIL ({}) ==", rules.len()).ok();
        for r in rules {
            let repo = if r.repo.is_empty() {
                String::new()
            } else {
                format!(" [{}]", r.repo)
            };
            writeln!(
                out,
                "  {:<40} ran={:<5} files_evaluated={:<6} findings={}{repo}",
                r.rule_id, r.ran, r.files_evaluated, r.findings_emitted,
            )
            .ok();
        }
    }

    out.trim_end().to_string()
}

/// The production-only key requirement: a client inspection on the `api` backend with no key
/// FAILS, naming the fix. It never degrades to a floor-only run or to the CLI backend (an
/// explicit `--backend cli` is the only way onto the subscription). Pure, so it is testable
/// without mutating process env. It lives here, not in [`validate_args`], because the
/// key-presence-injecting entry point deliberately runs `api` without a key to exercise the
/// deterministic floor with zero API spend.
pub fn require_api_key_for_client_inspection(
    backend: ProjectBackend,
    has_api_key: bool,
) -> Result<(), InspectError> {
    // The decision is the app's own `resolve_backend` (one rule for the CLI and the app); only
    // the wording is CLI-specific.
    if matches!(
        resolve_backend(backend, has_api_key),
        BackendResolution::Blocked { .. }
    ) {
        return Err(InspectError::Failed(
            "client inspections run on the Anthropic API, but no API key is configured. Add an \
             Anthropic API key in the app (Settings, Chat backend, API) or set ANTHROPIC_API_KEY, \
             and re-run. For your own development only (never client work), choose the Claude \
             Code subscription explicitly with `--internal --backend cli`."
                .to_string(),
        ));
    }
    Ok(())
}

/// Whether an Anthropic API key is available to this run: exactly whether the shared resolver
/// ([`camerata_server::credentials::anthropic_api_key`], the one the app and every model call
/// use) finds one in `store`, with `ANTHROPIC_API_KEY` as the fallback. The single place the
/// inspect path decides key presence, so `camerata inspect` uses the key saved in the app.
pub fn api_key_present(store: &dyn camerata_server::credentials::CredentialStore) -> bool {
    camerata_server::credentials::anthropic_api_key(store).is_some()
}

/// Production entry point: resolves the Anthropic key from the REAL Keychain (the one the app's
/// Settings writes), with `ANTHROPIC_API_KEY` as the fallback, then runs
/// [`run_inspect_with_store`].
pub async fn run_inspect(args: InspectArgs) -> Result<InspectOutcome, InspectError> {
    run_inspect_with_store(args, &camerata_server::credentials::KeyringCredentialStore).await
}

/// [`run_inspect`] with the credential store injected (tests pass an in-memory one so nothing
/// touches the real Keychain). Requires a key for the `api` backend
/// ([`require_api_key_for_client_inspection`]) and runs the pipeline with this same store, so
/// the audit's `Llm` is built with the key the check just found. See
/// [`run_inspect_with_key_presence`], and [`validate_args`]'s doc comment, for why the key
/// check is a separate, injectable parameter.
pub async fn run_inspect_with_store(
    args: InspectArgs,
    store: &dyn camerata_server::credentials::CredentialStore,
) -> Result<InspectOutcome, InspectError> {
    let has_api_key = api_key_present(store);
    require_api_key_for_client_inspection(args.backend, has_api_key)?;
    run_inspect_pipeline(args, has_api_key, store).await
}

/// Format the final report block `main.rs` prints after a successful run.
pub fn format_outcome_report(outcome: &InspectOutcome) -> String {
    let backend_label = match outcome.backend {
        ProjectBackend::Cli => "cli",
        ProjectBackend::Api => "api",
    };
    let telemetry = outcome
        .stage_telemetry
        .as_ref()
        .map(|t| format!("\n{}", t.render_table()))
        .unwrap_or_default();
    let cap = outcome
        .cost_cap
        .as_ref()
        .map(render_cost_cap)
        .unwrap_or_default()
        + &outcome
            .outage
            .as_ref()
            .map(render_outage)
            .unwrap_or_default();
    format!(
        "camerata inspect: wrote {} ({} bytes) — backend: {backend_label}, findings: {}\n\
         {}\nintegrity ledger: {}\n{}\nefficiency measures: {}\n{cap}\n{}{telemetry}",
        outcome.export_path.display(),
        outcome.export_zip_bytes,
        outcome.findings_count,
        outcome.release_gate,
        outcome.ledger_path.display(),
        format_usage_report(outcome.actual_usage.as_ref(), outcome.batch),
        outcome.efficiency.render(),
        outcome.ledger_summary,
    )
}

/// The outage block of the final report: what stopped the run and how much was never sent.
fn render_outage(outage: &camerata_server::run_health::OutageRecord) -> String {
    format!(
        "RUN STOPPED: {} ({}). This inspection is INCOMPLETE: {} scan pass(es) and {} stage(s) \
         were never sent (listed under FAILED PASSES as NOT RUN). Run it again when the provider \
         is reachable.\n",
        outage.trigger.detail,
        outage.trigger.class.as_str(),
        outage.not_run_passes.len(),
        outage.not_run_stages.len()
    )
}

/// The spend-limit block of the final report: what was set, what was spent, and, when the limit
/// stopped the run, that the export is incomplete and what was not run. Operator-only (dollar
/// figures live here, never in the export).
fn render_cost_cap(cap: &camerata_server::cost_cap::CostCapRecord) -> String {
    let mut out = format!(
        "spend limit: ${:.2}, spent ${:.2}",
        cap.cap_usd, cap.spent_usd
    );
    if cap.stopped_the_run() {
        write!(
            out,
            " -- REACHED. This inspection is INCOMPLETE: {} scan pass(es) and {} stage(s) were NOT RUN \
             (listed under FAILED PASSES). Passes already started finish, so a run can end about one pass \
             over the limit.",
            cap.not_run_passes.len(),
            cap.not_run_stages.len()
        )
        .ok();
    } else {
        out.push_str(" (not reached)");
    }
    for note in &cap.notes {
        write!(out, "\n  {note}").ok();
    }
    out.push('\n');
    out
}

#[cfg(test)]
mod redaction_tests;

#[cfg(test)]
mod exit_code_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use camerata_server::scan_ledger::StageAccounting;

    fn base_args(repo: PathBuf, export: PathBuf) -> InspectArgs {
        InspectArgs {
            repo,
            also_repos: Vec::new(),
            repo_names: Vec::new(),
            export,
            backend: ProjectBackend::Cli,
            batch: false,
            realtime: false,
            model: None,
            calibration_model: None,
            fix_model: None,
            full: false,
            verbose: false,
            max_cost_usd: None,
            efficiency: Vec::new(),
            client: "Example Client Ltd".to_string(),
            // The maintainer's own run: these tests exercise the `cli` backend's validation.
            client_engagement: false,
            run_client_tooling: false,
            consensus: false,
        }
    }

    // ── run controls: --efficiency and --max-cost-usd ───────────────────────────────

    #[test]
    fn efficiency_flags_switch_exactly_the_named_measures() {
        let dir = tempfile::tempdir().unwrap();
        let mut args = base_args(dir.path().to_path_buf(), dir.path().join("o.zip"));
        args.efficiency = vec![
            "warm_schedule=off".to_string(),
            "truncation_retry=off".to_string(),
            "warm_schedule=on".to_string(),
        ];
        let controls = args.run_controls().unwrap();
        assert_eq!(controls.efficiency.disabled(), vec!["truncation_retry"]);
        assert_eq!(controls.max_cost_usd, None);
    }

    #[test]
    fn an_unknown_measure_or_a_bad_cap_is_refused_before_anything_runs() {
        let dir = tempfile::tempdir().unwrap();
        let mut args = base_args(dir.path().to_path_buf(), dir.path().join("o.zip"));
        args.efficiency = vec!["warm_shedule=off".to_string()];
        let err = validate_args(&args, false).expect_err("a typo must not silently leave it on");
        assert!(
            err.contains("--efficiency") && err.contains("warm_schedule"),
            "{err}"
        );

        args.efficiency.clear();
        for bad in [0.0, -3.0, f64::NAN] {
            args.max_cost_usd = Some(bad);
            let err = validate_args(&args, false).expect_err("a non-positive cap");
            assert!(err.contains("--max-cost-usd"), "{err}");
        }
        args.max_cost_usd = Some(12.5);
        validate_args(&args, false).unwrap();
        assert_eq!(args.run_controls().unwrap().max_cost_usd, Some(12.5));
    }

    #[test]
    fn the_fix_model_flag_reaches_the_run_controls() {
        let dir = tempfile::tempdir().unwrap();
        let mut args = base_args(dir.path().to_path_buf(), dir.path().join("o.zip"));
        assert_eq!(args.run_controls().unwrap().fix_model, None);
        args.fix_model = Some("claude-opus-5".to_string());
        let controls = args.run_controls().unwrap();
        assert_eq!(controls.fix_model.as_deref(), Some("claude-opus-5"));
        assert_eq!(
            controls.fix_model_for(Some("claude-sonnet-5"), None),
            Some("claude-opus-5"),
            "an explicit fix model wins over the switch"
        );
    }

    #[test]
    fn the_final_report_states_the_efficiency_set_and_the_spend_limit() {
        use camerata_server::cost_cap::{CostCapRecord, NotRunPass};
        let mut efficiency = camerata_server::efficiency::EfficiencyConfig::default();
        efficiency.set("result_cache", false).unwrap();
        let mut outcome = InspectOutcome {
            run_id: None,
            export_path: PathBuf::from("/tmp/out.zip"),
            export_zip_bytes: 1,
            backend: ProjectBackend::Api,
            batch: false,
            actual_usage: None,
            findings_count: 0,
            ledger_summary: String::new(),
            ledger_path: PathBuf::from("/tmp/l.json"),
            release_gate: "release gate: passed (6 checks)".to_string(),
            stage_telemetry: None,
            preflight: None,
            efficiency,
            cost_cap: Some(CostCapRecord {
                repos_not_finished: Vec::new(),
                cap_usd: 5.0,
                spent_usd: 5.4,
                not_run_passes: vec![NotRunPass {
                    files: Vec::new(),
                    repo: "r".into(),
                    group: "pass[all]".into(),
                    chunk: 1,
                    n_chunks: 1,
                    rule_batch: 2,
                    n_rule_batches: 2,
                    rule_ids: vec!["A-1".into()],
                    class: camerata_server::cost_cap::FailureClass::SpendLimit,
                }],
                not_run_stages: Vec::new(),
                notes: Vec::new(),
            }),
            outage: None,
            incomplete_reasons: Vec::new(),
            failure_lines: Vec::new(),
        };
        let text = format_outcome_report(&outcome);
        assert!(
            text.contains("result_cache=off") && text.contains("warm_schedule=on"),
            "{text}"
        );
        assert!(
            text.contains("spend limit: $5.00, spent $5.40 -- REACHED"),
            "{text}"
        );
        assert!(
            text.contains("INCOMPLETE") && text.contains("NOT RUN"),
            "{text}"
        );
        assert!(outcome.cost_capped());

        outcome.cost_cap.as_mut().unwrap().not_run_passes.clear();
        assert!(
            !outcome.cost_capped(),
            "a cap that stopped nothing is not a capped run"
        );
        assert!(format_outcome_report(&outcome).contains("(not reached)"));
    }

    // ── validate_args ────────────────────────────────────────────────────────────────

    #[test]
    fn validate_args_rejects_a_missing_repo_directory() {
        let dir = tempfile::tempdir().unwrap();
        let args = base_args(dir.path().join("does-not-exist"), dir.path().to_path_buf());
        let err = validate_args(&args, false).expect_err("must reject a missing --repo");
        assert!(err.contains("does not exist"), "{err}");
        assert!(err.contains("--repo"), "{err}");
    }

    #[test]
    fn validate_args_rejects_a_repo_path_that_is_a_file_not_a_directory() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("not-a-dir.txt");
        std::fs::write(&file_path, "x").unwrap();
        let args = base_args(file_path, dir.path().to_path_buf());
        assert!(validate_args(&args, false).is_err());
    }

    #[test]
    fn validate_args_rejects_a_missing_export_directory() {
        let dir = tempfile::tempdir().unwrap();
        let args = base_args(
            dir.path().to_path_buf(),
            dir.path().join("nope").join("out.zip"),
        );
        let err = validate_args(&args, false).expect_err("must reject a missing --export dir");
        assert!(err.contains("--export directory"), "{err}");
    }

    #[test]
    fn validate_args_accepts_an_export_path_with_no_directory_component() {
        // `--export out.zip` (relative, no leading dir) must resolve against ".", not error.
        let dir = tempfile::tempdir().unwrap();
        let args = base_args(dir.path().to_path_buf(), PathBuf::from("out.zip"));
        assert!(validate_args(&args, false).is_ok());
    }

    // ── M11: several repos, one project ───────────────────────────────────────────────

    /// Two folders, each with a `src/index.ts` (the same path in both repos).
    fn two_repos() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let a = root.path().join("api");
        let b = root.path().join("web");
        for dir in [&a, &b] {
            std::fs::create_dir_all(dir.join("src")).unwrap();
            std::fs::write(dir.join("src/index.ts"), "export const x = 1;\n").unwrap();
        }
        (root, a, b)
    }

    fn multi(a: PathBuf, b: PathBuf, names: &[&str], export: PathBuf) -> InspectArgs {
        let mut args = base_args(a, export);
        args.also_repos = vec![b];
        args.repo_names = names.iter().map(|n| n.to_string()).collect();
        args
    }

    #[test]
    fn a_lone_repo_without_a_name_is_still_named_for_its_folder() {
        let (root, a, _) = two_repos();
        let args = base_args(a.clone(), root.path().join("o.zip"));
        assert_eq!(
            args.resolve_sources().unwrap(),
            vec![("api".to_string(), a)]
        );
        validate_args(&args, false).unwrap();
    }

    #[test]
    fn several_repos_resolve_to_one_named_source_each() {
        let (root, a, b) = two_repos();
        let args = multi(
            a.clone(),
            b.clone(),
            &["acme/api", "acme/web"],
            root.path().join("o.zip"),
        );
        assert_eq!(
            args.resolve_sources().unwrap(),
            vec![("acme/api".to_string(), a), ("acme/web".to_string(), b)]
        );
        validate_args(&args, false).unwrap();
    }

    #[test]
    fn several_repos_without_names_are_refused_with_the_fix() {
        let (root, a, b) = two_repos();
        let args = multi(a, b, &[], root.path().join("o.zip"));
        let err = validate_args(&args, false).expect_err("a folder name is not a repo name");
        assert!(err.contains("--repo-name OWNER/REPO"), "{err}");
    }

    #[test]
    fn a_name_count_that_does_not_match_the_repo_count_is_refused() {
        let (root, a, b) = two_repos();
        let args = multi(a, b, &["acme/api"], root.path().join("o.zip"));
        let err = validate_args(&args, false).expect_err("one name for two repos");
        assert!(err.contains("one name per repo"), "{err}");
    }

    /// The names go through `validate_repo_name`: a folder-style name, a name with a space and a
    /// path-escaping name are all refused, naming the flag.
    #[test]
    fn repo_names_are_validated_as_owner_slash_repo() {
        let (root, a, b) = two_repos();
        for bad in ["api", "acme/we b", "acme/../x", "acme/web/extra", ""] {
            let args = multi(
                a.clone(),
                b.clone(),
                &["acme/api", bad],
                root.path().join("o.zip"),
            );
            let err = validate_args(&args, false).expect_err(bad);
            assert!(err.contains("--repo-name"), "{bad}: {err}");
        }
    }

    #[test]
    fn two_repos_may_not_share_a_name_or_a_folder() {
        let (root, a, b) = two_repos();
        let same_name = multi(
            a.clone(),
            b,
            &["acme/api", "acme/api"],
            root.path().join("o.zip"),
        );
        let err = same_name.resolve_sources().expect_err("same name");
        assert!(err.contains("needs its own name"), "{err}");
        let same_dir = multi(
            a.clone(),
            a,
            &["acme/api", "acme/web"],
            root.path().join("o.zip"),
        );
        let err = same_dir.resolve_sources().expect_err("same folder");
        assert!(err.contains("more than once"), "{err}");
    }

    #[test]
    fn a_missing_second_repo_is_named() {
        let (root, a, b) = two_repos();
        let gone = b.join("nope");
        let args = multi(
            a,
            gone.clone(),
            &["acme/api", "acme/web"],
            root.path().join("o.zip"),
        );
        let err = validate_args(&args, false).expect_err("missing second repo");
        assert!(err.contains(&gone.display().to_string()), "{err}");
    }

    /// One project has ONE cache and ONE ledger, whatever order the repos were named in, and
    /// they are not the files of either repo alone.
    ///
    /// Each path read resolves the state dir from process environment, which other tests in
    /// this binary mutate, so it takes the `process_env` key like
    /// `bookkeeping_is_never_written_into_the_scanned_repo` (it flaked once in a full run).
    #[test]
    #[serial_test::serial(process_env)]
    fn several_repos_share_one_cache_and_one_ledger() {
        let (root, a, b) = two_repos();
        let ab = multi(
            a.clone(),
            b.clone(),
            &["acme/api", "acme/web"],
            root.path().join("o.zip"),
        );
        let ba = multi(
            b.clone(),
            a.clone(),
            &["acme/web", "acme/api"],
            root.path().join("o.zip"),
        );
        assert_eq!(ab.state_paths(), ba.state_paths());
        let (cache, ledger) = ab.state_paths();
        assert_ne!(cache, ledger);
        let alone_a = base_args(a, root.path().join("o.zip")).state_paths();
        let alone_b = base_args(b, root.path().join("o.zip")).state_paths();
        assert_ne!(cache, alone_a.0);
        assert_ne!(cache, alone_b.0);
        // A single repo keeps the key it always had.
        assert_eq!(alone_a.0, manifest_cache_path(&ab.repo));
        assert_eq!(alone_a.1, ledger_path(&ab.repo));
    }

    #[test]
    fn validate_args_rejects_batch_on_the_cli_backend() {
        let dir = tempfile::tempdir().unwrap();
        let mut args = base_args(dir.path().to_path_buf(), dir.path().to_path_buf());
        args.batch = true;
        args.backend = ProjectBackend::Cli;
        let err = validate_args(&args, true).expect_err("batch must require the api backend");
        assert!(err.contains("--batch requires --backend api"), "{err}");
    }

    #[test]
    fn validate_args_rejects_forcing_batch_and_realtime_together() {
        let dir = tempfile::tempdir().unwrap();
        let mut args = base_args(dir.path().to_path_buf(), dir.path().to_path_buf());
        args.backend = ProjectBackend::Api;
        args.batch = true;
        args.realtime = true;
        let err = validate_args(&args, true).expect_err("one mode at a time");
        assert!(err.contains("--batch and --realtime"), "{err}");
        args.batch = false;
        assert!(
            validate_args(&args, true).is_ok(),
            "--realtime alone is fine"
        );
    }

    #[test]
    fn validate_args_rejects_batch_on_api_without_a_key() {
        let dir = tempfile::tempdir().unwrap();
        let mut args = base_args(dir.path().to_path_buf(), dir.path().to_path_buf());
        args.batch = true;
        args.backend = ProjectBackend::Api;
        let err = validate_args(&args, false).expect_err("batch must require a key");
        assert!(err.contains("ANTHROPIC_API_KEY"), "{err}");
    }

    #[test]
    fn validate_args_accepts_batch_on_api_with_a_key() {
        let dir = tempfile::tempdir().unwrap();
        let mut args = base_args(dir.path().to_path_buf(), dir.path().to_path_buf());
        args.batch = true;
        args.backend = ProjectBackend::Api;
        assert!(validate_args(&args, true).is_ok());
    }

    #[test]
    fn validate_args_accepts_a_healthy_cli_backend_request() {
        let dir = tempfile::tempdir().unwrap();
        let args = base_args(dir.path().to_path_buf(), dir.path().to_path_buf());
        assert!(validate_args(&args, false).is_ok());
    }

    /// Confidentiality: a blank or multi-line `--client` is refused before anything is read or
    /// spent, so a run never pays for an inspection whose export would then be refused.
    #[test]
    fn validate_args_rejects_a_blank_or_multi_line_client() {
        let dir = tempfile::tempdir().unwrap();
        for bad in ["", "   ", "Example\nClient"] {
            let mut args = base_args(dir.path().to_path_buf(), dir.path().to_path_buf());
            args.client = bad.to_string();
            let err = validate_args(&args, false).expect_err("a bad --client is refused");
            assert!(err.starts_with("--client: "), "{err}");
        }
    }

    // ── the shared key resolver (the app-saved key is the key the CLI uses) ───────────

    fn memory_store_with(key: Option<&str>) -> camerata_server::credentials::MemoryCredentialStore {
        use camerata_server::credentials::CredentialStore;
        let store = camerata_server::credentials::MemoryCredentialStore::new();
        if let Some(k) = key {
            store
                .set(camerata_server::credentials::ANTHROPIC_API_KEY, k)
                .unwrap();
        }
        store
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn a_key_saved_in_the_app_store_satisfies_the_inspect_gate_with_no_env() {
        let _env = camerata_test_env::EnvGuard::remove("ANTHROPIC_API_KEY");
        let store = memory_store_with(Some("sk-ant-saved-in-app"));
        assert!(api_key_present(&store));
        require_api_key_for_client_inspection(ProjectBackend::Api, api_key_present(&store))
            .expect("a key in the store must pass the client-inspection gate with env unset");
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn an_empty_or_blank_store_and_no_env_fails_the_inspect_gate_naming_both_fixes() {
        let _env = camerata_test_env::EnvGuard::remove("ANTHROPIC_API_KEY");
        for store in [memory_store_with(None), memory_store_with(Some("  "))] {
            assert!(!api_key_present(&store));
            let err =
                require_api_key_for_client_inspection(ProjectBackend::Api, api_key_present(&store))
                    .expect_err("no key anywhere must fail");
            let text = err.to_string();
            assert!(text.contains("in the app"), "{text}");
            assert!(text.contains("ANTHROPIC_API_KEY"), "{text}");
            assert!(
                !text.contains('\u{2014}'),
                "no em-dash in user-facing text: {text}"
            );
        }
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn the_env_var_is_only_the_fallback_for_the_inspect_gate() {
        let _env = camerata_test_env::EnvGuard::set("ANTHROPIC_API_KEY", "sk-ant-env-only");
        assert!(api_key_present(&memory_store_with(None)));
    }

    /// `run_inspect_with_store` is the production path with the store injected: it must gate on
    /// the STORE's key (no env), failing with the key message when absent and getting past the
    /// key check (to argument validation) when a key was saved in the app.
    #[tokio::test]
    #[serial_test::serial(process_env)]
    async fn run_inspect_with_store_uses_the_store_key_not_env() {
        let _env = camerata_test_env::EnvGuard::remove("ANTHROPIC_API_KEY");
        let dir = tempfile::tempdir().unwrap();
        // A --repo that does not exist: validation fails AFTER the key check passes.
        let mut args = base_args(dir.path().join("nope"), dir.path().to_path_buf());
        args.backend = ProjectBackend::Api;

        let err = run_inspect_with_store(args.clone(), &memory_store_with(None))
            .await
            .expect_err("no key must fail");
        assert!(
            err.to_string().contains("no API key is configured"),
            "{err}"
        );

        let err = run_inspect_with_store(args, &memory_store_with(Some("sk-ant-saved")))
            .await
            .expect_err("the bogus --repo must still fail");
        let text = err.to_string();
        assert!(
            text.contains("--repo"),
            "a store key must get past the key gate to argument validation: {text}"
        );
    }

    // ── require_api_key_for_client_inspection ────────────────────────────────────────

    #[test]
    fn api_without_a_key_fails_clearly_and_never_falls_back_to_cli() {
        let err = require_api_key_for_client_inspection(ProjectBackend::Api, false)
            .expect_err("api with no key must fail");
        let text = err.to_string();
        assert!(text.contains("Anthropic API key"), "{text}");
        assert!(
            text.contains("--backend cli"),
            "names the explicit escape: {text}"
        );
    }

    #[test]
    fn api_with_a_key_and_an_explicit_cli_choice_both_pass() {
        assert!(require_api_key_for_client_inspection(ProjectBackend::Api, true).is_ok());
        assert!(require_api_key_for_client_inspection(ProjectBackend::Cli, false).is_ok());
        assert!(require_api_key_for_client_inspection(ProjectBackend::Cli, true).is_ok());
    }

    // ── curated_rule_selection ───────────────────────────────────────────────────────

    fn opt(id: &str, directive: &str) -> camerata_server::onboard::RuleOptionView {
        camerata_server::onboard::RuleOptionView {
            id: id.to_string(),
            label: id.to_string(),
            directive: directive.to_string(),
            why: String::new(),
            violates: false,
        }
    }

    fn proposed(id: &str, is_auto_recommended: bool) -> ProposedRule {
        ProposedRule {
            id: id.to_string(),
            title: format!("Title for {id}"),
            kind: "review".to_string(),
            enforcement: "structured".to_string(),
            options: Vec::new(),
            default_option: None,
            verification: "grounded".to_string(),
            sources: Vec::new(),
            decision_question: None,
            decision_why: None,
            scope: "repo-local".to_string(),
            domain: "architecture".to_string(),
            enforcement_point: "content".to_string(),
            repos: Vec::new(),
            placement: "content".to_string(),
            finding_count: 0,
            recommended: is_auto_recommended,
            is_auto_recommended,
        }
    }

    #[test]
    fn curated_rule_selection_keeps_only_auto_recommended_rules() {
        let proposed_rules = vec![proposed("RULE-A", true), proposed("RULE-B", false)];
        let selected = curated_rule_selection(&proposed_rules);
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].id, "RULE-A");
    }

    #[test]
    fn curated_rule_selection_uses_the_default_options_directive() {
        let mut rule = proposed("RULE-A", true);
        rule.options = vec![opt("a", "Do it the A way"), opt("b", "Do it the B way")];
        rule.default_option = Some("b".to_string());
        let selected = curated_rule_selection(&[rule]);
        assert_eq!(selected[0].directive, "Do it the B way");
    }

    #[test]
    fn curated_rule_selection_falls_back_to_the_title_with_no_options() {
        let rule = proposed("RULE-A", true);
        let selected = curated_rule_selection(&[rule]);
        assert_eq!(selected[0].directive, "Title for RULE-A");
    }

    #[test]
    fn curated_rule_selection_falls_back_to_the_title_when_default_option_is_unresolvable() {
        let mut rule = proposed("RULE-A", true);
        rule.options = vec![opt("a", "Do it the A way")];
        rule.default_option = Some("missing-id".to_string());
        let selected = curated_rule_selection(&[rule]);
        assert_eq!(selected[0].directive, "Title for RULE-A");
    }

    // ── format_usage_report / format_outcome_report ─────────────────────────────────

    fn synthetic_usage() -> ActualUsage {
        ActualUsage {
            input_tokens: 123_456,
            output_tokens: 7_890,
            cost_usd: 2.5,
            calls: 4,
            cost_complete: true,
            cache_read_input_tokens: 50_000,
            cache_creation_input_tokens: 1_000,
        }
    }

    #[test]
    fn format_usage_report_shows_input_output_cache_and_cost() {
        let usage = synthetic_usage();
        let text = format_usage_report(Some(&usage), false);
        assert!(text.contains("input: 123456"), "{text}");
        assert!(text.contains("output: 7890"), "{text}");
        assert!(text.contains("cache read: 50000"), "{text}");
        assert!(text.contains("cache write: 1000"), "{text}");
        assert!(text.contains("calls: 4"), "{text}");
        assert!(text.contains("$2.5000"), "{text}");
        assert!(text.contains("mode: real-time"), "{text}");
    }

    #[test]
    fn format_usage_report_labels_batch_mode() {
        let usage = synthetic_usage();
        let text = format_usage_report(Some(&usage), true);
        assert!(text.to_lowercase().contains("batch"), "{text}");
    }

    #[test]
    fn format_usage_report_flags_a_partial_cost_total() {
        let mut usage = synthetic_usage();
        usage.cost_complete = false;
        let text = format_usage_report(Some(&usage), false);
        assert!(text.contains("PARTIAL"), "{text}");
    }

    #[test]
    fn format_usage_report_handles_no_ai_review_having_run() {
        let text = format_usage_report(None, false);
        assert!(text.contains("none recorded"), "{text}");
    }

    /// W20 (second half) — Camerata's bookkeeping never lands in the client's repository.
    ///
    /// The first half stopped the engine READING its own cache back as source, which is what
    /// put 174,236 characters of prior output into a hearth audit prompt. This is the half
    /// that stops it being written there at all, so a client running an audit does not find
    /// two unexplained vendor-named files in their `git status` afterwards.
    ///
    /// The state dir is process environment (`CAMERATA_STATE_DIR`, `HOME`, `XDG_CACHE_HOME`),
    /// and the `redaction_tests` in this same binary mutate `HOME`. The test therefore takes the
    /// workspace `process_env` key and pins the state dir with an `EnvGuard`, so the three
    /// `state_dir::dir()` reads below can never straddle another test's mutation (it flaked
    /// once in a full run, ~5% of isolated loops, before this).
    #[test]
    #[serial_test::serial(process_env)]
    fn bookkeeping_is_never_written_into_the_scanned_repo() {
        let state = tempfile::tempdir().expect("state dir");
        let _state = camerata_test_env::EnvGuard::set(
            camerata_server::scan_cache::state_dir::STATE_DIR_ENV,
            state.path(),
        );
        let repo = PathBuf::from("/Users/auditor/clients/acme-platform");
        for path in [manifest_cache_path(&repo), ledger_path(&repo)] {
            assert!(
                !path.starts_with(&repo),
                "{} is inside the scanned repo; that is the defect this test exists for",
                path.display()
            );
            assert!(
                path.starts_with(camerata_state_dir()),
                "{} should live under the per-user state dir",
                path.display()
            );
        }
        // The two must not collide, or the ledger overwrites the cache.
        assert_ne!(manifest_cache_path(&repo), ledger_path(&repo));
    }

    /// The key must be stable across runs (or every scan is a cold scan) and distinct per
    /// repo (or two clients share one cache, which would be a confidentiality problem as
    /// well as a correctness one).
    #[test]
    fn repo_state_key_is_stable_distinct_and_filesystem_safe() {
        let a = PathBuf::from("/Users/auditor/clients/acme-platform");
        let b = PathBuf::from("/Users/auditor/clients/other-client");

        assert_eq!(repo_state_key(&a), repo_state_key(&a), "must be stable");
        assert_ne!(
            repo_state_key(&a),
            repo_state_key(&b),
            "two repos must never share a cache key"
        );

        // Same basename, different parents, must still differ — the hash covers the full
        // absolute path precisely so two clients' `web` directories do not collide.
        assert_ne!(
            repo_state_key(Path::new("/clients/one/web")),
            repo_state_key(Path::new("/clients/two/web")),
        );

        let key = repo_state_key(&a);
        assert!(
            key.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'),
            "key `{key}` must be filesystem-safe"
        );
        assert!(
            key.contains("acme-platform"),
            "key `{key}` should stay human-readable so an operator can tell which cache is which"
        );
    }

    #[test]
    fn format_outcome_report_names_the_export_path_and_backend() {
        let outcome = InspectOutcome {
            run_id: None,
            export_path: PathBuf::from("/tmp/out.zip"),
            export_zip_bytes: 4096,
            backend: ProjectBackend::Api,
            batch: false,
            actual_usage: Some(synthetic_usage()),
            findings_count: 3,
            ledger_summary: "== PIPELINE STAGES ==\n  (no stages recorded this scan)".to_string(),
            ledger_path: PathBuf::from("/tmp/state/out-deadbeef.ledger.json"),
            release_gate: "release gate: passed (6 checks)".to_string(),
            stage_telemetry: None,
            preflight: None,
            efficiency: camerata_server::efficiency::EfficiencyConfig::default(),
            cost_cap: None,
            outage: None,
            incomplete_reasons: Vec::new(),
            failure_lines: Vec::new(),
        };
        let text = format_outcome_report(&outcome);
        assert!(text.contains("/tmp/out.zip"), "{text}");
        assert!(text.contains("4096"), "{text}");
        assert!(text.contains("backend: api"), "{text}");
        assert!(text.contains("findings: 3"), "{text}");
        assert!(
            text.contains("PIPELINE STAGES"),
            "the ledger summary must be appended to the final report block: {text}"
        );
        assert!(
            !text.contains("STAGE TELEMETRY"),
            "a scan that made no model call has no telemetry table: {text}"
        );
    }

    /// Rank 1: the per-stage cost table closes the operator's output, so the next sweep can read
    /// where the money went without hand-summing a log.
    #[test]
    fn format_outcome_report_prints_the_per_stage_telemetry_table() {
        let meter = camerata_server::stage_telemetry::StageMeter::default();
        meter.record(
            camerata_server::stage_telemetry::Stage::ScanPass,
            &camerata_server::llm::LlmResponse {
                text: String::new(),
                model: "m".to_string(),
                backend: "api".to_string(),
                cost_usd: Some(2.0),
                input_tokens: Some(9_000),
                output_tokens: Some(700),
                cache_read_input_tokens: 4_000,
                cache_creation_input_tokens: 2_000,
                or_cache_discount: None,
                truncated: false,
            },
        );
        let outcome = InspectOutcome {
            run_id: None,
            export_path: PathBuf::from("/tmp/out.zip"),
            export_zip_bytes: 1,
            backend: ProjectBackend::Api,
            batch: false,
            actual_usage: Some(synthetic_usage()),
            findings_count: 0,
            ledger_summary: String::new(),
            ledger_path: PathBuf::from("/tmp/state/x.ledger.json"),
            release_gate: String::new(),
            stage_telemetry: Some(meter.snapshot(8_000)),
            preflight: None,
            efficiency: camerata_server::efficiency::EfficiencyConfig::default(),
            cost_cap: None,
            outage: None,
            incomplete_reasons: Vec::new(),
            failure_lines: Vec::new(),
        };
        let text = format_outcome_report(&outcome);
        assert!(text.contains("STAGE TELEMETRY"), "{text}");
        assert!(text.contains("$/1K lines"), "{text}");
        assert!(text.contains("counted lines: 8000"), "{text}");
    }

    // ── render_ledger_summary ────────────────────────────────────────────────────────

    /// A fully healthy ledger: one rule per family, all ran, a single cleanly-reconciled
    /// stage, no not-run rules, no failed passes. No `!!` marker anywhere — a clean scan must
    /// read as unambiguously clean.
    #[test]
    fn render_ledger_summary_clean_ledger_has_no_alarms() {
        let mut ledger = ScanLedger::new();
        ledger.record_rule(
            "",
            "SEC-NO-HARDCODED-SECRETS-1",
            RuleTier::Deterministic,
            true,
            None,
            10,
            0,
        );
        ledger.record_rule(
            "",
            "ARCH-STRICT-LAYERING-1",
            RuleTier::Architectural,
            true,
            None,
            5,
            0,
        );
        ledger.record_rule(
            "",
            "SEC-NO-RAW-SQL-CONCAT-1",
            RuleTier::ExternalTool,
            true,
            None,
            8,
            0,
        );
        ledger.record_rule("", "AI-SITE-DEFECT-1", RuleTier::Semantic, true, None, 3, 1);
        ledger.record_stage("cross-family-merge", 1, 1, StageAccounting::default());

        let text = render_ledger_summary(&ledger, &[], false);

        assert!(text.contains("PIPELINE STAGES"), "{text}");
        assert!(text.contains("DETECTION FAMILIES"), "{text}");
        assert!(text.contains("NOT RUN (0)"), "{text}");
        assert!(text.contains("FAILED PASSES (0)"), "{text}");
        assert!(
            !text.contains("!!"),
            "a clean ledger must raise no alarm markers: {text}"
        );
    }

    /// req_w13: the `Unloaded` family's alarm polarity is INVERTED, and this is the one place
    /// in the summary where that is true.
    ///
    /// It is not an engine — it holds rules whose own corpus file would not parse — so an
    /// empty bucket means "the rule library loaded cleanly" and a non-empty one is the alarm.
    /// The generic "ZERO RULES RECORDED — this family never ran at all" line would otherwise
    /// fire on EVERY correct scan, which is the cries-wolf failure mode that gets a gate
    /// switched off (it did fire, on
    /// `render_ledger_summary_clean_ledger_has_no_alarms`, the moment the tier was added).
    #[test]
    fn req_w13_the_unloaded_family_alarms_on_a_broken_rule_file_not_on_an_empty_bucket() {
        // Empty bucket: a positive disclosure, no alarm.
        let mut clean = ScanLedger::new();
        clean.record_rule(
            "",
            "SEC-NO-RAW-SQL-CONCAT-1",
            RuleTier::Deterministic,
            true,
            None,
            4,
            0,
        );
        let clean_text = render_ledger_summary(&clean, &[], false);
        let clean_line = clean_text
            .lines()
            .find(|l| l.contains(RuleTier::Unloaded.family_label()))
            .expect("the family must always get its own line, never silent omission");
        assert!(
            !clean_line.contains("!!"),
            "an empty Unloaded bucket is the GOOD state: {clean_line}"
        );
        assert!(
            clean_line.contains("the rule library loaded cleanly"),
            "and it must say so positively rather than printing a bare zero: {clean_line}"
        );

        // A broken rule file: the alarm.
        let mut broken = clean.clone();
        broken.record_corpus_load_failure("SUPABASE-RLS-ENABLED-1");
        let broken_text = render_ledger_summary(&broken, &[], false);
        let broken_line = broken_text
            .lines()
            .find(|l| l.contains(RuleTier::Unloaded.family_label()))
            .expect("the family line must still be present");
        assert!(
            broken_line.contains("!!") && broken_line.contains("FAILED TO PARSE"),
            "a rule file that would not parse must raise the loud marker: {broken_line}"
        );
        // And the rule itself is named in NOT RUN, so a reader gets the id, not just a count.
        assert!(
            broken_text.contains("SUPABASE-RLS-ENABLED-1"),
            "the NOT RUN section must name it: {broken_text}"
        );
    }

    /// The core integrity check, surfaced to a human: a stage that lost a row with no recorded
    /// disposition must render with the loud `!!` marker, never blend into a normal-looking
    /// line.
    #[test]
    fn render_ledger_summary_flags_a_stage_with_unaccounted_rows() {
        let mut ledger = ScanLedger::new();
        ledger.record_stage("lossy-stage", 10, 8, StageAccounting::default()); // 2 vanish

        let text = render_ledger_summary(&ledger, &[], false);

        let stage_line = text
            .lines()
            .find(|l| l.contains("lossy-stage"))
            .expect("the lossy stage must have its own line");
        assert!(
            stage_line.contains("!!"),
            "a stage with unaccounted rows must carry a loud marker: {stage_line}"
        );
        assert!(stage_line.contains("unaccounted=2"), "{stage_line}");
    }

    /// The exact defect this feature exists to catch: an entire detection family (here,
    /// external-tool/taint) recorded ZERO rules this scan. It must be impossible to miss —
    /// flagged loudly, not silently absent from the output.
    #[test]
    fn render_ledger_summary_flags_a_family_with_zero_rules_recorded() {
        let mut ledger = ScanLedger::new();
        ledger.record_rule(
            "",
            "SEC-NO-HARDCODED-SECRETS-1",
            RuleTier::Deterministic,
            true,
            None,
            10,
            0,
        );
        // No ExternalTool, Architectural, or Semantic rule recorded at all this scan.

        let text = render_ledger_summary(&ledger, &[], false);

        let family_line = text
            .lines()
            .find(|l| l.contains(RuleTier::ExternalTool.family_label()))
            .expect("every family must get its own line even with zero rules");
        assert!(
            family_line.contains("!!") && family_line.to_uppercase().contains("ZERO"),
            "a family with no recorded rules must be flagged loudly, not silently omitted: {family_line}"
        );
    }

    /// The flip side of the same defect: a family DID get ledger entries, but every single one
    /// is recorded as not-run (e.g. the commodity taint pass's rules were all corrected to
    /// `ran = false` after the tool failed to provision). `ran == 0` must be flagged exactly
    /// like zero rules recorded — the family never actually executed either way.
    #[test]
    fn render_ledger_summary_flags_a_family_whose_rules_all_failed_to_run() {
        let mut ledger = ScanLedger::new();
        ledger.record_rule(
            "",
            "SEC-NO-RAW-SQL-CONCAT-1",
            RuleTier::ExternalTool,
            false,
            Some("commodity taint pass did not run: semgrep binary not found".to_string()),
            0,
            0,
        );

        let text = render_ledger_summary(&ledger, &[], false);

        let family_line = text
            .lines()
            .find(|l| l.contains(RuleTier::ExternalTool.family_label()))
            .expect("the external-tool family line must be present");
        assert!(
            family_line.contains("!!") && family_line.contains("ran=0"),
            "a family with rules recorded but none that ran must be flagged: {family_line}"
        );
    }

    /// Not-run rules (any reason) must render with their real reason text in the NOT RUN
    /// section, so a dark layer can never hide even when every stage happens to reconcile.
    #[test]
    fn render_ledger_summary_renders_not_run_reasons() {
        let mut ledger = ScanLedger::new();
        ledger.record_rule(
            "",
            "PYTHON-PARAMETERIZED-SQL-1",
            RuleTier::Architectural,
            false,
            Some("declares mechanical enforcement but has no wired detector".to_string()),
            0,
            0,
        );
        ledger.record_rule(
            "",
            "SEC-NO-HARDCODED-SECRETS-1",
            RuleTier::Deterministic,
            false,
            Some("deterministic scan deselected for this run".to_string()),
            0,
            0,
        );

        let text = render_ledger_summary(&ledger, &[], false);

        assert!(text.contains("NOT RUN (2)"), "{text}");
        assert!(
            text.contains("PYTHON-PARAMETERIZED-SQL-1") && text.contains("no wired detector"),
            "{text}"
        );
        assert!(
            text.contains("SEC-NO-HARDCODED-SECRETS-1")
                && text.contains("deterministic scan deselected for this run"),
            "{text}"
        );
    }

    /// `FailedPass` disclosures must be printed plainly — repo, pass name, and the real reason
    /// text verbatim, never summarized away.
    #[test]
    fn render_ledger_summary_prints_failed_passes_plainly() {
        let ledger = ScanLedger::new();
        let failed_passes = vec![FailedPass {
            repo: "acme/widgets".to_string(),
            pass: "commodity taint pass".to_string(),
            reason: "SEC-NO-RAW-SQL-CONCAT-1: commodity taint pass did not run: semgrep absent"
                .to_string(),
            files_not_reviewed: 0,
            class: camerata_server::ai_audit::FailureClass::Content,
            files: Vec::new(),
            attempts: Vec::new(),
            scope_note: false,
        }];

        let text = render_ledger_summary(&ledger, &failed_passes, false);

        assert!(text.contains("FAILED PASSES (1)"), "{text}");
        assert!(text.contains("acme/widgets"), "{text}");
        assert!(text.contains("commodity taint pass"), "{text}");
        assert!(text.contains("semgrep absent"), "{text}");
    }

    /// Default (non-verbose) output must NOT list individual rules — only `--verbose` does.
    #[test]
    fn render_ledger_summary_default_omits_per_rule_detail() {
        let mut ledger = ScanLedger::new();
        ledger.record_rule(
            "",
            "SEC-NO-HARDCODED-SECRETS-1",
            RuleTier::Deterministic,
            true,
            None,
            10,
            0,
        );

        let concise = render_ledger_summary(&ledger, &[], false);
        assert!(!concise.contains("PER-RULE DETAIL"), "{concise}");

        let verbose = render_ledger_summary(&ledger, &[], true);
        assert!(verbose.contains("PER-RULE DETAIL"), "{verbose}");
        assert!(verbose.contains("SEC-NO-HARDCODED-SECRETS-1"), "{verbose}");
        assert!(verbose.contains("files_evaluated=10"), "{verbose}");
    }
}
