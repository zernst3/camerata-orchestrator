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

    warn_on_unknown_corpus_rule_ids(&cli.rule_id).await;

    let opts = camerata_check::Options {
        repo_root: cli.path.clone(),
        config_override: cli.config.clone(),
        rule_ids: cli.rule_id.clone(),
    };

    let report = match camerata_check::run(&opts) {
        Ok(report) => report,
        Err(e) => {
            eprintln!("camerata-check: error: {e:#}");
            return ExitCode::from(camerata_check::EXIT_CODE_RUN_ERROR as u8);
        }
    };

    match cli.format {
        Format::Human => print!("{}", camerata_check::render_human(&report)),
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

/// Best-effort, non-fatal: if the rule corpus happens to be loadable (it is, on any machine
/// that has this repo checked out; it usually is NOT on a bare client CI runner — the binary
/// works fine either way, since this is purely an informational typo-catcher, never load-
/// bearing for the actual check), warn about any `--rule-id` value that isn't a real corpus
/// rule id at all. A SEPARATE, always-available check (`RunReport::unmatched_rule_ids`, no
/// corpus needed) already reports ids no *checker* answers; this additionally catches a bare
/// typo against the corpus itself when the corpus happens to be reachable.
async fn warn_on_unknown_corpus_rule_ids(rule_ids: &[String]) {
    if rule_ids.is_empty() {
        return;
    }
    let corpus_dir = camerata_rules::corpus_path();
    let Ok(rules) = camerata_rules::load_corpus(&corpus_dir).await else {
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
