//! The pre-flight estimate against what saved runs cost, stage by stage.
//!
//! The estimate is what a client quote is read from, so it must err high but not by much: between
//! 1.0 and 1.5 times what the run cost. These are the figures of the complete cli runs made with
//! the engine as it sends calls now (no tools, the caller's own system prompt). Each stage's
//! dollars are the run's own stage table (calls, tokens and cost per stage as the operator's
//! summary printed them; the scan and clone judge rows of a resumed run are read from its stored
//! calls, because a replay is metered at no cost). A replacement of the engine's calls moves them;
//! re-measure from the next complete run and add it here.
//!
//! Runs made before that change are NOT here: their calls wrote about 11,000 tokens each for
//! stage 1 where today's write about 2,000 (see `CLI_CALL_OVERHEAD_TOKENS`), so the same repo cost
//! three times as much (a 26k-character repo: $39; the 26.5k-character repo below: $13). No
//! estimate of today's engine can fit both, and the quote is for today's.

use super::*;

/// One saved run: the shape the estimate is given and what each stage cost.
struct SavedRun {
    name: &'static str,
    /// What the estimate is given.
    files: usize,
    code_chars: usize,
    selected: usize,
    multi_option: usize,
    routing_groups: usize,
    /// What the run cost, by stage, in dollars (all calls, replies set aside included).
    scan: f64,
    alternatives: f64,
    detail: f64,
    calibration: f64,
    group_review: f64,
    fix: f64,
    clone_judge: f64,
    /// What the run's ledger says about its units, for the fit: the sites stage 1 calibrated, the
    /// fix stage's calls and the tokens it wrote to the cache and out, stage 2's calls and the
    /// tokens they wrote to the cache.
    stage1_sites: usize,
    fix_calls: usize,
    fix_written_tokens: usize,
    fix_output_tokens: usize,
    group_review_calls: usize,
    group_review_written_tokens: usize,
    /// Whether the fit is made from this run. The 2026-11-05 run of the 36-file repository is
    /// held out of it: its stage 2 prompt and its scan were the engine's before the R3 changes
    /// (it wrote 1,982 tokens a stage 2 call where the runs below write 1,330 to 1,400, and
    /// calibrated 102 sites where the same repository gives 80 now).
    in_fit: bool,
}

impl SavedRun {
    fn modeled(&self) -> f64 {
        self.scan
            + self.alternatives
            + self.detail
            + self.calibration
            + self.group_review
            + self.fix
            + self.clone_judge
    }
}

/// Source: a fresh cli run of a 54-file repository (1,416 counted lines), 271 calls, $14.7569.
const BIGGER_RUN: SavedRun = SavedRun {
    name: "lumen r3 prompt run, 2026-11-06",
    files: 54,
    code_chars: 51_423,
    selected: 152,
    multi_option: 34,
    routing_groups: 4,
    scan: 1.9746,
    alternatives: 0.5097,
    detail: 0.7302,
    calibration: 2.8430,
    group_review: 6.2215,
    fix: 2.4066,
    clone_judge: 0.0713,
    stage1_sites: 141,
    fix_calls: 7,
    fix_written_tokens: 143_154,
    fix_output_tokens: 63_051,
    group_review_calls: 82,
    group_review_written_tokens: 114_603,
    in_fit: true,
};

/// Source: a fresh cli run of a 36-file repository (857 counted lines) on the engine of
/// 2026-11-08 (1ada6108), 192 calls, $9.4692 (the stage table of its ledger).
const SUPABASE_RUN: SavedRun = SavedRun {
    name: "supabase portal fixture, 2026-11-08",
    files: 36,
    code_chars: 26_544,
    selected: 152,
    multi_option: 34,
    routing_groups: 4,
    scan: 1.2440,
    alternatives: 0.3458,
    detail: 0.4328,
    calibration: 1.6749,
    group_review: 4.3519,
    fix: 1.3226,
    clone_judge: 0.0972,
    stage1_sites: 80,
    fix_calls: 5,
    fix_written_tokens: 82_664,
    fix_output_tokens: 33_045,
    group_review_calls: 73,
    group_review_written_tokens: 97_078,
    in_fit: true,
};

/// Source: a fresh cli run of a 39-file repository (799 counted lines), same engine, 251 calls,
/// $12.9779. It made no clone judge call.
const FAKTURA_RUN: SavedRun = SavedRun {
    name: "faktura, 2026-11-08",
    files: 39,
    code_chars: 25_871,
    selected: 152,
    multi_option: 34,
    routing_groups: 4,
    scan: 1.3680,
    alternatives: 0.3704,
    detail: 0.6652,
    calibration: 2.3990,
    group_review: 6.0726,
    fix: 2.1027,
    clone_judge: 0.0,
    stage1_sites: 119,
    fix_calls: 8,
    fix_written_tokens: 127_179,
    fix_output_tokens: 54_225,
    group_review_calls: 85,
    group_review_written_tokens: 115_644,
    in_fit: true,
};

/// Source: a fresh cli run of a 45-file repository (1,158 counted lines), same engine, 265 calls,
/// $13.9382.
const TUTORLY_RUN: SavedRun = SavedRun {
    name: "tutorly, 2026-11-08",
    files: 45,
    code_chars: 43_524,
    selected: 152,
    multi_option: 34,
    routing_groups: 4,
    scan: 1.7555,
    alternatives: 0.4522,
    detail: 0.7374,
    calibration: 2.7740,
    group_review: 5.8466,
    fix: 2.3348,
    clone_judge: 0.0377,
    stage1_sites: 132,
    fix_calls: 7,
    fix_written_tokens: 144_722,
    fix_output_tokens: 58_817,
    group_review_calls: 86,
    group_review_written_tokens: 119_426,
    in_fit: true,
};

/// Source: a cli run of a 36-file repository (857 counted lines), 233 calls, $13.0451 (the scan
/// and clone judge rows from the run's stored calls; stage 2's include $0.2142 for two replies the
/// stage set aside).
const SMALLER_RUN: SavedRun = SavedRun {
    name: "supabase portal fixture, 2026-11-05",
    files: 36,
    code_chars: 26_544,
    selected: 152,
    multi_option: 34,
    routing_groups: 4,
    scan: 1.4355,
    alternatives: 0.3509,
    detail: 0.5432,
    calibration: 2.5338,
    group_review: 6.5450,
    fix: 1.5283,
    clone_judge: 0.1084,
    stage1_sites: 102,
    fix_calls: 6,
    fix_written_tokens: 106_719,
    fix_output_tokens: 33_709,
    group_review_calls: 76,
    group_review_written_tokens: 150_627,
    in_fit: false,
};

/// Every saved run the quote is checked against.
const ALL_RUNS: [SavedRun; 5] = [
    BIGGER_RUN,
    SMALLER_RUN,
    SUPABASE_RUN,
    FAKTURA_RUN,
    TUTORLY_RUN,
];

fn priced(id: &str, price_in: f64, price_out: f64) -> PricedModel<'_> {
    PricedModel {
        id,
        price_in,
        price_out,
    }
}

/// The models of those runs, at their list prices (US dollars per million tokens): the scan on
/// Sonnet 5.5, calibration on Opus 5.5, stage 2 on Fable 5.1, the fix stage and the clone judge on
/// Haiku 5.5, as the pre-flight resolves them.
fn estimate_of(run: &SavedRun) -> RunStageDollars {
    estimate_with(&RUN_FIT, run)
}

/// The estimate of a run under a given fit, with the shape of the run as the pre-flight has it
/// before any model call.
fn estimate_with(fit: &RunFit, run: &SavedRun) -> RunStageDollars {
    let models = RunModels {
        scan: priced("claude-sonnet-5-5", 2.0, 10.0),
        calibration: priced("claude-opus-5-5", 4.0, 20.0),
        group_review: Some(priced("claude-fable-5-1", 10.0, 50.0)),
        fix: Some(priced("claude-haiku-5-5", 0.10, 0.50)),
        clone_judge: Some(priced("claude-haiku-5-5", 0.10, 0.50)),
    };
    let repo = RunRepoInput {
        repo: "saved".to_string(),
        code_chars: run.code_chars,
        files: run.files,
        selected: run.selected,
        multi_option_selected: run.multi_option,
        routing_groups: run.routing_groups,
    };
    estimate_run_stage_dollars_with(
        fit,
        &repo,
        "parallel",
        &models,
        RunFlags {
            deep: false,
            backend: ProjectBackend::Cli,
            terse_output: true,
        },
    )
}

#[test]
fn the_estimate_is_between_one_and_one_and_a_half_times_what_each_saved_run_cost() {
    // Before the first refit the estimate read 2.48 times the bigger run and 2.66 times the
    // smaller; before the second, 1.73 times the supabase portal run of 2026-11-08, 1.26 times
    // faktura's and 1.24 times tutorly's.
    for run in ALL_RUNS {
        let estimate = estimate_of(&run).total();
        let ratio = estimate / run.modeled();
        assert!(
            (1.0..=1.5).contains(&ratio),
            "{}: estimate ${estimate:.2} against ${:.2} modeled is {ratio:.2} times",
            run.name,
            run.modeled()
        );
    }
}

/// The least a stage cost for the stage-by-stage check to hold the estimate of it to a factor of
/// two either way.
const SMALLEST_STAGE_CHECKED_USD: f64 = 0.05;

#[test]
fn no_stage_of_the_estimate_is_far_from_what_the_stage_cost() {
    // Stage by stage, so a total that is right by one stage's error cancelling another's is seen.
    // Not the 1.0 to 1.5 band of the whole (a stage can be a little under where the total is
    // carried by another), but never off by more than a factor of two either way.
    for run in ALL_RUNS {
        let name = run.name;
        let e = estimate_of(&run);
        for (stage, estimated, actual) in [
            ("scan", e.scan, run.scan),
            ("alternatives", e.alternatives, run.alternatives),
            ("detail", e.detail, run.detail),
            ("calibration", e.calibration, run.calibration),
            ("group review", e.group_review, run.group_review),
            ("fix", e.fix, run.fix),
            ("clone judge", e.clone_judge, run.clone_judge),
        ] {
            // A stage of a few cents is one call more or less (the clone judge made one call in
            // one run and two in three others, at $0.04 and $0.07 to $0.10), not a rate to hold
            // to a factor of two; its dollars are in the total, which has the 1.0 to 1.5 band.
            if actual < SMALLEST_STAGE_CHECKED_USD {
                continue;
            }
            let ratio = estimated / actual;
            assert!(
                (0.5..=2.0).contains(&ratio),
                "{name} run, {stage}: estimate ${estimated:.3} against ${actual:.3} is {ratio:.2} times"
            );
        }
    }
}

#[test]
fn the_stages_add_up_to_the_estimate_the_pre_flight_prints() {
    let run = BIGGER_RUN;
    let by_stage = estimate_of(&run).total();
    let models = RunModels {
        scan: priced("claude-sonnet-5-5", 2.0, 10.0),
        calibration: priced("claude-opus-5-5", 4.0, 20.0),
        group_review: Some(priced("claude-fable-5-1", 10.0, 50.0)),
        fix: Some(priced("claude-haiku-5-5", 0.10, 0.50)),
        clone_judge: Some(priced("claude-haiku-5-5", 0.10, 0.50)),
    };
    let repo = RunRepoInput {
        repo: "saved".to_string(),
        code_chars: run.code_chars,
        files: run.files,
        selected: run.selected,
        multi_option_selected: run.multi_option,
        routing_groups: run.routing_groups,
    };
    let bounds = estimate_run_cost_bounds(
        &repo,
        "parallel",
        &models,
        RunFlags {
            deep: false,
            backend: ProjectBackend::Cli,
            terse_output: true,
        },
    );
    assert!((bounds.expected.1 - by_stage).abs() < 1e-9);
    assert!(
        bounds.ceiling_usd >= bounds.expected.1,
        "the ceiling stays above the figure"
    );
}

// ── The fit, and the check that it holds for a run it was not made from ────────────────────────

/// The middle value of the rates (the mean of the two middle ones when there is an even number).
fn median(mut rates: Vec<f64>) -> f64 {
    rates.sort_by(f64::total_cmp);
    let mid = rates.len() / 2;
    if rates.len().is_multiple_of(2) {
        (rates[mid - 1] + rates[mid]) / 2.0
    } else {
        rates[mid]
    }
}

/// The fitted constants a set of saved runs gives. This is the procedure `RUN_FIT` was made with:
/// the tokens a stage writes per unit are the LARGEST its runs showed, so the quote is not under
/// any of them; the sites per file are the MEDIAN, because they differ between repositories of one
/// size by more than the whole 1.0 to 1.5 band (80 sites for 36 files, 119 for 39) and the largest
/// would quote the first of those 1.5 times what it cost.
fn fit_from(runs: &[&SavedRun]) -> RunFit {
    let largest = |of: &dyn Fn(&SavedRun) -> f64| -> f64 {
        runs.iter().map(|r| of(r)).fold(f64::MIN, f64::max)
    };
    RunFit {
        sites_per_file: median(
            runs.iter()
                .map(|r| r.stage1_sites as f64 / r.files as f64)
                .collect(),
        ),
        // What the fix stage wrote to the cache beyond the CLI's own prompt of each call.
        fix_cli_input_tokens_per_site: largest(&|r| {
            (r.fix_written_tokens as f64 - r.fix_calls as f64 * CLI_CALL_OVERHEAD_TOKENS)
                / r.stage1_sites as f64
        }),
        fix_cli_output_tokens_per_site: largest(&|r| {
            r.fix_output_tokens as f64 / r.stage1_sites as f64
        }),
        // What a stage 2 call wrote beyond its site lines.
        group_review_cli_tokens_per_call: largest(&|r| {
            (r.group_review_written_tokens as f64
                - GROUP_REVIEW_INPUT_TOKENS_PER_SITE * r.stage1_sites as f64)
                / r.group_review_calls as f64
        }),
    }
}

fn fit_runs() -> Vec<&'static SavedRun> {
    // The consts are promoted to statics so the borrows live long enough.
    static RUNS: [SavedRun; 5] = ALL_RUNS;
    RUNS.iter().filter(|r| r.in_fit).collect()
}

#[test]
fn the_fit_is_not_under_any_run_it_was_made_from_and_not_far_over() {
    let fit = fit_from(&fit_runs());
    for (name, used, measured) in [
        ("sites per file", RUN_FIT.sites_per_file, fit.sites_per_file),
        (
            "fix input tokens per site",
            RUN_FIT.fix_cli_input_tokens_per_site,
            fit.fix_cli_input_tokens_per_site,
        ),
        (
            "fix output tokens per site",
            RUN_FIT.fix_cli_output_tokens_per_site,
            fit.fix_cli_output_tokens_per_site,
        ),
        (
            "stage 2 tokens per call",
            RUN_FIT.group_review_cli_tokens_per_call,
            fit.group_review_cli_tokens_per_call,
        ),
    ] {
        assert!(
            used >= measured,
            "{name}: the estimate uses {used}, a saved run showed {measured}"
        );
        assert!(
            used <= measured * 1.05,
            "{name}: the estimate uses {used}, the largest saved run showed {measured}"
        );
    }
}

/// The check that the fit is not just the runs it was made from: leave each run out, fit from the
/// others, and the estimate for the run left out still lands between 1.0 and 1.5 times what it cost.
#[test]
fn leaving_any_run_out_of_the_fit_the_estimate_for_it_is_still_within_one_to_one_and_a_half() {
    let runs = fit_runs();
    assert!(runs.len() >= 4);
    for held_out in &runs {
        let others: Vec<&SavedRun> = runs
            .iter()
            .copied()
            .filter(|r| r.name != held_out.name)
            .collect();
        let fit = fit_from(&others);
        let estimate = estimate_with(&fit, held_out).total();
        let ratio = estimate / held_out.modeled();
        eprintln!(
            "leave-one-out {}: ${estimate:.2} against ${:.2} is {ratio:.2} times",
            held_out.name,
            held_out.modeled()
        );
        assert!(
            (1.0..=1.5).contains(&ratio),
            "{} left out of the fit: estimate ${estimate:.2} against ${:.2} modeled is {ratio:.2} times",
            held_out.name,
            held_out.modeled()
        );
    }
}

/// Two repositories of the same code size are not the same run: the quote follows the files the
/// scan reads (the sites stage 1 calibrates, and so every stage after it), and a repository of
/// which only the code size is known is quoted on that, as before.
#[test]
fn the_estimate_follows_the_files_of_the_repository() {
    let repo = |files| RunRepoInput {
        repo: "saved".to_string(),
        code_chars: 26_544,
        files,
        selected: 152,
        multi_option_selected: 34,
        routing_groups: 4,
    };
    let models = RunModels {
        scan: priced("claude-sonnet-5-5", 2.0, 10.0),
        calibration: priced("claude-opus-5-5", 4.0, 20.0),
        group_review: Some(priced("claude-fable-5-1", 10.0, 50.0)),
        fix: Some(priced("claude-haiku-5-5", 0.10, 0.50)),
        clone_judge: Some(priced("claude-haiku-5-5", 0.10, 0.50)),
    };
    let flags = RunFlags {
        deep: false,
        backend: ProjectBackend::Cli,
        terse_output: true,
    };
    let usd = |files| {
        estimate_run_cost_bounds(&repo(files), "parallel", &models, flags)
            .expected
            .1
    };
    assert!(
        usd(20) < usd(36),
        "fewer files, fewer sites, a smaller quote"
    );
    assert!(usd(36) < usd(60), "more files, more sites, a larger quote");
    // Sites a repository of 36 files is quoted: its files times the fitted rate.
    assert_eq!(
        estimated_sites(&repo(36), "parallel", ProjectBackend::Cli),
        (36.0 * RUN_FIT.sites_per_file).ceil() as usize
    );
    // No file count: the sites are sized from the code, as they were.
    assert!(estimated_sites(&repo(0), "parallel", ProjectBackend::Cli) > 100);
}

/// The API backend keeps the sizing it had: no saved API run records its sites, so the files are
/// not used for it and its figures do not move with this fit. The two recorded API runs of the
/// supabase portal fixture (Haiku 5.5 $0.414, Sonnet 5.5 $1.543) are matched within a quarter, as
/// they were, whether or not the estimate is given the fixture's 36 files.
#[test]
fn the_api_estimate_does_not_move_with_the_files_and_still_matches_both_recorded_api_runs() {
    let repo = |files| RunRepoInput {
        repo: "supabase-portal-fixture".to_string(),
        code_chars: 26_544,
        files,
        selected: 152,
        multi_option_selected: 34,
        routing_groups: 4,
    };
    let flags = RunFlags {
        deep: false,
        backend: ProjectBackend::Api,
        terse_output: true,
    };
    let judge = priced("claude-haiku-4-5-20251001", 1.0, 5.0);
    let usd = |model, files| {
        let models = RunModels {
            scan: model,
            calibration: model,
            group_review: Some(model),
            fix: Some(priced("claude-haiku-5-5", 0.10, 0.50)),
            clone_judge: Some(judge),
        };
        estimate_run_cost_bounds(&repo(files), "parallel", &models, flags)
            .expected
            .1
    };
    let sonnet = priced("claude-sonnet-5-5", 2.0, 10.0);
    let haiku = priced("claude-haiku-5-5", 0.10, 0.50);
    for model in [sonnet, haiku] {
        assert_eq!(usd(model, 36), usd(model, 0), "{}", model.id);
    }
    // The recorded runs had no stage 2: price the stages they had.
    let without_stage_2 = |model: PricedModel<'_>| {
        let models = RunModels {
            scan: model,
            calibration: model,
            group_review: None,
            fix: Some(priced("claude-haiku-5-5", 0.10, 0.50)),
            clone_judge: Some(judge),
        };
        estimate_run_cost_bounds(&repo(36), "parallel", &models, flags)
            .expected
            .1
    };
    let (sonnet_usd, haiku_usd) = (without_stage_2(sonnet), without_stage_2(haiku));
    assert!(
        (sonnet_usd - 1.543).abs() <= 1.543 * 0.25,
        "Sonnet 5.5: estimated ${sonnet_usd:.3} against $1.543 spent"
    );
    assert!(
        (haiku_usd - 0.414).abs() <= 0.414 * 0.25,
        "Haiku 5.5: estimated ${haiku_usd:.3} against $0.414 spent"
    );
}
