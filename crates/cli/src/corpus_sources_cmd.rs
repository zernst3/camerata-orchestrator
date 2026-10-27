//! `camerata corpus check-sources`: Part A of the corpus freshness routine.
//!
//! Fetches every rule's `[[sources]]` URL politely, records ok / redirected / dead (or
//! unreachable), hashes each cited section against the sidecar from the last run, and prints a
//! date-stamped report as text or JSON. No model is involved and no rule is ever edited; the only
//! file written is the sidecar (`crates/rules/CORPUS_SOURCE_HASHES.json`), unless `--no-save`.
//!
//! A changed section is never silently re-baselined: it is listed on every run until a person
//! accepts it with `--accept <url#anchor>` or `--accept-all`, which records who and when.
//!
//! The logic lives in `camerata-source-check`; this module is the thin operator surface.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use camerata_rules::{corpus_path, load_corpus_lenient};
use camerata_source_check::{
    render_json, render_text, run_check, sidecar_path, AcceptTarget, CheckConfig, HttpFetcher,
    SourceHashes,
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
    /// Cited URLs (anchor included) whose changed section was reviewed and is accepted.
    pub accept: Vec<String>,
    /// Accept every unreviewed change.
    pub accept_all: bool,
    /// Who is accepting; defaults to `$USER`.
    pub by: Option<String>,
}

/// Who to record as the accepter: `--by`, else `$USER`, else `unknown`.
fn accepter(by: Option<String>) -> String {
    by.or_else(|| std::env::var("USER").ok())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

/// The testable core of `--accept` / `--accept-all`: accept in the sidecar at `path` and save it.
/// Makes no requests. Returns the accepted URLs.
fn accept_in_sidecar(
    path: &std::path::Path,
    target: AcceptTarget<'_>,
    today: &str,
    by: &str,
) -> Result<Vec<String>> {
    let mut sidecar = SourceHashes::load(path)?;
    let accepted = sidecar.accept(target, today, by)?;
    if !accepted.is_empty() {
        sidecar.save(path)?;
    }
    Ok(accepted)
}

/// Run the check and print or write the report, or (with `--accept` / `--accept-all`) record
/// the acceptance of reviewed changes and make no requests.
pub async fn run_corpus_check_sources(args: CheckSourcesArgs) -> Result<()> {
    if args.accept_all || !args.accept.is_empty() {
        let path = args.sidecar.unwrap_or_else(sidecar_path);
        let target = if args.accept_all {
            AcceptTarget::All
        } else {
            AcceptTarget::Urls(&args.accept)
        };
        let today = check_config(0).today;
        let by = accepter(args.by);
        let accepted = accept_in_sidecar(&path, target, &today, &by)?;
        for url in &accepted {
            println!("Accepted: {url}");
        }
        println!("{} change(s) accepted by {by} on {today}.", accepted.len());
        if !accepted.is_empty() {
            println!("Commit {} to record the acceptance.", path.display());
        }
        return Ok(());
    }

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

    fn sidecar_with_pending(url: &str) -> SourceHashes {
        use camerata_source_check::{LinkState, PendingChange, SourceRecord};
        let mut s = SourceHashes::default();
        s.entries.insert(
            url.to_string(),
            SourceRecord {
                last_checked: "2026-10-01".to_string(),
                link: LinkState::Ok,
                redirected_to: None,
                section_hash: Some("sha256:old".to_string()),
                section_excerpt: Some("Old.".to_string()),
                pending: Some(PendingChange {
                    hash: "sha256:new".to_string(),
                    excerpt: "New.".to_string(),
                    first_seen: "2026-10-15".to_string(),
                }),
                accepted: None,
            },
        );
        s
    }

    #[test]
    fn accepting_updates_the_saved_sidecar_without_any_request() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("hashes.json");
        let url = "https://docs.example/g#s";
        sidecar_with_pending(url).save(&path).expect("save");

        let done = accept_in_sidecar(
            &path,
            AcceptTarget::Urls(&[url.to_string()]),
            "2026-11-01",
            "pat",
        )
        .expect("accept");
        assert_eq!(done, vec![url.to_string()]);

        let reloaded = SourceHashes::load(&path).expect("load");
        let record = &reloaded.entries[url];
        assert!(record.pending.is_none());
        assert_eq!(record.section_hash.as_deref(), Some("sha256:new"));
        let accepted = record.accepted.as_ref().expect("acceptance recorded");
        assert_eq!(
            (accepted.by.as_str(), accepted.on.as_str()),
            ("pat", "2026-11-01")
        );
    }

    #[test]
    fn a_bad_accept_leaves_the_file_untouched() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("hashes.json");
        sidecar_with_pending("https://docs.example/g#s")
            .save(&path)
            .expect("save");
        let before = std::fs::read_to_string(&path).expect("read");
        let err = accept_in_sidecar(
            &path,
            AcceptTarget::Urls(&["https://docs.example/other#x".to_string()]),
            "2026-11-01",
            "pat",
        )
        .expect_err("unknown url");
        assert!(err.to_string().contains("other#x"));
        assert_eq!(before, std::fs::read_to_string(&path).expect("read"));
    }

    #[test]
    fn the_accepter_is_the_explicit_name_first() {
        assert_eq!(accepter(Some("zach".to_string())), "zach");
        assert!(!accepter(None).is_empty());
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
