//! `camerata-check` CLI — see `crates/camerata-check/src/lib.rs` for the design rationale
//! (Layer-3 CI parity, `docs/design/2026-07-26_architectural-executor-feasibility.md` §2.4)
//! and `crates/camerata-check/README.md` for the CI invocation.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, ValueEnum};

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "lowercase")]
enum Format {
    Human,
    Json,
}

/// Standalone CI runner for Camerata's native, deterministic architectural checkers.
///
/// Runs the same checker registry the Camerata-hosted scan and Layer-2 governed-dev loop run
/// (`camerata_checks::arch_checker::all_checkers`), with no dependency on any Camerata server
/// or network call. Exit code: 0 when clean, 1 when a deterministic violation is found, 2 on a
/// run error (bad `--config` path, etc.) — standard CI gate semantics.
#[derive(Parser, Debug)]
#[command(name = "camerata-check", version, about, long_about = None)]
struct Cli {
    /// Repo (or worktree) path to scan.
    #[arg(default_value = ".")]
    path: PathBuf,

    /// Output format.
    #[arg(long, value_enum, default_value_t = Format::Human)]
    format: Format,

    /// Use this file as `.camerata/architecture.toml` instead of (or in addition to — this
    /// wins) whatever `<path>/.camerata/architecture.toml` the walk finds.
    #[arg(long)]
    config: Option<PathBuf>,

    /// Restrict to this corpus rule id. Repeatable. Omit to run every registered checker.
    #[arg(long = "rule-id")]
    rule_id: Vec<String>,

    /// Also fail the build on needs-review-grade findings (advisory-coexisting checkers, or a
    /// checker's own needs-review-tier fallback verdict). Off by default: a CI gate should only
    /// hard-fail on deterministic, hard verdicts.
    #[arg(long)]
    strict: bool,
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();

    let corpus_dir = camerata_rules::corpus_path();
    // Best-effort and lenient: an unreachable corpus is never fatal, and one malformed rule file
    // must not disable every bound. A skipped file is named on stderr, the loaded rules' bounds
    // still apply, and the report says the corpus was not fully applied (corpus_load_errors).
    let (corpus, corpus_load_errors) = load_corpus_for_gate(&corpus_dir).await;
    warn_on_unknown_corpus_rule_ids(&cli.rule_id, corpus.as_ref(), &corpus_dir);

    let opts = camerata_check::Options {
        repo_root: cli.path.clone(),
        config_override: cli.config.clone(),
        rule_ids: cli.rule_id.clone(),
        corpus,
        corpus_load_errors,
    };

    let report = match camerata_check::run(&opts) {
        Ok(report) => report,
        Err(e) => {
            eprintln!("camerata-check: error: {e:#}");
            return ExitCode::from(camerata_check::EXIT_CODE_RUN_ERROR as u8);
        }
    };

    match cli.format {
        Format::Human => print!("{}", camerata_check::render_human_for(&report, cli.strict)),
        Format::Json => match camerata_check::render_json(&report) {
            Ok(json) => println!("{json}"),
            Err(e) => {
                eprintln!("camerata-check: error rendering JSON: {e:#}");
                return ExitCode::from(camerata_check::EXIT_CODE_RUN_ERROR as u8);
            }
        },
    }

    match report.exit_code(cli.strict) {
        0 => ExitCode::SUCCESS,
        _ => ExitCode::FAILURE,
    }
}

/// Load the corpus leniently. `None` when nothing loaded at all (no corpus directory, or every
/// file malformed); otherwise the rules that did load plus the number of files that were
/// skipped, each reported on stderr with its path and error.
async fn load_corpus_for_gate(
    corpus_dir: &std::path::Path,
) -> (Option<camerata_rules::RuleSet>, usize) {
    let (set, errors) = camerata_rules::load_corpus_lenient(corpus_dir).await;
    for (path, err) in &errors {
        eprintln!(
            "camerata-check: warning: skipped corpus file {}: {err}",
            path.display()
        );
    }
    if set.is_empty() && !errors.is_empty() {
        return (None, errors.len());
    }
    (Some(set), errors.len())
}

/// Best-effort, non-fatal: if the rule corpus loaded (it does on any machine that has this repo
/// checked out, or with `CAMERATA_CORPUS_PATH` set; it usually does NOT on a bare client CI
/// runner holding only a vendored binary), warn about any `--rule-id` value that isn't a real
/// corpus rule id at all. A SEPARATE, always-available check (`RunReport::unmatched_rule_ids`, no
/// corpus needed) already reports ids no *checker* answers; this additionally catches a bare
/// typo against the corpus itself when the corpus is reachable.
fn warn_on_unknown_corpus_rule_ids(
    rule_ids: &[String],
    corpus: Option<&camerata_rules::RuleSet>,
    corpus_dir: &std::path::Path,
) {
    let Some(rules) = corpus else {
        return; // corpus not reachable on this machine — silently skip, never fatal.
    };
    for id in rule_ids {
        if rules.get_by_id(id).is_none() {
            eprintln!(
                "camerata-check: warning: '{id}' is not a rule id in the loaded corpus \
                 ({}) — check for a typo",
                corpus_dir.display()
            );
        }
    }
}
