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
    /// What the estimate is given.
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
};

/// Source: a cli run of a 36-file repository (857 counted lines), 233 calls, $13.0451 (the scan
/// and clone judge rows from the run's stored calls; stage 2's include $0.2142 for two replies the
/// stage set aside).
const SMALLER_RUN: SavedRun = SavedRun {
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
};

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
        selected: run.selected,
        multi_option_selected: run.multi_option,
        routing_groups: run.routing_groups,
    };
    estimate_run_stage_dollars(
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
    // Before the refit the estimate read 2.48 times the bigger run and 2.66 times the smaller.
    for (name, run) in [("bigger", BIGGER_RUN), ("smaller", SMALLER_RUN)] {
        let estimate = estimate_of(&run).total();
        let ratio = estimate / run.modeled();
        assert!(
            (1.0..=1.5).contains(&ratio),
            "{name} run: estimate ${estimate:.2} against ${:.2} modeled is {ratio:.2} times",
            run.modeled()
        );
    }
}

#[test]
fn no_stage_of_the_estimate_is_far_from_what_the_stage_cost() {
    // Stage by stage, so a total that is right by one stage's error cancelling another's is seen.
    // Not the 1.0 to 1.5 band of the whole (a stage can be a little under where the total is
    // carried by another), but never off by more than a factor of two either way.
    for (name, run) in [("bigger", BIGGER_RUN), ("smaller", SMALLER_RUN)] {
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
