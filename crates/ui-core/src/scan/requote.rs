//! The re-quote after the scan stage (owner ruling R-9, 2026-11-15).
//!
//! The pre-flight estimate is printed before any model call, so it has to guess how many sites
//! (problem places) the scan will report: [`RUN_FIT`]'s fixed rate per file. Real runs ranged from
//! 2.16 to 3.76 sites a file against the 2.78 the fit assumes, and every stage after the scan
//! (detail, calibration, the group review, the fix stage, the clone judge) scales with the sites.
//! Those stages are most of the spend, so the pre-flight runs low on dense code and high on
//! sparse code.
//!
//! The scan stage is the cheap first stage. Once it has run, the site count is a fact, so the run
//! quotes again before most of the money is spent:
//!
//! ```text
//! after     = the stages after the scan, priced by the same per-stage model at the ACTUAL sites
//! later     = the pre-flight figure of every repo not scanned yet
//! remaining = runs * (after + later) + (runs - 1) * scan, less the stored calls still ahead
//! re-quote  = what the run has spent so far + remaining
//! ```
//!
//! `scan` is what this repo's scan stage actually cost (the alternatives pass included, since it
//! runs first), `runs` the consensus passes (1 unless consensus is on, which the pre-flight
//! multiplies the same way) and the stored calls ahead are the replies a resumed run already holds
//! for the work that remains (zero on a fresh run): it replays them rather than paying again. The
//! "at most" figure is the same sum with the detail stage at every repeat round
//! ([`regeneration_estimate`]) and the later repos at their pre-flight ceiling.

use serde::{Deserialize, Serialize};

use super::{
    estimate_run_cost_bounds, estimate_run_stages_with_regeneration, regeneration_estimate,
    regeneration_expected, RunFlags, RunModels, RunRepoInput, RunStageDollars, RUN_FIT,
};

/// What the stages after the scan cost for one repo at a known site count.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AfterScanEstimate {
    /// The usual run: the detail stage pays for its first round only.
    pub expected_usd: f64,
    /// The detail stage at every repeat round on every finding. Never below `expected_usd`.
    pub ceiling_usd: f64,
}

/// The dollars of the stages that run after the scan stage.
fn after_scan_dollars(stages: &RunStageDollars) -> f64 {
    stages.detail
        + stages.calibration
        + stages.group_review
        + stages.fix
        + stages.deep
        + stages.clone_judge
}

/// The stages after the scan stage of `repo` (detail, calibration, the group review, the fix stage,
/// the deep tier and the clone judge), priced by the run estimate's own per-stage model at the
/// `sites` the scan stage actually reported instead of the fitted rate per file.
pub fn estimate_after_scan(
    repo: &RunRepoInput,
    sites: usize,
    mode: &str,
    models: &RunModels<'_>,
    flags: RunFlags,
) -> AfterScanEstimate {
    let (_, expected, _) = estimate_run_stages_with_regeneration(
        &RUN_FIT,
        repo,
        mode,
        models,
        flags,
        regeneration_expected,
        true,
        Some(sites),
    );
    let (_, ceiling, _) = estimate_run_stages_with_regeneration(
        &RUN_FIT,
        repo,
        mode,
        models,
        flags,
        regeneration_estimate,
        false,
        Some(sites),
    );
    let expected_usd = after_scan_dollars(&expected);
    AfterScanEstimate {
        expected_usd,
        ceiling_usd: after_scan_dollars(&ceiling).max(expected_usd),
    }
}

/// Everything the re-quote is made from, at the moment one repo's scan stage has finished.
#[derive(Clone, Copy, Debug)]
pub struct RequoteInput<'a> {
    /// The repo whose scan stage just finished, as the pre-flight priced it.
    pub repo: &'a RunRepoInput,
    /// The sites the scan stage reported: the rows that go on to calibration.
    pub sites: usize,
    /// What this repo's scan stage actually cost on this pass (its alternatives pass included).
    pub scan_spent_usd: f64,
    /// What the whole run has cost so far, as the spend limit counts it: on a resume, every stored
    /// reply included.
    pub run_spent_usd: f64,
    /// On a resume: what the stored replies of the work still ahead cost. They are replayed, not
    /// sent again, so they come off the remaining figure (they are already in `run_spent_usd`).
    /// Zero on a fresh run.
    pub stored_ahead_usd: f64,
    /// The repos whose scan has not started, as the pre-flight priced them.
    pub later: &'a [RunRepoInput],
    /// The consensus passes the run makes (1 unless consensus is on).
    pub runs: usize,
    /// The pre-flight figure of the whole run, for the line.
    pub preflight_expected_usd: f64,
    /// The pre-flight ceiling of the whole run, for the line.
    pub preflight_ceiling_usd: f64,
    /// `--max-cost-usd`, when one is set.
    pub cap_usd: Option<f64>,
    /// The run is a resume, so the line is about the work that remains.
    pub resumed: bool,
}

/// The re-quote made after one repo's scan stage. Kept with the run (the scan ledger and the
/// run's folder), beside the pre-flight figure and, once the run ends, what it actually cost.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Requote {
    /// The repo whose scan stage the re-quote follows.
    pub repo: String,
    /// The sites its scan stage reported.
    pub sites: usize,
    /// The files the pre-flight counted for it (0 when it did not know them).
    pub files: usize,
    /// What its scan stage actually cost.
    pub scan_spent_usd: f64,
    /// The whole run's expected cost: what it has spent so far plus the remaining work.
    pub expected_usd: f64,
    /// The whole run's "at most" figure.
    pub ceiling_usd: f64,
    /// The work that remains, less the stored replies a resume replays.
    pub remaining_expected_usd: f64,
    /// The "at most" figure of the work that remains.
    pub remaining_ceiling_usd: f64,
    /// The pre-flight figure of the whole run.
    pub preflight_expected_usd: f64,
    /// The pre-flight ceiling of the whole run.
    pub preflight_ceiling_usd: f64,
    /// The spend limit in force, when one was set.
    #[serde(default)]
    pub cap_usd: Option<f64>,
    /// Made on a resumed run, so the line speaks of the work that remains.
    #[serde(default)]
    pub resumed: bool,
}

/// The re-quote of the whole run after one repo's scan stage; the module comment gives the
/// formula.
pub fn requote_after_scan(
    input: &RequoteInput<'_>,
    mode: &str,
    models: &RunModels<'_>,
    flags: RunFlags,
) -> Requote {
    let runs = input.runs.max(1) as f64;
    let after = estimate_after_scan(input.repo, input.sites, mode, models, flags);
    let (later_expected, later_ceiling) =
        input
            .later
            .iter()
            .fold((0.0f64, 0.0f64), |(expected, ceiling), r| {
                let bounds = estimate_run_cost_bounds(r, mode, models, flags);
                (
                    expected + bounds.expected.1,
                    ceiling + bounds.ceiling_usd.max(bounds.expected.1),
                )
            });
    // The other consensus passes of this repo repeat its scan stage, at what this one cost.
    let other_scans = (runs - 1.0) * input.scan_spent_usd.max(0.0);
    let stored = input.stored_ahead_usd.max(0.0);
    let remaining_expected =
        (runs * (after.expected_usd + later_expected) + other_scans - stored).max(0.0);
    let remaining_ceiling =
        (runs * (after.ceiling_usd + later_ceiling) + other_scans - stored).max(remaining_expected);
    let spent = input.run_spent_usd.max(0.0);
    Requote {
        repo: input.repo.repo.clone(),
        sites: input.sites,
        files: input.repo.files,
        scan_spent_usd: input.scan_spent_usd.max(0.0),
        expected_usd: spent + remaining_expected,
        ceiling_usd: spent + remaining_ceiling,
        remaining_expected_usd: remaining_expected,
        remaining_ceiling_usd: remaining_ceiling,
        preflight_expected_usd: input.preflight_expected_usd,
        preflight_ceiling_usd: input.preflight_ceiling_usd,
        cap_usd: input.cap_usd,
        resumed: input.resumed,
    }
}

impl Requote {
    /// True when the whole run's expected cost is above the spend limit.
    pub fn over_limit(&self) -> bool {
        self.cap_usd.is_some_and(|cap| self.expected_usd > cap)
    }

    /// The line the command line prints and the cockpit's run view shows; see [`requote_line`].
    pub fn line(&self) -> String {
        requote_line(self)
    }
}

/// `re-quote after the scan stage: about $X, at most $Y (...)`, in the pre-flight's format. A
/// fresh run quotes the whole run; a resume quotes the work that remains and names the whole
/// run's figure beside it. When the expected cost is above the spend limit the line says so.
pub fn requote_line(q: &Requote) -> String {
    let (about, most) = if q.resumed {
        (q.remaining_expected_usd, q.remaining_ceiling_usd)
    } else {
        (q.expected_usd, q.ceiling_usd)
    };
    let mut line = format!("re-quote after the scan stage: about ${about:.2}, at most ${most:.2}");
    if q.resumed {
        line.push_str(" for the work that remains");
    }
    let density = if q.files > 0 {
        format!(
            "{} sites in {} files, {:.2} a file where the pre-flight assumed {:.2}",
            q.sites,
            q.files,
            q.sites as f64 / q.files as f64,
            RUN_FIT.sites_per_file
        )
    } else {
        format!("{} sites", q.sites)
    };
    let basis = if q.resumed {
        format!(
            "the whole run comes to about ${:.2} with the stored calls",
            q.expected_usd
        )
    } else {
        format!(
            "the pre-flight said about ${:.2}, at most ${:.2}",
            q.preflight_expected_usd, q.preflight_ceiling_usd
        )
    };
    line.push_str(&format!(" ({}: {density}; {basis})", q.repo));
    if let (true, Some(cap)) = (q.over_limit(), q.cap_usd) {
        line.push_str(&format!(
            ". The expected cost is above the spend limit of ${cap:.2}: the run stops at the \
             limit and says what was not run."
        ));
    }
    line
}

#[cfg(test)]
mod tests {
    use super::super::{estimate_run_stage_dollars, estimate_run_stage_dollars_with, PricedModel};
    use super::*;
    use camerata_api_types::project::ProjectBackend;

    /// A repository of 40 files of about 740 characters each (the saved runs' average), with 30
    /// rules, 2 of them with alternatives.
    fn repo(name: &str) -> RunRepoInput {
        RunRepoInput {
            repo: name.to_string(),
            code_chars: 40 * 740,
            files: 40,
            selected: 30,
            multi_option_selected: 2,
            routing_groups: 1,
        }
    }

    fn priced(id: &str, price_in: f64, price_out: f64) -> PricedModel<'_> {
        PricedModel {
            id,
            price_in,
            price_out,
        }
    }

    fn models() -> RunModels<'static> {
        RunModels {
            scan: priced("claude-sonnet-5-5", 3.0, 15.0),
            calibration: priced("claude-sonnet-5-5", 3.0, 15.0),
            group_review: Some(priced("claude-sonnet-5-5", 3.0, 15.0)),
            fix: Some(priced("claude-haiku-5-5", 1.0, 5.0)),
            clone_judge: None,
        }
    }

    fn flags(backend: ProjectBackend) -> RunFlags {
        RunFlags {
            deep: false,
            backend,
            terse_output: true,
        }
    }

    const MODE: &str = "parallel";

    /// The sites a scan at `rate` sites a file reports over `repo`.
    fn sites_at(repo: &RunRepoInput, rate: f64) -> usize {
        (rate * repo.files as f64).ceil() as usize
    }

    /// The scan stage's synthetic actual: what the price model puts on the scan and the
    /// alternatives pass (neither depends on the sites).
    fn scan_stage_usd(repo: &RunRepoInput, backend: ProjectBackend) -> f64 {
        let s = estimate_run_stage_dollars(repo, MODE, &models(), flags(backend));
        s.scan + s.alternatives
    }

    /// The synthetic actual of a whole run whose scan reported `rate` sites a file: every stage of
    /// the same price model, with the sites at that rate (through the fit, not the re-quote).
    fn actual_at(repo: &RunRepoInput, rate: f64, backend: ProjectBackend) -> f64 {
        let fit = super::super::RunFit {
            sites_per_file: rate,
            ..RUN_FIT
        };
        estimate_run_stage_dollars_with(&fit, repo, MODE, &models(), flags(backend)).total()
    }

    fn requote_at(repo: &RunRepoInput, rate: f64, backend: ProjectBackend) -> Requote {
        let pre = estimate_run_cost_bounds(repo, MODE, &models(), flags(backend));
        let scan = scan_stage_usd(repo, backend);
        requote_after_scan(
            &RequoteInput {
                repo,
                sites: sites_at(repo, rate),
                scan_spent_usd: scan,
                run_spent_usd: scan,
                stored_ahead_usd: 0.0,
                later: &[],
                runs: 1,
                preflight_expected_usd: pre.expected.1,
                preflight_ceiling_usd: pre.ceiling_usd,
                cap_usd: None,
                resumed: false,
            },
            MODE,
            &models(),
            flags(backend),
        )
    }

    /// R-9: a dense scan (3.76 sites a file, the most any run showed) re-quotes above the
    /// pre-flight, a sparse one (2.16, the fewest) below it, on both backends.
    #[test]
    fn a_dense_scan_requotes_higher_and_a_sparse_one_lower_than_the_preflight() {
        for backend in [ProjectBackend::Cli, ProjectBackend::Api] {
            let r = repo("acme/api");
            let pre = estimate_run_cost_bounds(&r, MODE, &models(), flags(backend))
                .expected
                .1;
            let dense = requote_at(&r, 3.76, backend);
            let sparse = requote_at(&r, 2.16, backend);
            assert!(
                dense.expected_usd > pre,
                "{backend:?}: dense {} must re-quote above the pre-flight {pre}",
                dense.expected_usd
            );
            assert!(
                sparse.expected_usd < pre,
                "{backend:?}: sparse {} must re-quote below the pre-flight {pre}",
                sparse.expected_usd
            );
            assert!(dense.ceiling_usd >= dense.expected_usd);
            assert!(sparse.ceiling_usd >= sparse.expected_usd);
        }
    }

    /// R-9: the re-quote is within 1.0 to 1.5 times a synthetic actual built from the same stage
    /// counts (never under it), at both ends of the range of real runs, where the pre-flight is
    /// under the dense run's actual.
    #[test]
    fn the_requote_is_within_one_to_one_and_a_half_times_the_actual() {
        for backend in [ProjectBackend::Cli, ProjectBackend::Api] {
            let r = repo("acme/api");
            for rate in [2.16, 2.78, 3.76] {
                let actual = actual_at(&r, rate, backend);
                let q = requote_at(&r, rate, backend);
                let ratio = q.expected_usd / actual;
                assert!(
                    (1.0 - 1e-9..=1.5).contains(&ratio),
                    "{backend:?} at {rate} a file: re-quote {} is {ratio:.3} times the actual \
                     {actual}",
                    q.expected_usd
                );
            }
            let pre = estimate_run_cost_bounds(&r, MODE, &models(), flags(backend))
                .expected
                .1;
            assert!(
                pre < actual_at(&r, 3.76, backend),
                "{backend:?}: the pre-flight is under the dense run, which is what the re-quote \
                 is for"
            );
        }
    }

    /// The re-quote prices the repos not scanned yet at their pre-flight figure, and counts what
    /// the run has already spent.
    #[test]
    fn the_requote_adds_the_later_repos_and_the_spend_so_far() {
        let (a, b) = (repo("acme/api"), repo("acme/web"));
        let f = flags(ProjectBackend::Cli);
        let alone = requote_at(&a, 3.0, ProjectBackend::Cli);
        let later_b = estimate_run_cost_bounds(&b, MODE, &models(), f).expected.1;
        let mut with_b = alone.clone();
        let scan = scan_stage_usd(&a, ProjectBackend::Cli);
        let q = requote_after_scan(
            &RequoteInput {
                repo: &a,
                sites: sites_at(&a, 3.0),
                scan_spent_usd: scan,
                run_spent_usd: scan + 0.5,
                stored_ahead_usd: 0.0,
                later: std::slice::from_ref(&b),
                runs: 1,
                preflight_expected_usd: 0.0,
                preflight_ceiling_usd: 0.0,
                cap_usd: None,
                resumed: false,
            },
            MODE,
            &models(),
            f,
        );
        with_b.expected_usd += later_b + 0.5;
        assert!(
            (q.expected_usd - with_b.expected_usd).abs() < 1e-9,
            "{} vs {}",
            q.expected_usd,
            with_b.expected_usd
        );
        assert!((q.remaining_expected_usd - (alone.remaining_expected_usd + later_b)).abs() < 1e-9);
    }

    /// R-9: a resume re-quotes only the work that remains. The stored replies of the work ahead
    /// are replayed, not paid again, so they come off what remains; the whole run's figure is the
    /// same, and the line speaks of what remains.
    #[test]
    fn a_resume_requotes_only_the_remaining_work() {
        let r = repo("acme/api");
        let f = flags(ProjectBackend::Cli);
        let scan = scan_stage_usd(&r, ProjectBackend::Cli);
        let sites = sites_at(&r, 3.0);
        let after = estimate_after_scan(&r, sites, MODE, &models(), f);
        let input = |stored: f64, resumed: bool| RequoteInput {
            repo: &r,
            sites,
            scan_spent_usd: scan,
            // On a resume the limit counts every stored reply, those of the work ahead included.
            run_spent_usd: scan + stored,
            stored_ahead_usd: stored,
            later: &[],
            runs: 1,
            preflight_expected_usd: 1.0,
            preflight_ceiling_usd: 1.2,
            cap_usd: None,
            resumed,
        };
        let fresh = requote_after_scan(&input(0.0, false), MODE, &models(), f);
        let stored = after.expected_usd * 0.4;
        let resumed = requote_after_scan(&input(stored, true), MODE, &models(), f);
        assert!(
            (resumed.remaining_expected_usd - (fresh.remaining_expected_usd - stored)).abs() < 1e-9,
            "remaining {} must be the fresh {} less the stored {stored}",
            resumed.remaining_expected_usd,
            fresh.remaining_expected_usd
        );
        assert!((resumed.expected_usd - fresh.expected_usd).abs() < 1e-9);
        let line = resumed.line();
        assert!(
            line.starts_with(&format!(
                "re-quote after the scan stage: about ${:.2}, at most ${:.2} for the work that \
                 remains",
                resumed.remaining_expected_usd, resumed.remaining_ceiling_usd
            )),
            "{line}"
        );
        // Stored replies that cover more than the model's figure leave nothing to send.
        let all = requote_after_scan(&input(after.ceiling_usd * 2.0, true), MODE, &models(), f);
        assert_eq!(all.remaining_expected_usd, 0.0);
        assert_eq!(all.remaining_ceiling_usd, 0.0);
    }

    /// R-9: the line is in the pre-flight's format, names the sites and the pre-flight figure, and
    /// says so plainly when the expected cost is above the spend limit.
    #[test]
    fn the_requote_line_names_the_limit_when_it_is_exceeded() {
        let r = repo("acme/api");
        let mut q = requote_at(&r, 3.76, ProjectBackend::Cli);
        let line = q.line();
        assert!(
            line.starts_with(&format!(
                "re-quote after the scan stage: about ${:.2}, at most ${:.2} (acme/api: 151 sites \
                 in 40 files, 3.77 a file where the pre-flight assumed 2.78; the pre-flight said \
                 about ${:.2}",
                q.expected_usd, q.ceiling_usd, q.preflight_expected_usd
            )),
            "{line}"
        );
        assert!(!line.contains("spend limit"), "{line}");

        q.cap_usd = Some(q.expected_usd * 2.0);
        assert!(!q.over_limit());
        assert!(!q.line().contains("spend limit"), "{}", q.line());

        q.cap_usd = Some(q.expected_usd / 2.0);
        assert!(q.over_limit());
        let over = q.line();
        assert!(
            over.contains(&format!(
                "The expected cost is above the spend limit of ${:.2}: the run stops at the limit \
                 and says what was not run.",
                q.expected_usd / 2.0
            )),
            "{over}"
        );
        for dash in ['\u{2014}', '\u{2013}'] {
            assert!(
                !over.contains(dash),
                "no dashes in user-facing text: {over}"
            );
        }
    }

    /// Consensus runs the audit of each repo more than once; the re-quote multiplies the way the
    /// pre-flight does, and adds the other passes' scan stages at what this one cost.
    #[test]
    fn consensus_requotes_every_pass() {
        let r = repo("acme/api");
        let f = flags(ProjectBackend::Api);
        let sites = sites_at(&r, 3.0);
        let after = estimate_after_scan(&r, sites, MODE, &models(), f);
        let q = requote_after_scan(
            &RequoteInput {
                repo: &r,
                sites,
                scan_spent_usd: 0.25,
                run_spent_usd: 0.25,
                stored_ahead_usd: 0.0,
                later: &[],
                runs: 3,
                preflight_expected_usd: 0.0,
                preflight_ceiling_usd: 0.0,
                cap_usd: None,
                resumed: false,
            },
            MODE,
            &models(),
            f,
        );
        let want = 0.25 + 3.0 * after.expected_usd + 2.0 * 0.25;
        assert!(
            (q.expected_usd - want).abs() < 1e-9,
            "{} vs {want}",
            q.expected_usd
        );
    }
}
