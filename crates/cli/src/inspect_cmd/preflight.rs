//! The pre-flight cost estimate `camerata inspect` prints BEFORE its first model call.
//!
//! The arithmetic is `camerata_ui_core::scan::estimate_audit_cost`, the estimator the cockpit
//! shows; this module only feeds it the facts the scan will actually use
//! ([`camerata_server::preflight`]) and words the result. It is an ESTIMATE and says so, with
//! its basis, because a number shown before a spend decision that reads like a quote is worse
//! than none.

use camerata_server::llm::ProjectBackend;
use camerata_server::preflight::{ExactSample, RepoScanFacts};
use camerata_ui_core::scan::{estimate_audit_cost, human_tokens};

/// Everything the wording needs.
pub struct PreflightInputs<'a> {
    pub backend: ProjectBackend,
    pub batch: bool,
    /// The resolved audit model and calibration model ids.
    pub model: String,
    pub calibration_model: String,
    pub repos: &'a [RepoScanFacts],
    /// A previous scan's cache will be tried (the estimate prices a full scan; a rerun costs less).
    pub incremental: bool,
    pub cap_usd: Option<f64>,
    /// The exact size of one scan pass, when the API backend could count it.
    pub exact: Option<ExactSample>,
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
    let estimate = prices.map(|((a_in, a_out), (c_in, c_out))| {
        let mut tokens = 0u64;
        let mut dollars = 0.0f64;
        let mut passes = 0usize;
        for repo in inputs.repos {
            let (t, d, p) = estimate_audit_cost(
                repo.code_chars,
                repo.semantic.len(),
                mode,
                a_in,
                a_out,
                c_in,
                c_out,
                false,
                inputs.incremental,
                false,
                inputs.backend,
                repo.multi_option_rules,
            );
            tokens += t;
            dollars += d;
            passes += p;
        }
        (tokens, dollars, passes)
    });

    let mut out = String::new();
    writeln!(
        out,
        "== PRE-FLIGHT ESTIMATE (printed before any model call; an estimate, not a quote) =="
    )
    .ok();
    match estimate {
        Some((tokens, dollars, passes)) => {
            writeln!(
                out,
                "  estimated cost: about ${dollars:.2} ({} tokens over about {passes} model calls)",
                human_tokens(tokens)
            )
            .ok();
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
            if let Some((_, dollars, _)) = estimate {
                if dollars > cap {
                    writeln!(
                        out,
                        "  WARNING: the estimate is above the spend limit, so this run is expected to stop \
                         before it finishes and its export will be marked incomplete."
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
        estimate_usd: estimate.map(|(_, d, _)| d),
    }
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
        }
    }

    fn inputs<'a>(repos: &'a [RepoScanFacts], cap: Option<f64>) -> PreflightInputs<'a> {
        PreflightInputs {
            backend: ProjectBackend::Api,
            batch: false,
            model: "claude-sonnet-5".into(),
            calibration_model: "claude-sonnet-5".into(),
            repos,
            incremental: false,
            cap_usd: cap,
            exact: None,
        }
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

    #[test]
    fn batch_mode_explains_the_check_happens_before_submitting() {
        let repos = [repo(1_000, 3)];
        let mut batch = inputs(&repos, Some(5.0));
        batch.batch = true;
        assert!(build(&batch).text.contains("cannot be cancelled cheaply"));
    }
}
