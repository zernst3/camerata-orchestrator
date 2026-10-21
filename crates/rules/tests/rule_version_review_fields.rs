//! Per-rule `version` and `last_reviewed`, and the supersession pairing gate
//! (design: `docs/design/2026-10-20_corpus-staleness-check.md`, "Amendment 2026-10-21").
//!
//! - `version` is a REQUIRED integer >= 1 on every corpus rule. It is bumped whenever anything
//!   about the rule changes; the lock gate (`corpus_lock.rs`) is what forces the bump.
//! - `last_reviewed` is OPTIONAL: the date this one rule was individually confirmed current,
//!   between full domain sweeps. It must be a real calendar date and not in the future.
//! - `superseded_since` and `current_recommendation` travel together: a finding that says
//!   "the ecosystem moved on in 2021" with no pointer to what replaced it is half a claim.
//!
//! Same lenient-parse / strict-gate split `kind` and `inspectable` use: the TOML parse accepts
//! a rule that omits `version` (fixture corpora in other crates do not declare it), and the
//! GATE fails the build on the real corpus. Each gate is also shown to FIRE against a
//! synthetic corpus, because a gate that cannot fail is the failure class this corpus has
//! already shipped three times.

use camerata_rules::{
    corpus_files_with_unknown_fields, load_corpus, load_corpus_lenient, rules_missing_version,
    rules_with_invalid_last_reviewed, rules_with_invalid_version, rules_with_unpaired_supersession,
    RuleSet, DEFAULT_CORPUS_PATH,
};

/// The date the synthetic fixtures treat as "today". Fixed, so no test here depends on a clock.
const TODAY: (i64, u32, u32) = (2026, 10, 21);

fn rule_toml(id: &str, top_level_extra: &str) -> String {
    format!(
        r#"id = "{id}"
title = "Synthetic fixture rule"
enforcement = "prose"
kind = "decision"
inspectable = true
default = true
verification = "policy"
{top_level_extra}

[decision]
question = "Synthetic?"
default = "the-correct-option"

[[option]]
id = "the-correct-option"
label = "The correct option"
directive = "Do the correct thing."
why = "The natural fit for the synthetic fixture."
"#
    )
}

fn synthetic_corpus(files: &[(&str, String)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp dir");
    let universal = dir.path().join("universal");
    std::fs::create_dir_all(&universal).expect("mkdir universal");
    for (name, body) in files {
        std::fs::write(universal.join(name), body).expect("write fixture");
    }
    dir
}

async fn load(files: &[(&str, String)]) -> (tempfile::TempDir, RuleSet) {
    let dir = synthetic_corpus(files);
    let (set, errs) = load_corpus_lenient(dir.path()).await;
    assert!(errs.is_empty(), "the synthetic corpus must parse: {errs:?}");
    (dir, set)
}

/// The real shipped corpus, or `None` when it is not checked out.
async fn real_corpus() -> Option<RuleSet> {
    let path = std::path::Path::new(DEFAULT_CORPUS_PATH);
    if !path.exists() {
        return None;
    }
    Some(load_corpus(path).await.expect("corpus loads"))
}

// ────────────────────────────────────────────────────────────────────────────
// Parsing
// ────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn version_and_last_reviewed_round_trip_onto_the_rule() {
    let (_dir, set) = load(&[(
        "a.toml",
        rule_toml(
            "FIX-ROUNDTRIP-1",
            "version = 3\nlast_reviewed = \"2026-09-01\"",
        ),
    )])
    .await;
    let rule = set.get_by_id("FIX-ROUNDTRIP-1").expect("loaded");
    assert_eq!(rule.version, Some(3));
    assert_eq!(rule.last_reviewed.as_deref(), Some("2026-09-01"));
}

#[tokio::test]
async fn a_rule_that_omits_both_fields_still_parses_with_none() {
    // Lenient parse: it is the GATE, not the loader, that refuses an unversioned rule.
    let (_dir, set) = load(&[("a.toml", rule_toml("FIX-BARE-1", ""))]).await;
    let rule = set.get_by_id("FIX-BARE-1").expect("loaded");
    assert_eq!(rule.version, None);
    assert_eq!(rule.last_reviewed, None);
}

#[tokio::test]
async fn a_blank_last_reviewed_is_scrubbed_to_absent() {
    let (_dir, set) = load(&[(
        "a.toml",
        rule_toml("FIX-BLANK-1", "version = 1\nlast_reviewed = \"  \""),
    )])
    .await;
    assert_eq!(set.get_by_id("FIX-BLANK-1").unwrap().last_reviewed, None);
}

#[tokio::test]
async fn the_unknown_field_gate_accepts_the_two_new_fields() {
    let dir = synthetic_corpus(&[(
        "a.toml",
        rule_toml(
            "FIX-ALLOWLIST-1",
            "version = 1\nlast_reviewed = \"2026-09-01\"",
        ),
    )]);
    let offenders = corpus_files_with_unknown_fields(dir.path())
        .await
        .expect("walks");
    assert!(
        offenders.is_empty(),
        "`version` and `last_reviewed` are declared schema, not typos: {offenders:?}"
    );
}

// ────────────────────────────────────────────────────────────────────────────
// `version` gates
// ────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn the_missing_version_gate_fires_on_a_rule_that_omits_it() {
    let (_dir, set) = load(&[
        ("ok.toml", rule_toml("FIX-HAS-VERSION-1", "version = 1")),
        ("bad.toml", rule_toml("FIX-NO-VERSION-1", "")),
    ])
    .await;
    assert_eq!(
        rules_missing_version(&set),
        vec!["FIX-NO-VERSION-1".to_string()]
    );
}

#[tokio::test]
async fn the_invalid_version_gate_rejects_zero_and_accepts_one() {
    let (_dir, set) = load(&[
        ("zero.toml", rule_toml("FIX-ZERO-1", "version = 0")),
        ("one.toml", rule_toml("FIX-ONE-1", "version = 1")),
        ("seven.toml", rule_toml("FIX-SEVEN-1", "version = 7")),
        // Absent is the OTHER gate's finding; this one must not double-report it.
        ("none.toml", rule_toml("FIX-NONE-1", "")),
    ])
    .await;
    assert_eq!(
        rules_with_invalid_version(&set),
        vec!["FIX-ZERO-1: version 0 (must be an integer >= 1)".to_string()]
    );
}

#[tokio::test]
async fn req_every_corpus_rule_declares_a_version_of_at_least_one() {
    let Some(set) = real_corpus().await else {
        return;
    };
    let missing = rules_missing_version(&set);
    assert!(
        missing.is_empty(),
        "every corpus rule must declare `version` (an integer >= 1, bumped whenever anything \
         about the rule changes). Undeclared:\n  {}",
        missing.join("\n  ")
    );
    let invalid = rules_with_invalid_version(&set);
    assert!(
        invalid.is_empty(),
        "`version` must be >= 1:\n  {}",
        invalid.join("\n  ")
    );
}

// ────────────────────────────────────────────────────────────────────────────
// `last_reviewed` gate
// ────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn the_last_reviewed_gate_accepts_today_and_the_past() {
    let (_dir, set) = load(&[
        (
            "today.toml",
            rule_toml("FIX-TODAY-1", "version = 1\nlast_reviewed = \"2026-10-21\""),
        ),
        (
            "past.toml",
            rule_toml("FIX-PAST-1", "version = 1\nlast_reviewed = \"2026-02-28\""),
        ),
        (
            "leap.toml",
            rule_toml("FIX-LEAP-1", "version = 1\nlast_reviewed = \"2024-02-29\""),
        ),
        ("absent.toml", rule_toml("FIX-ABSENT-1", "version = 1")),
    ])
    .await;
    assert_eq!(
        rules_with_invalid_last_reviewed(&set, TODAY),
        Vec::<String>::new()
    );
}

#[tokio::test]
async fn the_last_reviewed_gate_rejects_future_and_malformed_dates() {
    let (_dir, set) = load(&[
        (
            "future.toml",
            rule_toml(
                "FIX-FUTURE-1",
                "version = 1\nlast_reviewed = \"2026-10-22\"",
            ),
        ),
        (
            "prose.toml",
            rule_toml(
                "FIX-PROSE-1",
                "version = 1\nlast_reviewed = \"last tuesday\"",
            ),
        ),
        (
            "month.toml",
            rule_toml("FIX-MONTH-1", "version = 1\nlast_reviewed = \"2026-13-01\""),
        ),
        // Shape-valid but not a real calendar day: the lenient 1..=31 check would pass it.
        (
            "feb30.toml",
            rule_toml("FIX-FEB30-1", "version = 1\nlast_reviewed = \"2026-02-30\""),
        ),
        // 2026 is not a leap year.
        (
            "feb29.toml",
            rule_toml("FIX-FEB29-1", "version = 1\nlast_reviewed = \"2026-02-29\""),
        ),
        (
            "short.toml",
            rule_toml("FIX-SHORT-1", "version = 1\nlast_reviewed = \"26-10-01\""),
        ),
    ])
    .await;
    let offenders = rules_with_invalid_last_reviewed(&set, TODAY);
    let ids: Vec<&str> = offenders
        .iter()
        .map(|o| o.split(':').next().unwrap())
        .collect();
    assert_eq!(
        ids,
        vec![
            "FIX-FEB29-1",
            "FIX-FEB30-1",
            "FIX-FUTURE-1",
            "FIX-MONTH-1",
            "FIX-PROSE-1",
            "FIX-SHORT-1"
        ],
        "{offenders:?}"
    );
    assert!(
        offenders
            .iter()
            .find(|o| o.starts_with("FIX-FUTURE-1"))
            .unwrap()
            .contains("future"),
        "the future-date message must say so: {offenders:?}"
    );
}

#[tokio::test]
async fn req_every_authored_last_reviewed_is_a_real_past_or_present_date() {
    let Some(set) = real_corpus().await else {
        return;
    };
    // The one place this suite reads a clock: the question is "is the authored date in the
    // future relative to the real today", which no fixed date can answer.
    let now = chrono::Utc::now().date_naive();
    let today = {
        use chrono::Datelike;
        (now.year() as i64, now.month(), now.day())
    };
    let offenders = rules_with_invalid_last_reviewed(&set, today);
    assert!(
        offenders.is_empty(),
        "`last_reviewed` must be a real YYYY-MM-DD date that is not in the future:\n  {}",
        offenders.join("\n  ")
    );
}

// ────────────────────────────────────────────────────────────────────────────
// Supersession pairing
// ────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn the_pairing_gate_requires_both_or_neither() {
    let (_dir, set) = load(&[
        (
            "both.toml",
            rule_toml(
                "FIX-BOTH-1",
                "version = 1\nsuperseded_since = \"2021\"\ncurrent_recommendation = \"Use X instead.\"",
            ),
        ),
        ("neither.toml", rule_toml("FIX-NEITHER-1", "version = 1")),
        (
            "year-only.toml",
            rule_toml("FIX-YEAR-ONLY-1", "version = 1\nsuperseded_since = \"2021\""),
        ),
        (
            "rec-only.toml",
            rule_toml(
                "FIX-REC-ONLY-1",
                "version = 1\ncurrent_recommendation = \"Use X instead.\"",
            ),
        ),
        // Blank is scrubbed to absent at load, so a blank recommendation is still unpaired.
        (
            "blank-rec.toml",
            rule_toml(
                "FIX-BLANK-REC-1",
                "version = 1\nsuperseded_since = \"2021\"\ncurrent_recommendation = \"  \"",
            ),
        ),
    ])
    .await;
    let offenders = rules_with_unpaired_supersession(&set);
    let ids: Vec<&str> = offenders
        .iter()
        .map(|o| o.split(':').next().unwrap())
        .collect();
    assert_eq!(
        ids,
        vec!["FIX-BLANK-REC-1", "FIX-REC-ONLY-1", "FIX-YEAR-ONLY-1"],
        "{offenders:?}"
    );
}

#[tokio::test]
async fn req_no_corpus_rule_has_half_a_supersession_claim() {
    let Some(set) = real_corpus().await else {
        return;
    };
    let offenders = rules_with_unpaired_supersession(&set);
    assert!(
        offenders.is_empty(),
        "`superseded_since` and `current_recommendation` must be set together:\n  {}",
        offenders.join("\n  ")
    );
}
