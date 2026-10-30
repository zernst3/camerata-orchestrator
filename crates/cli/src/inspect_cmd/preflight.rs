//! The pre-flight cost estimate `camerata inspect` prints BEFORE its first model call.
//!
//! The arithmetic is `camerata_ui_core::scan::estimate_audit_cost`, the estimator the cockpit
//! shows; this module only feeds it the facts the scan will actually use
//! ([`camerata_server::preflight`]) and words the result. It is an ESTIMATE and says so, with
//! its basis, because a number shown before a spend decision that reads like a quote is worse
//! than none.

use camerata_server::llm::ProjectBackend;
use camerata_server::preflight::{ExactSample, RepoScanFacts};
use camerata_ui_core::scan::{
    estimate_run_cost_by_repo, human_tokens, PricedModel, RepoEstimate, RunFlags, RunModels,
    RunRepoInput,
};

/// Everything the wording needs.
pub struct PreflightInputs<'a> {
    pub backend: ProjectBackend,
    pub batch: bool,
    /// The resolved audit model and calibration model ids.
    pub model: String,
    pub calibration_model: String,
    /// The model the fix stage will call (`None` when it does not run). Priced in the figure.
    pub fix_model: Option<String>,
    /// The clone judge's model, when a clone rule is selected (`None` otherwise). Priced too.
    pub clone_judge_model: Option<String>,
    pub repos: &'a [RepoScanFacts],
    /// A previous scan's cache will be tried (the estimate prices a full scan; a rerun costs less).
    pub incremental: bool,
    pub cap_usd: Option<f64>,
    /// The exact size of one scan pass, when the API backend could count it.
    pub exact: Option<ExactSample>,
    /// The `terse_output` measure is on: the full detail of each surviving finding is written by
    /// a separate step after the scan, which the estimate then includes.
    pub terse_output: bool,
    /// The three-pass consensus option is on: the whole AI audit runs
    /// `camerata_server::consensus::CONSENSUS_PASSES` times.
    pub consensus: bool,
}

/// The wording, and the number the batch spend-cap gate is given.
pub struct Preflight {
    pub text: String,
    /// The estimated dollars for the whole run; `None` when a model is unpriced.
    pub estimate_usd: Option<f64>,
}

fn price_of(model: &str) -> Option<(f64, f64)> {
    camerata_server::llm::models()
        .into_iter()
        .find(|m| m.id == model)
        .map(|m| (m.price_in, m.price_out))
}

pub fn build(inputs: &PreflightInputs<'_>) -> Preflight {
    use std::fmt::Write as _;
    let mode = if inputs.batch { "batch" } else { "parallel" };
    let backend = match inputs.backend {
        ProjectBackend::Api => "api",
        ProjectBackend::Cli => "cli",
    };
    let code_chars: usize = inputs.repos.iter().map(|r| r.code_chars).sum();
    let files: usize = inputs.repos.iter().map(|r| r.files.len()).sum();
    let rules: usize = inputs
        .repos
        .iter()
        .map(|r| r.semantic.len())
        .max()
        .unwrap_or(0);
    let multi: usize = inputs
        .repos
        .iter()
        .map(|r| r.multi_option_rules)
        .max()
        .unwrap_or(0);

    let prices = price_of(&inputs.model).zip(price_of(&inputs.calibration_model));
    // One estimate per repo, summed: the scan reads and chunks each repo on its own. A repo
    // that could not be read counts nothing and is named in the output instead. The rule router
    // splits each repo into groups (the rules of one scope share one file subset), and each
    // group has its own rule batches, so the pass count follows the group count.
    let est_inputs: Vec<RunRepoInput> = inputs
        .repos
        .iter()
        .filter(|r| r.read_error.is_none())
        .map(|r| RunRepoInput {
            repo: r.repo.clone(),
            code_chars: r.code_chars,
            selected: r.semantic.len(),
            multi_option_selected: r.multi_option_rules,
            routing_groups: camerata_server::scan_routing::plan_routes(
                &camerata_server::scan_routing::rules_from_pairs(&r.semantic),
                &r.files,
                true,
            )
            .groups
            .len()
            .max(1),
        })
        .collect();
    // Consensus runs the whole AI audit several times over the same inputs.
    let runs = if inputs.consensus {
        camerata_server::consensus::CONSENSUS_PASSES
    } else {
        1
    };
    // The fix stage's model and the clone judge's, when the run uses them. A model with no list
    // price leaves its stage out of the figure (the figure says when a price is missing).
    let fix_price = inputs
        .fix_model
        .as_deref()
        .and_then(|id| price_of(id).map(|(price_in, price_out)| (id, price_in, price_out)));
    let judge_price = inputs
        .clone_judge_model
        .as_deref()
        .and_then(|id| price_of(id).map(|(price_in, price_out)| (id, price_in, price_out)));
    let estimate = prices.map(|((a_in, a_out), (c_in, c_out))| {
        let piece = |id, price_in, price_out| PricedModel {
            id,
            price_in,
            price_out,
        };
        let models = RunModels {
            scan: piece(inputs.model.as_str(), a_in, a_out),
            calibration: piece(inputs.calibration_model.as_str(), c_in, c_out),
            fix: fix_price.map(|(id, i, o)| piece(id, i, o)),
            clone_judge: judge_price.map(|(id, i, o)| piece(id, i, o)),
        };
        let e = estimate_run_cost_by_repo(
            &est_inputs,
            mode,
            &models,
            RunFlags {
                thorough: false,
                deep: false,
                backend: inputs.backend,
                terse_output: inputs.terse_output,
            },
        );
        let rows: Vec<RepoEstimate> = e
            .rows
            .into_iter()
            .map(|r| RepoEstimate {
                tokens: r.tokens * runs as u64,
                dollars: r.dollars * runs as f64,
                passes: r.passes * runs,
                ceiling_usd: r.ceiling_usd * runs as f64,
                ..r
            })
            .collect();
        (
            e.tokens * runs as u64,
            e.dollars * runs as f64,
            e.passes * runs,
            rows,
            e.ceiling_usd * runs as f64,
        )
    });

    let mut out = String::new();
    writeln!(
        out,
        "== PRE-FLIGHT ESTIMATE (printed before any model call; an estimate, not a quote) =="
    )
    .ok();
    match estimate {
        Some((tokens, dollars, passes, _, ceiling)) => {
            writeln!(
                out,
                "  estimated cost: about ${dollars:.2} ({} tokens over about {passes} model calls)",
                human_tokens(tokens)
            )
            .ok();
            if ceiling_is_above(dollars, ceiling) {
                writeln!(
                    out,
                    "  at most ${ceiling:.2} if the detail step needs every repeat round on every \
                     finding (the usual run pays for its first round only)"
                )
                .ok();
            }
        }
        None => {
            writeln!(
                out,
                "  estimated cost: unavailable (no list price is known for model {} / {})",
                inputs.model, inputs.calibration_model
            )
            .ok();
        }
    }
    writeln!(
        out,
        "  basis: {code_chars} characters of code in {files} file(s), {rules} rule(s) checked by the \
         model ({multi} with alternatives), audit model {}, calibration model {}, {mode} mode, {backend} \
         backend; characters-per-token heuristic with prompt-cache pricing modeled",
        inputs.model, inputs.calibration_model
    )
    .ok();
    if inputs.repos.len() > 1 {
        writeln!(out, "  per repo:").ok();
        for repo in inputs.repos {
            match (&repo.read_error, estimate.as_ref()) {
                (Some(why), _) => {
                    writeln!(
                        out,
                        "    {}: could not be read ({why}); it is NOT in this estimate",
                        repo.repo
                    )
                    .ok();
                }
                (None, Some((_, _, _, rows, _))) => {
                    if let Some(row) = rows.iter().find(|r| r.repo == repo.repo) {
                        writeln!(
                            out,
                            "    {}: {} characters of code in {} file(s), {} rule(s), about ${:.2} ({} model calls)",
                            repo.repo,
                            repo.code_chars,
                            repo.files.len(),
                            repo.semantic.len(),
                            row.dollars,
                            row.passes
                        )
                        .ok();
                    }
                }
                (None, None) => {
                    writeln!(
                        out,
                        "    {}: {} characters of code in {} file(s), {} rule(s)",
                        repo.repo,
                        repo.code_chars,
                        repo.files.len(),
                        repo.semantic.len()
                    )
                    .ok();
                }
            }
        }
    }
    if inputs.terse_output {
        writeln!(
            out,
            "  includes the detail regeneration step (an upper bound); rule routing by declared scope is not modeled, so a scan that skips rules costs less"
        )
        .ok();
    }
    match (&inputs.exact, inputs.backend) {
        (Some(exact), _) => {
            writeln!(
                out,
                "  exact: one scan pass is {} input tokens (counted by the API, not billed); the scan makes \
                 about {} such passes",
                exact.input_tokens, exact.passes
            )
            .ok();
        }
        (None, ProjectBackend::Api) => {
            writeln!(
                out,
                "  exact: the API token count of one scan pass was unavailable; the heuristic above stands alone"
            )
            .ok();
        }
        (None, ProjectBackend::Cli) => {
            writeln!(
                out,
                "  exact: not available on the cli backend (token counts come from the API)"
            )
            .ok();
        }
    }
    if inputs.consensus {
        writeln!(
            out,
            "  consensus: on. The AI audit runs {} times and keeps what {} runs agree on; the \
             figures above include every run.",
            camerata_server::consensus::CONSENSUS_PASSES,
            camerata_server::consensus::CONSENSUS_QUORUM
        )
        .ok();
    }
    if inputs.incremental {
        writeln!(
            out,
            "  note: a previous scan's cache exists; unchanged files reuse its findings, so this run can cost less"
        )
        .ok();
    }
    match inputs.cap_usd {
        Some(cap) => {
            writeln!(
                out,
                "  spend limit: ${cap:.2}. The run stops between passes once actual spend reaches it \
                 and says what was not run; passes already started finish, so it can end about one \
                 pass over the limit."
            )
            .ok();
            if let Some((_, dollars, _, _, ceiling)) = estimate {
                if dollars > cap {
                    writeln!(
                        out,
                        "  WARNING: the estimate is above the spend limit, so this run is expected to stop \
                         before it finishes and its export will be marked incomplete."
                    )
                    .ok();
                } else if ceiling > cap {
                    writeln!(
                        out,
                        "  note: the worst case (${ceiling:.2}) is above the spend limit. The run is \
                         expected to stay under it; if the detail step needs its repeat rounds, the \
                         run stops at the limit and says what was not run."
                    )
                    .ok();
                }
            }
            if inputs.batch {
                writeln!(
                    out,
                    "  batch mode: a submitted batch cannot be cancelled cheaply, so the limit is checked \
                     BEFORE submitting, against the estimate and the exact input size."
                )
                .ok();
            }
        }
        None => {
            writeln!(out, "  spend limit: none (set one with --max-cost-usd)").ok();
        }
    }
    Preflight {
        text: out,
        estimate_usd: estimate.map(|(_, d, _, _, _)| d),
    }
}

/// Whether the ceiling is worth a line of its own: more than a cent above the expected figure.
fn ceiling_is_above(expected: f64, ceiling: f64) -> bool {
    ceiling - expected > 0.005
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(chars: usize, rules: usize) -> RepoScanFacts {
        RepoScanFacts {
            repo: "acme/api".into(),
            files: vec![("a.rs".into(), "x".repeat(chars))],
            code_chars: chars,
            semantic: (0..rules)
                .map(|i| (format!("R-{i}"), "do it".to_string()))
                .collect(),
            multi_option_rules: 2,
            read_error: None,
        }
    }

    fn named(name: &str, chars: usize, rules: usize) -> RepoScanFacts {
        RepoScanFacts {
            repo: name.into(),
            ..repo(chars, rules)
        }
    }

    fn inputs<'a>(repos: &'a [RepoScanFacts], cap: Option<f64>) -> PreflightInputs<'a> {
        PreflightInputs {
            backend: ProjectBackend::Api,
            batch: false,
            model: "claude-sonnet-5".into(),
            calibration_model: "claude-sonnet-5".into(),
            fix_model: None,
            clone_judge_model: None,
            repos,
            incremental: false,
            cap_usd: cap,
            exact: None,
            terse_output: false,
            consensus: false,
        }
    }

    /// The operator reads the expected cost; the ceiling (every regeneration round on every
    /// finding) is shown beside it as "at most". The spend-limit WARNING is about the expected
    /// cost; a limit between the two gets a note about the worst case instead, and the number the
    /// batch gate is given is the expected one.
    #[test]
    fn the_preflight_shows_the_expected_cost_with_the_ceiling_beside_it() {
        let repos = [repo(400_000, 30)];
        let mut on = inputs(&repos, None);
        on.terse_output = true;
        let p = build(&on);
        assert!(p.text.contains("at most $"), "{}", p.text);
        let expected = p.estimate_usd.unwrap();

        let mut between = inputs(&repos, Some(expected + 1.0));
        between.terse_output = true;
        let p = build(&between);
        assert!(
            !p.text
                .contains("WARNING: the estimate is above the spend limit"),
            "{}",
            p.text
        );
        assert!(p.text.contains("worst case"), "{}", p.text);
        assert_eq!(
            p.estimate_usd.unwrap(),
            expected,
            "the gate's figure is the expected cost"
        );

        // With the stage off the two figures are one and nothing says "at most".
        let p = build(&inputs(&repos, None));
        assert!(!p.text.contains("at most $"), "{}", p.text);
    }

    #[test]
    fn consensus_triples_the_estimate_and_says_so() {
        let repos = [repo(400_000, 30)];
        let single = build(&inputs(&repos, None));
        let mut on = inputs(&repos, None);
        on.consensus = true;
        let triple = build(&on);
        let (one, three) = (single.estimate_usd.unwrap(), triple.estimate_usd.unwrap());
        assert!((three - 3.0 * one).abs() < 1e-9, "{one} x3 != {three}");
        assert!(triple.text.contains("consensus: on"), "{}", triple.text);
        assert!(!single.text.contains("consensus"), "{}", single.text);
    }

    #[test]
    fn it_states_an_estimate_with_its_basis_and_no_em_dash() {
        let repos = [repo(400_000, 30)];
        let p = build(&inputs(&repos, None));
        assert!(p.estimate_usd.is_some_and(|d| d > 0.0), "{}", p.text);
        assert!(p.text.contains("PRE-FLIGHT ESTIMATE"));
        assert!(p.text.contains("an estimate, not a quote"));
        assert!(p.text.contains("400000 characters of code"), "{}", p.text);
        assert!(p.text.contains("30 rule(s)") && p.text.contains("2 with alternatives"));
        assert!(p.text.contains("spend limit: none"));
        assert!(
            !p.text.contains('\u{2014}'),
            "no em-dashes in user-facing text"
        );
    }

    #[test]
    fn the_exact_count_is_shown_when_the_api_gave_one_and_its_absence_is_stated() {
        let repos = [repo(1_000, 3)];
        let mut with = inputs(&repos, None);
        with.exact = Some(ExactSample {
            input_tokens: 41_203,
            passes: 7,
        });
        let text = build(&with).text;
        assert!(
            text.contains("41203 input tokens") && text.contains("about 7 such passes"),
            "{text}"
        );

        let without = build(&inputs(&repos, None)).text;
        assert!(without.contains("was unavailable"), "{without}");

        let mut cli = inputs(&repos, None);
        cli.backend = ProjectBackend::Cli;
        assert!(build(&cli)
            .text
            .contains("not available on the cli backend"));
    }

    #[test]
    fn a_cap_below_the_estimate_warns_and_the_bound_is_stated() {
        let repos = [repo(400_000, 30)];
        let p = build(&inputs(&repos, Some(0.01)));
        assert!(p.text.contains("spend limit: $0.01"), "{}", p.text);
        assert!(p.text.contains("about one pass over the limit"));
        assert!(p.text.contains("WARNING"), "{}", p.text);
        let roomy = build(&inputs(&repos, Some(10_000.0))).text;
        assert!(!roomy.contains("WARNING"), "{roomy}");
    }

    #[test]
    fn an_unpriced_model_says_so_instead_of_printing_zero() {
        let repos = [repo(1_000, 3)];
        let mut unpriced = inputs(&repos, None);
        unpriced.model = "some-unlisted-model".into();
        let p = build(&unpriced);
        assert!(p.estimate_usd.is_none());
        assert!(p.text.contains("unavailable (no list price"), "{}", p.text);
    }

    /// Rank 4: with `terse_output` on the estimate (and so the hint the batch spend-limit gate
    /// is given) includes the detail-regeneration step; with it off it does not.
    #[test]
    fn the_estimate_includes_detail_regeneration_only_with_terse_output_on() {
        let repos = [repo(400_000, 30)];
        let off = build(&inputs(&repos, None));
        let mut on_inputs = inputs(&repos, None);
        on_inputs.terse_output = true;
        let on = build(&on_inputs);
        let (off_usd, on_usd) = (off.estimate_usd.unwrap(), on.estimate_usd.unwrap());
        assert!(on_usd > off_usd, "on {on_usd} must exceed off {off_usd}");
        assert!(on.text.contains("detail regeneration"), "{}", on.text);
        assert!(!off.text.contains("detail regeneration"), "{}", off.text);
        assert!(
            !on.text.contains('\u{2014}'),
            "no em-dashes in user-facing text"
        );
    }

    #[test]
    fn batch_mode_explains_the_check_happens_before_submitting() {
        let repos = [repo(1_000, 3)];
        let mut batch = inputs(&repos, Some(5.0));
        batch.batch = true;
        assert!(build(&batch).text.contains("cannot be cancelled cheaply"));
    }

    /// M1: a project of several repos is priced per repo (each is read and chunked on its
    /// own), with one row per repo whose dollars add up to the headline figure.
    #[test]
    fn several_repos_get_one_row_each_and_the_rows_sum_to_the_estimate() {
        let repos = [
            named("acme/api", 360_000, 10),
            named("acme/web", 360_000, 10),
            named("acme/jobs", 20_000, 10),
        ];
        let p = build(&inputs(&repos, None));
        assert!(p.text.contains("per repo:"), "{}", p.text);
        for name in ["acme/api", "acme/web", "acme/jobs"] {
            assert!(p.text.contains(&format!("    {name}: ")), "{}", p.text);
        }
        let row_total: f64 = p
            .text
            .lines()
            .filter(|l| l.starts_with("    acme/"))
            .map(|l| {
                let after = l.split("about $").nth(1).expect("a dollar figure");
                after.split(' ').next().unwrap().parse::<f64>().unwrap()
            })
            .sum();
        let total = p.estimate_usd.unwrap();
        assert!(
            (row_total - total).abs() < 0.02,
            "rows {row_total} vs {total}"
        );
        // And it is more than pricing the same code as one stream.
        let merged = [named("acme/all", 740_000, 10)];
        assert!(total > build(&inputs(&merged, None)).estimate_usd.unwrap());
    }

    #[test]
    fn a_single_repo_prints_no_per_repo_block() {
        let repos = [repo(1_000, 3)];
        assert!(!build(&inputs(&repos, None)).text.contains("per repo:"));
    }

    /// M1: an unreadable repo is named in the estimate, counts nothing, and never reads as free.
    #[test]
    fn an_unreadable_repo_is_named_and_left_out_of_the_total() {
        let mut gone = named("acme/gone", 0, 0);
        gone.read_error = Some("no such folder".into());
        let repos = [named("acme/api", 360_000, 10), gone];
        let p = build(&inputs(&repos, None));
        assert!(
            p.text.contains(
                "acme/gone: could not be read (no such folder); it is NOT in this estimate"
            ),
            "{}",
            p.text
        );
        let alone = [named("acme/api", 360_000, 10)];
        let (with, without) = (
            p.estimate_usd.unwrap(),
            build(&inputs(&alone, None)).estimate_usd.unwrap(),
        );
        assert!((with - without).abs() < 1e-9, "{with} vs {without}");
    }

    /// The figure counts what the run does: the fix stage (on its own model) and the clone judge
    /// are priced when the run uses them, and left out when it does not.
    #[test]
    fn the_fix_stage_and_the_clone_judge_are_in_the_figure_when_the_run_uses_them() {
        let repos = [repo(26_544, 152)];
        let mut bare = inputs(&repos, None);
        bare.model = "claude-sonnet-5-5".into();
        bare.calibration_model = "claude-sonnet-5-5".into();
        bare.terse_output = true;
        let without = build(&bare).estimate_usd.unwrap();

        let mut full = inputs(&repos, None);
        full.model = "claude-sonnet-5-5".into();
        full.calibration_model = "claude-sonnet-5-5".into();
        full.terse_output = true;
        full.fix_model = Some("claude-haiku-5-5".into());
        full.clone_judge_model = Some("claude-haiku-4-5-20251001".into());
        let with = build(&full).estimate_usd.unwrap();
        assert!(
            with > without + 0.03,
            "fix and judge are priced: {without} -> {with}"
        );
        // A fix model with no list price is left out of the figure, not guessed at.
        full.fix_model = Some("some-model-without-a-price".into());
        full.clone_judge_model = None;
        let unpriced = build(&full).estimate_usd.unwrap();
        assert!((unpriced - without).abs() < 1e-9, "{unpriced} vs {without}");
    }
}
