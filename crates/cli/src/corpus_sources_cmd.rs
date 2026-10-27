//! `camerata corpus check-sources`: Part A of the corpus freshness routine.
//!
//! Fetches every rule's `[[sources]]` URL politely, records ok / redirected / dead (or
//! unreachable), hashes each cited section against the sidecar from the last run, and prints a
//! date-stamped report as text or JSON. No model is involved and no rule is ever edited; the only
//! file written is the sidecar (`crates/rules/CORPUS_SOURCE_HASHES.json`), and only when asked
//! not to skip it.
//!
//! The logic lives in `camerata-source-check`; this module is the thin operator surface.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use camerata_rules::{corpus_path, load_corpus_lenient};
use camerata_source_check::{
    render_json, render_text, run_check, sidecar_path, CheckConfig, HttpFetcher, SourceHashes,
};

/// Seconds a single request may take before the URL counts as unreachable.
const REQUEST_TIMEOUT_SECS: u64 = 20;
/// Hosts worked side by side (each host still sees one request at a time).
const PARALLEL_HOSTS: usize = 8;

/// The user agent sent on every request, naming the tool and its purpose.
fn user_agent() -> String {
    format!(
        "camerata-source-check/{} (corpus link and section freshness check)",
        env!("CARGO_PKG_VERSION")
    )
}

/// Options for `corpus check-sources`.
#[derive(Debug)]
pub struct CheckSourcesArgs {
    /// Print JSON instead of text.
    pub json: bool,
    /// Write the report to this file instead of stdout.
    pub out: Option<PathBuf>,
    /// Sidecar to read and update (default: beside the corpus).
    pub sidecar: Option<PathBuf>,
    /// Do not write the sidecar back (a dry run that records nothing).
    pub no_save: bool,
    /// Pause between two requests to the same host, in milliseconds.
    pub delay_ms: u64,
}

/// Run the check and print or write the report.
pub async fn run_corpus_check_sources(args: CheckSourcesArgs) -> Result<()> {
    let (corpus, errors) = load_corpus_lenient(&corpus_path()).await;
    if !errors.is_empty() {
        eprintln!(
            "warning: {} corpus file(s) failed to load: {errors:?}",
            errors.len()
        );
    }

    let sidecar_file = args.sidecar.unwrap_or_else(sidecar_path);
    let previous = SourceHashes::load(&sidecar_file)?;
    let fetcher = HttpFetcher::new(Duration::from_secs(REQUEST_TIMEOUT_SECS), &user_agent())?;
    let config = check_config(args.delay_ms);

    let (report, updated) = run_check(&corpus, &fetcher, &previous, &config).await;

    let rendered = if args.json {
        render_json(&report)?
    } else {
        render_text(&report)
    };
    match &args.out {
        Some(path) => {
            std::fs::write(path, &rendered)
                .with_context(|| format!("writing report {}", path.display()))?;
            eprintln!("Report written to {}", path.display());
        }
        None => println!("{rendered}"),
    }

    if args.no_save {
        eprintln!("--no-save: sidecar left untouched.");
    } else {
        updated.save(&sidecar_file)?;
        eprintln!(
            "Sidecar updated: {}. Commit it to keep the baselines.",
            sidecar_file.display()
        );
    }
    Ok(())
}

/// The run settings, with today's UTC date (the one clock read).
fn check_config(delay_ms: u64) -> CheckConfig {
    CheckConfig {
        today: chrono::Utc::now().format("%Y-%m-%d").to_string(),
        delay: Duration::from_millis(delay_ms),
        max_parallel_hosts: PARALLEL_HOSTS,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_agent_names_the_tool_and_its_purpose() {
        let ua = user_agent();
        assert!(ua.starts_with("camerata-source-check/"));
        assert!(ua.contains("freshness"));
    }

    #[test]
    fn the_run_date_is_a_strict_ymd() {
        let cfg = check_config(0);
        assert!(
            camerata_rules::parse_ymd(&cfg.today).is_some(),
            "{}",
            cfg.today
        );
    }
}
