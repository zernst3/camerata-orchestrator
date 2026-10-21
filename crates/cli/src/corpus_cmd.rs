//! `camerata corpus` - the operator surface for the corpus staleness watermark.
//!
//! Design: `docs/design/2026-10-20_corpus-staleness-check.md`. Two actions:
//!
//! - `corpus check` runs the staleness nag: which domains are overdue for a sweep, and which
//!   have never been confirmed at all. Non-fatal, read-only.
//! - `corpus confirm <domain> <date>` advances ONE domain's watermark after a human has
//!   reviewed that domain's sweep. This is the ONLY thing that advances a watermark; running a
//!   sweep never does. It writes `crates/rules/CORPUS_FRESHNESS.toml`; the operator commits it.

use anyhow::{bail, Context, Result};
use camerata_rules::{
    confirm_domain, corpus_freshness_path, corpus_path, format_staleness_nag, load_corpus_lenient,
    CorpusFreshness,
};
use chrono::Datelike;

/// Top-level domain (the part before any `:`), the pack granularity the watermark tracks.
fn top_level(domain: &str) -> String {
    domain.split(':').next().unwrap_or(domain).to_string()
}

/// The distinct top-level domains the live corpus contains, sorted.
async fn corpus_domains() -> Vec<String> {
    let (corpus, errors) = load_corpus_lenient(&corpus_path()).await;
    if !errors.is_empty() {
        eprintln!(
            "warning: {} corpus file(s) failed to load: {errors:?}",
            errors.len()
        );
    }
    let mut domains: Vec<String> = corpus.iter().map(|r| top_level(&r.domain)).collect();
    domains.sort();
    domains.dedup();
    domains
}

/// `corpus check`: print the staleness nag (overdue + never-confirmed), or a clean-bill line.
pub async fn run_corpus_check() -> Result<()> {
    let domains = corpus_domains().await;

    let Some(freshness) =
        CorpusFreshness::load(&corpus_freshness_path()).context("parsing CORPUS_FRESHNESS.toml")?
    else {
        println!(
            "No CORPUS_FRESHNESS.toml found. Every domain is unconfirmed:\n  {}",
            domains.join(", ")
        );
        return Ok(());
    };

    let unparseable = freshness.unparseable();
    if !unparseable.is_empty() {
        eprintln!(
            "warning: CORPUS_FRESHNESS.toml has unparseable dates for: {}",
            unparseable.join(", ")
        );
    }

    let today = chrono::Utc::now().date_naive();
    let overdue = freshness.overdue((today.year() as i64, today.month(), today.day()));
    let never = freshness.never_confirmed(&domains);

    match format_staleness_nag(&overdue, &never) {
        Some(msg) => println!("{msg}"),
        None => println!("Corpus is fresh: every domain confirmed within its staleness window."),
    }
    Ok(())
}

/// `corpus confirm <domain> <date>`: advance one domain's watermark to `date` (YYYY-MM-DD).
pub async fn run_corpus_confirm(domain: String, date: String) -> Result<()> {
    // Validate the date shape here so a typo never lands in the file.
    if !is_ymd(&date) {
        bail!("date must be YYYY-MM-DD (got {date:?})");
    }
    // Validate the domain is one the corpus actually contains, so a typo cannot create a
    // watermark for a pack that does not exist.
    let domains = corpus_domains().await;
    if !domains.contains(&domain) {
        bail!(
            "'{domain}' is not a corpus domain. Known domains:\n  {}",
            domains.join(", ")
        );
    }

    confirm_domain(&corpus_freshness_path(), &domain, &date)
        .with_context(|| format!("advancing the '{domain}' watermark"))?;

    println!("Confirmed '{domain}' as reviewed current on {date}.");
    println!("Commit crates/rules/CORPUS_FRESHNESS.toml to record the confirmation.");
    Ok(())
}

/// Minimal `YYYY-MM-DD` shape check (4-digit year, month 1-12, day 1-31).
fn is_ymd(s: &str) -> bool {
    let mut parts = s.split('-');
    let (Some(y), Some(m), Some(d), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    y.len() == 4
        && y.chars().all(|c| c.is_ascii_digit())
        && matches!(m.parse::<u32>(), Ok(1..=12))
        && matches!(d.parse::<u32>(), Ok(1..=31))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ymd_validation() {
        assert!(is_ymd("2026-10-20"));
        assert!(is_ymd("2026-01-01"));
        assert!(!is_ymd("2026-13-01")); // bad month
        assert!(!is_ymd("2026-10-40")); // bad day
        assert!(!is_ymd("26-10-20")); // short year
        assert!(!is_ymd("2026/10/20")); // wrong separator
        assert!(!is_ymd("last tuesday"));
        assert!(!is_ymd("2026-10-20-1")); // trailing
    }

    #[test]
    fn top_level_strips_subdomain() {
        assert_eq!(top_level("rust:seaorm"), "rust");
        assert_eq!(top_level("supabase:auth"), "supabase");
        assert_eq!(top_level("universal"), "universal");
    }
}
