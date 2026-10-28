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
    render_json, render_text, run_check, sidecar_path, AcceptSpec, AcceptTarget, AcceptedChange,
    CheckConfig, HttpFetcher, SidecarLock, SourceHashes,
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

/// The testable core of `--accept` / `--accept-all`: take the sidecar lock, accept in the sidecar
/// at `path`, and save it. Makes no requests. Returns what was accepted, hashes included.
fn accept_in_sidecar(
    path: &std::path::Path,
    target: AcceptTarget<'_>,
    today: &str,
    by: &str,
) -> Result<Vec<AcceptedChange>> {
    let _lock = SidecarLock::acquire(path)?;
    let mut sidecar = SourceHashes::load(path)?;
    let accepted = sidecar.accept(target, today, by)?;
    if !accepted.is_empty() {
        sidecar.save(path)?;
    }
    Ok(accepted)
}

/// What to do with the sidecar after a run.
#[derive(Debug, PartialEq, Eq)]
enum SaveDecision {
    /// Write it.
    Save,
    /// The operator asked for a dry run.
    SkipRequested,
    /// Some rule files did not load, so their URLs would look "no longer cited" and the save
    /// would erase their baselines and unreviewed changes. Nothing is written.
    RefuseCorpusErrors(usize),
}

/// Decide whether the run may save the sidecar. A corpus that loaded with errors never may.
fn save_decision(corpus_load_errors: usize, no_save: bool) -> SaveDecision {
    if no_save {
        SaveDecision::SkipRequested
    } else if corpus_load_errors > 0 {
        SaveDecision::RefuseCorpusErrors(corpus_load_errors)
    } else {
        SaveDecision::Save
    }
}

/// `--accept` / `--accept-all`: record the acceptance and print what was accepted.
fn run_accept(args: CheckSourcesArgs) -> Result<()> {
    let path = args.sidecar.clone().unwrap_or_else(sidecar_path);
    let specs: Vec<AcceptSpec> = args.accept.iter().map(|s| AcceptSpec::parse(s)).collect();
    let target = if args.accept_all {
        AcceptTarget::All
    } else {
        AcceptTarget::Urls(&specs)
    };
    let today = check_config(0).today;
    let by = accepter(args.by);
    let accepted = accept_in_sidecar(&path, target, &today, &by)?;
    for change in &accepted {
        println!("Accepted: {} ({})", change.url, change.hash);
        if change.unpinned && !args.accept_all {
            eprintln!(
                "warning: no hash was given for {}, so the CURRENT pending text was accepted. \
                 Use --accept \"<url>=<hash>\" with the hash from the report to accept only what \
                 you reviewed.",
                change.url
            );
        }
    }
    println!("{} change(s) accepted by {by} on {today}.", accepted.len());
    if !accepted.is_empty() {
        println!("Commit {} to record the acceptance.", path.display());
    }
    Ok(())
}

/// Run the check and print or write the report, or (with `--accept` / `--accept-all`) record
/// the acceptance of reviewed changes and make no requests.
pub async fn run_corpus_check_sources(args: CheckSourcesArgs) -> Result<()> {
    if args.accept_all || !args.accept.is_empty() {
        return run_accept(args);
    }

    let (corpus, errors) = load_corpus_lenient(&corpus_path()).await;
    if !errors.is_empty() {
        eprintln!(
            "warning: {} corpus file(s) failed to load: {errors:?}",
            errors.len()
        );
    }
    let decision = save_decision(errors.len(), args.no_save);

    let sidecar_file = args.sidecar.unwrap_or_else(sidecar_path);
    // Hold the lock for the whole run when it will write, so a second run (or an accept) cannot
    // interleave and silently overwrite this one. The sidecar is loaded AFTER the lock is taken.
    let _lock = match decision {
        SaveDecision::Save => Some(SidecarLock::acquire(&sidecar_file)?),
        _ => None,
    };
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

    match decision {
        SaveDecision::Save => {
            updated.save(&sidecar_file)?;
            eprintln!(
                "Sidecar updated: {}. Commit it to keep the baselines.",
                sidecar_file.display()
            );
        }
        SaveDecision::SkipRequested => eprintln!("--no-save: sidecar left untouched."),
        SaveDecision::RefuseCorpusErrors(n) => {
            anyhow::bail!(
                "sidecar NOT saved: {n} corpus file(s) failed to load, so their cited URLs would \
                 be treated as no longer cited and their baselines and unreviewed changes erased. \
                 Fix the rule files and run again. The report above is still valid for the rules \
                 that did load."
            );
        }
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
                anchor_missing_since: None,
            },
        );
        s
    }

    #[test]
    fn a_corpus_that_loaded_with_errors_never_saves_the_sidecar() {
        assert_eq!(save_decision(0, false), SaveDecision::Save);
        assert_eq!(save_decision(0, true), SaveDecision::SkipRequested);
        assert_eq!(save_decision(3, false), SaveDecision::RefuseCorpusErrors(3));
        // A dry run is a dry run whatever the corpus did.
        assert_eq!(save_decision(3, true), SaveDecision::SkipRequested);
    }

    #[test]
    fn an_accept_holds_the_lock_so_a_concurrent_one_fails_clearly() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("hashes.json");
        sidecar_with_pending("https://docs.example/g#s")
            .save(&path)
            .expect("save");
        let _held = SidecarLock::acquire(&path).expect("another run holds the lock");
        let err = accept_in_sidecar(&path, AcceptTarget::All, "2026-11-01", "pat")
            .expect_err("second holder");
        assert!(err.to_string().contains("hashes.json.lock"), "{err}");
        assert!(
            SourceHashes::load(&path).expect("load").entries["https://docs.example/g#s"]
                .pending
                .is_some(),
            "nothing was accepted"
        );
    }

    #[test]
    fn accepting_updates_the_saved_sidecar_without_any_request() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("hashes.json");
        let url = "https://docs.example/g#s";
        sidecar_with_pending(url).save(&path).expect("save");

        let done = accept_in_sidecar(
            &path,
            AcceptTarget::Urls(&[AcceptSpec::parse(&format!("{url}=sha256:new"))]),
            "2026-11-01",
            "pat",
        )
        .expect("accept");
        assert_eq!(done.len(), 1);
        assert_eq!(
            (done[0].url.as_str(), done[0].hash.as_str()),
            (url, "sha256:new")
        );
        assert!(!done[0].unpinned);

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
            AcceptTarget::Urls(&[AcceptSpec::unpinned("https://docs.example/other#x")]),
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
