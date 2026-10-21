//! The bundled `CORPUS_FRESHNESS.toml` is valid and references only real corpus domains.
//!
//! Guards against (a) a syntax/shape error in the watermark file silently disabling the
//! staleness nag, and (b) a typo'd domain key (`javscript`) that would track freshness for a
//! pack that does not exist while the real one goes unwatched.

use camerata_rules::{corpus_freshness_path, corpus_path, load_corpus_lenient, CorpusFreshness};
use std::collections::BTreeSet;

/// Top-level domain of a rule's `domain` field: the part before any `:` (e.g. `rust:seaorm` ->
/// `rust`), matching the pack granularity the staleness watermark tracks.
fn top_level(domain: &str) -> String {
    domain.split(':').next().unwrap_or(domain).to_string()
}

#[tokio::test]
async fn bundled_freshness_file_loads_and_is_well_formed() {
    let loaded =
        CorpusFreshness::load(&corpus_freshness_path()).expect("CORPUS_FRESHNESS.toml must parse");
    let freshness = loaded.expect("CORPUS_FRESHNESS.toml must exist in the crate");

    assert!(
        freshness.warn_after_days > 0,
        "the default staleness window must be positive"
    );
    assert!(
        freshness.unparseable().is_empty(),
        "every date in CORPUS_FRESHNESS.toml must be YYYY-MM-DD; offenders: {:?}",
        freshness.unparseable()
    );
}

#[tokio::test]
async fn every_watermarked_domain_is_a_real_corpus_domain() {
    let (corpus, errors) = load_corpus_lenient(&corpus_path()).await;
    assert!(errors.is_empty(), "corpus must load cleanly: {errors:?}");

    let corpus_domains: BTreeSet<String> = corpus.iter().map(|r| top_level(&r.domain)).collect();

    let freshness = CorpusFreshness::load(&corpus_freshness_path())
        .expect("parse")
        .expect("exists");

    // Both the confirmed watermarks and any per-domain window override must name a domain the
    // corpus actually contains, else a rename/typo silently tracks (or skips) the wrong pack.
    let keys = freshness
        .confirmed
        .keys()
        .chain(freshness.warn_after_days_overrides.keys())
        .chain(freshness.warn_lead_days_overrides.keys());
    let unknown: Vec<&String> = keys.filter(|d| !corpus_domains.contains(*d)).collect();
    assert!(
        unknown.is_empty(),
        "CORPUS_FRESHNESS.toml references domains not in the corpus: {unknown:?}\n\
         known domains: {corpus_domains:?}"
    );
}

/// `warn_lead_days` must sit at the TOP LEVEL of the file. A key written after a `[table]`
/// header silently becomes a member of that table; because 14 is also the default, the parsed
/// struct would look identical and the misplacement would never be noticed. So this inspects
/// the raw TOML, not the deserialized struct.
#[test]
fn warn_lead_days_is_a_top_level_key_in_the_bundled_file() {
    let text = std::fs::read_to_string(corpus_freshness_path()).expect("file exists");
    let raw: toml::Value = text.parse().expect("valid TOML");
    assert_eq!(
        raw.get("warn_lead_days").and_then(toml::Value::as_integer),
        Some(14),
        "warn_lead_days must be a top-level integer (it must precede the first [table] header)"
    );
    assert!(
        raw.get("warn_after_days_overrides")
            .and_then(|t| t.get("warn_lead_days"))
            .is_none(),
        "warn_lead_days leaked into [warn_after_days_overrides]"
    );
    let loaded = CorpusFreshness::load(&corpus_freshness_path())
        .expect("parse")
        .expect("exists");
    assert_eq!(loaded.warn_lead_days, 14);
}
