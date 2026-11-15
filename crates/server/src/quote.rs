//! The two-step quote (owner ruling R-9, 2026-11-15): the pre-flight figure, then a firmer
//! re-quote once the scan stage has run and before most of the money is spent.
//!
//! The pre-flight (`camerata inspect`'s, printed before any model call) prices the run on a fitted
//! number of sites per file. The stages after the scan scale with the sites the scan actually
//! reports, so once one repo's scan stage has finished the run quotes again from that count and
//! from what the scan stage actually cost, with the same per-stage price model
//! ([`camerata_ui_core::scan::requote_after_scan`], whose module comment gives the formula). The
//! re-quote is printed in the pre-flight's format, shown in the cockpit's run view, kept in the
//! run's folder (so a resume prices what remains from it) and recorded on the scan ledger beside
//! the pre-flight figure and, once the run ends, what it actually cost ([`QuoteRecord`]).
//!
//! This module makes no model call and changes nothing about how the run is stopped: the spend
//! limit stops it between calls exactly as before. The re-quote only says, on its own line, when
//! its expected cost is above the limit.

use camerata_ui_core::scan::{
    estimate_run_cost_by_repo, requote_after_scan, PricedModel, Requote, RequoteInput, RunFlags,
    RunModels, RunRepoInput,
};
use serde::{Deserialize, Serialize};

use crate::llm::ProjectBackend;
use crate::preflight::RepoScanFacts;

/// A model the quote prices, with its list prices in US dollars per million tokens.
#[derive(Debug, Clone, PartialEq)]
pub struct QuotedModel {
    pub id: String,
    pub price_in: f64,
    pub price_out: f64,
}

impl QuotedModel {
    /// `id` at its list price, or `None` when the registry has no price for it.
    pub fn priced(id: &str) -> Option<Self> {
        crate::llm::models()
            .into_iter()
            .find(|m| m.id == id)
            .map(|m| Self {
                id: id.to_string(),
                price_in: m.price_in,
                price_out: m.price_out,
            })
    }

    fn as_priced(&self) -> PricedModel<'_> {
        PricedModel {
            id: &self.id,
            price_in: self.price_in,
            price_out: self.price_out,
        }
    }
}

/// The models a run calls, resolved the way its stages resolve them.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct QuoteModelIds {
    pub scan: String,
    pub calibration: String,
    pub group_review: String,
    /// `None` when the fix stage does not run.
    pub fix: Option<String>,
    /// `None` when no clone rule is selected.
    pub clone_judge: Option<String>,
}

/// The models of a run, resolved from its picks and controls exactly as the stages and the
/// pre-flight resolve them.
pub fn resolve_quote_models(
    model: Option<&str>,
    calibration_model: Option<&str>,
    controls: &crate::cost_cap::RunControls,
    selected_rule_ids: &[&str],
) -> QuoteModelIds {
    let scan = crate::ai_audit::resolve_audit_model(model);
    let judge_selected = crate::cost_cap::models_the_run_will_use(
        model,
        calibration_model,
        false,
        selected_rule_ids,
        controls,
    )
    .iter()
    .any(|(roles, _)| roles.contains("the clone judge"));
    QuoteModelIds {
        calibration: crate::ai_audit::resolve_calibration_model(calibration_model, &scan),
        group_review: controls.group_review_model_for(),
        fix: Some(crate::ai_audit::resolve_audit_model(
            controls.fix_model_for(model, calibration_model),
        )),
        clone_judge: judge_selected.then(|| controls.clone_judge_model_for()),
        scan,
    }
}

/// What each readable repo contributes to the run estimate: its code, files and rules, and the
/// routing groups the scan splits it into. A repo that could not be read is left out (the
/// pre-flight names it instead).
pub fn run_inputs(facts: &[RepoScanFacts]) -> Vec<RunRepoInput> {
    facts
        .iter()
        .filter(|r| r.read_error.is_none())
        .map(|r| RunRepoInput {
            repo: r.repo.clone(),
            code_chars: r.code_chars,
            files: r.files.len(),
            selected: r.semantic.len(),
            multi_option_selected: r.multi_option_rules,
            routing_groups: crate::scan_routing::plan_routes(
                &crate::scan_routing::rules_from_pairs(&r.semantic),
                &r.files,
                true,
            )
            .groups
            .len()
            .max(1),
        })
        .collect()
}

/// Where the live re-quote line is written, besides the scan ledger, the run's folder and the
/// cockpit job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum QuoteEcho {
    /// Nowhere else (tests).
    #[default]
    Silent,
    /// Standard output, where `camerata inspect` prints its pre-flight.
    Stdout,
    /// Standard error, the server's log.
    Stderr,
}

/// What the pre-flight priced, kept with the run so it can quote again once the scan stage has
/// run: the same repos, models and flags, and the pre-flight's own figure.
#[derive(Debug, Clone, PartialEq)]
pub struct QuotePlan {
    /// The scan mode string the estimate takes (`batch` or `parallel`).
    pub mode: String,
    pub backend: ProjectBackend,
    pub terse_output: bool,
    /// The consensus passes (1 unless consensus is on).
    pub runs: usize,
    pub scan: QuotedModel,
    pub calibration: QuotedModel,
    /// `None` leaves stage 2 out of the figure (its model has no list price).
    pub group_review: Option<QuotedModel>,
    pub fix: Option<QuotedModel>,
    pub clone_judge: Option<QuotedModel>,
    /// The repos in the order the run scans them.
    pub repos: Vec<RunRepoInput>,
    /// The pre-flight figure of the whole run.
    pub preflight_expected_usd: f64,
    /// The pre-flight ceiling of the whole run.
    pub preflight_ceiling_usd: f64,
    pub echo: QuoteEcho,
}

impl QuotePlan {
    /// The plan of a run over `repos` (in scan order) at `models`, with the pre-flight figure the
    /// command line prints. `None` when the scan or calibration model has no list price (the
    /// pre-flight then prints no figure either). A later stage whose model has no price is left
    /// out, as the pre-flight leaves it out.
    pub fn new(
        models: &QuoteModelIds,
        repos: Vec<RunRepoInput>,
        backend: ProjectBackend,
        batch: bool,
        terse_output: bool,
        consensus: bool,
    ) -> Option<Self> {
        let mut plan = Self {
            mode: if batch { "batch" } else { "parallel" }.to_string(),
            backend,
            terse_output,
            runs: if consensus {
                crate::consensus::CONSENSUS_PASSES
            } else {
                1
            },
            scan: QuotedModel::priced(&models.scan)?,
            calibration: QuotedModel::priced(&models.calibration)?,
            group_review: QuotedModel::priced(&models.group_review),
            fix: models.fix.as_deref().and_then(QuotedModel::priced),
            clone_judge: models.clone_judge.as_deref().and_then(QuotedModel::priced),
            repos,
            preflight_expected_usd: 0.0,
            preflight_ceiling_usd: 0.0,
            echo: QuoteEcho::Silent,
        };
        let e = estimate_run_cost_by_repo(&plan.repos, &plan.mode, &plan.models(), plan.flags());
        plan.preflight_expected_usd = e.dollars * plan.runs as f64;
        plan.preflight_ceiling_usd = e.ceiling_usd * plan.runs as f64;
        Some(plan)
    }

    /// The models as the estimate takes them.
    pub fn models(&self) -> RunModels<'_> {
        RunModels {
            scan: self.scan.as_priced(),
            calibration: self.calibration.as_priced(),
            group_review: self.group_review.as_ref().map(QuotedModel::as_priced),
            fix: self.fix.as_ref().map(QuotedModel::as_priced),
            clone_judge: self.clone_judge.as_ref().map(QuotedModel::as_priced),
        }
    }

    /// The flags as the estimate takes them (the pre-flight never prices the deep tier).
    pub fn flags(&self) -> RunFlags {
        RunFlags {
            deep: false,
            backend: self.backend,
            terse_output: self.terse_output,
        }
    }

    /// The repos scanned after `repo`, in the plan's order.
    pub fn repos_after(&self, repo: &str) -> &[RunRepoInput] {
        match self.repos.iter().position(|r| r.repo == repo) {
            Some(i) => &self.repos[i + 1..],
            None => &[],
        }
    }

    /// The re-quote once `at.repo`'s scan stage has finished; `None` for a repo the plan does not
    /// hold (the pre-flight did not price it, so there is nothing to quote it against).
    pub fn requote(&self, at: &RequotePoint<'_>) -> Option<Requote> {
        let repo = self.repos.iter().find(|r| r.repo == at.repo)?;
        Some(requote_after_scan(
            &RequoteInput {
                repo,
                sites: at.sites,
                scan_spent_usd: at.scan_spent_usd,
                run_spent_usd: at.run_spent_usd,
                stored_ahead_usd: at.stored_ahead_usd,
                later: self.repos_after(at.repo),
                runs: self.runs,
                preflight_expected_usd: self.preflight_expected_usd,
                preflight_ceiling_usd: self.preflight_ceiling_usd,
                cap_usd: at.cap_usd,
                resumed: at.resumed,
            },
            &self.mode,
            &self.models(),
            self.flags(),
        ))
    }
}

/// The plan of a run whose caller made no pre-flight of its own (the cockpit, and a resumed
/// command-line run): the repos read the way the scan reads them, the models resolved the way the
/// stages resolve them, echoed to `echo`. `None` where the compliance gate blocked the AI review
/// (no model call is made) or the scan or calibration model has no list price.
#[allow(clippy::too_many_arguments)]
pub async fn plan_for_run(
    sources: &[(String, std::path::PathBuf)],
    selected: &[crate::onboard::SelectedRule],
    corpus: Option<&camerata_rules::RuleSet>,
    model: Option<&str>,
    calibration_model: Option<&str>,
    controls: &crate::cost_cap::RunControls,
    resolution: &crate::llm::BackendResolution,
    batch: bool,
    echo: QuoteEcho,
) -> Option<QuotePlan> {
    let backend = match resolution {
        crate::llm::BackendResolution::Api => ProjectBackend::Api,
        crate::llm::BackendResolution::Cli => ProjectBackend::Cli,
        crate::llm::BackendResolution::Blocked { .. } => return None,
    };
    let facts = crate::preflight::gather_scan_facts(sources, selected, corpus).await;
    let ids: Vec<&str> = selected.iter().map(|r| r.id.as_str()).collect();
    QuotePlan::new(
        &resolve_quote_models(model, calibration_model, controls, &ids),
        run_inputs(&facts),
        backend,
        batch,
        controls.efficiency.terse_output,
        controls.consensus,
    )
    .map(|plan| QuotePlan { echo, ..plan })
}

/// The facts of the moment one repo's scan stage finished.
#[derive(Debug, Clone, Copy)]
pub struct RequotePoint<'a> {
    pub repo: &'a str,
    /// The rows that go on to calibration.
    pub sites: usize,
    /// What this repo's scan stage cost on this pass.
    pub scan_spent_usd: f64,
    /// What the whole run has cost so far, as the spend limit counts it.
    pub run_spent_usd: f64,
    /// On a resume, what the stored replies of the work still ahead cost (zero otherwise).
    pub stored_ahead_usd: f64,
    pub cap_usd: Option<f64>,
    pub resumed: bool,
}

/// The run's quotes and what it actually cost, kept on the scan ledger (operator-only: it carries
/// dollar figures, and no exporter renders it).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct QuoteRecord {
    /// The pre-flight figure of the whole run.
    pub preflight_expected_usd: f64,
    /// The pre-flight ceiling of the whole run.
    pub preflight_ceiling_usd: f64,
    /// One re-quote per repo, made when its scan stage finished, in the order they were made.
    #[serde(default)]
    pub requotes: Vec<Requote>,
    /// What this attempt of the run actually cost, every stage included. `None` until the run
    /// has ended.
    #[serde(default)]
    pub actual_usd: Option<f64>,
    /// On a resumed run, what the stored replies of the earlier attempts cost (zero otherwise):
    /// the whole run's cost is `actual_usd` plus this.
    #[serde(default)]
    pub stored_usd: f64,
}

impl QuoteRecord {
    /// The latest re-quote, the one made with the most known.
    pub fn latest(&self) -> Option<&Requote> {
        self.requotes.last()
    }

    /// What the whole run cost: this attempt's spend plus, on a resume, the stored replies'.
    pub fn whole_run_actual_usd(&self) -> Option<f64> {
        self.actual_usd.map(|a| a + self.stored_usd.max(0.0))
    }

    /// The block the command line prints at the end of a run: both quotes, the actual and how
    /// each quote compares with it (the target is 1.0 to 1.5 times the actual, never under).
    pub fn render(&self) -> String {
        let mut out = format!(
            "quotes: the pre-flight said about ${:.2}, at most ${:.2}\n",
            self.preflight_expected_usd, self.preflight_ceiling_usd
        );
        for q in &self.requotes {
            out.push_str(&format!("  {}\n", q.line()));
        }
        if let Some(actual) = self.whole_run_actual_usd() {
            let times = |quote: f64| {
                if actual > 0.0 {
                    format!("{:.2}", quote / actual)
                } else {
                    "n/a".to_string()
                }
            };
            out.push_str(&format!(
                "  actual: ${actual:.2}; the pre-flight was {} times it",
                times(self.preflight_expected_usd)
            ));
            if let Some(q) = self.latest() {
                out.push_str(&format!(
                    " and the re-quote {} times it",
                    times(q.expected_usd)
                ));
            }
            out.push_str(" (the target is 1.0 to 1.5 times, never under)\n");
        }
        out
    }
}

/// Make the re-quote once `repo`'s scan stage has finished and before calibration starts, on the
/// run's meter, when the run carries a plan ([`crate::cost_cap::RunControls::quote_plan`]). The
/// first pass of a repo makes it (consensus runs the audit more than once; the re-quote already
/// counts every pass). The line is written where the plan says, set on the cockpit job, kept in
/// the run's folder and held on the meter for the scan ledger. Returns the re-quote it made.
pub(crate) fn requote_after_scan_stage(
    meter: Option<&crate::ai_audit::UsageMeter>,
    job: Option<(&crate::jobs::JobStore, &str)>,
    repo: &str,
    sites: usize,
    scan_spent_usd: f64,
) -> Option<Requote> {
    let meter = meter?;
    let plan = meter.controls().quote_plan.as_ref()?;
    if meter.has_requoted(repo) {
        return None;
    }
    let resumed = meter.is_resume();
    let run_dir = crate::checkpoint::run_dir_in_scope();
    let stored_ahead_usd = match (resumed, run_dir.as_deref()) {
        (true, Some(dir)) => {
            let later: Vec<&str> = plan
                .repos_after(repo)
                .iter()
                .map(|r| r.repo.as_str())
                .collect();
            crate::run_store::journal::stored_ahead_usd(dir, repo, &later)
        }
        _ => 0.0,
    };
    let q = plan.requote(&RequotePoint {
        repo,
        sites,
        scan_spent_usd,
        run_spent_usd: meter.limit_spend_usd(),
        stored_ahead_usd,
        cap_usd: meter.controls().max_cost_usd,
        resumed,
    })?;
    let line = q.line();
    match plan.echo {
        QuoteEcho::Silent => {}
        QuoteEcho::Stdout => println!("  {line}"),
        QuoteEcho::Stderr => eprintln!("[camerata-server] {line}"),
    }
    if let Some((jobs, id)) = job {
        jobs.set_requote(id, &line);
    }
    if let Some(dir) = run_dir.as_deref() {
        crate::run_store::journal::save_requote(dir, &q);
    }
    meter.note_requote(q.clone());
    Some(q)
}

#[cfg(test)]
mod tests;
