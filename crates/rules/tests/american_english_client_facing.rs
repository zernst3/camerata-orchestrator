//! House style: client-reaching rule text is American English.
//!
//! Runs over the same field set as the dash gate (`common::client_reaching_fields`). A British
//! spelling in a title, directive, option label or any other field can land on the Rules checked
//! sheet, coverage.json, coverage.csv or a report. Words are matched whole (case-insensitive,
//! letters only on both sides) so a URL path or an identifier like `colour_picker` is not hit.
//!
//! Exempt: the title of an external source (`[[sources]].title`), because a quoted standard or
//! book keeps its own spelling.

mod common;

use camerata_rules::{corpus_path, load_corpus_lenient, RuleSet};
use common::client_reaching_fields;

/// (British stem, American stem). A word is flagged when it is the stem plus an optional
/// inflection suffix drawn from `SUFFIXES`.
const PAIRS: &[(&str, &str)] = &[
    ("behaviour", "behavior"),
    ("honour", "honor"),
    ("colour", "color"),
    ("organis", "organiz"),
    ("recognis", "recogniz"),
    ("normalis", "normaliz"),
    ("centre", "center"),
    ("licence", "license"),
    ("categoris", "categoriz"),
    ("favour", "favor"),
    ("analyse", "analyze"),
    ("analysing", "analyzing"),
];

const SUFFIXES: &[&str] = &[
    "", "s", "e", "d", "r", "rs", "ed", "es", "ing", "ings", "er", "ers", "ation", "ations",
    "able", "ly", "ite", "ites",
];

/// The British words found in `text` (whole words only).
fn british_words(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    // A token holding "://" is a URL, not prose.
    for token in text.split_whitespace().filter(|t| !t.contains("://")) {
        let lower = token.to_lowercase();
        for word in lower.split(|c: char| !c.is_ascii_alphabetic()) {
            if word.is_empty() {
                continue;
            }
            for (british, _) in PAIRS {
                if let Some(rest) = word.strip_prefix(british) {
                    if SUFFIXES.contains(&rest) {
                        found.push(word.to_string());
                    }
                }
            }
        }
    }
    found
}

fn spelling_offenders(corpus: &RuleSet) -> Vec<String> {
    client_reaching_fields(corpus)
        .into_iter()
        .filter(|f| !f.external_title)
        .flat_map(|f| {
            british_words(f.text)
                .into_iter()
                .map(move |w| format!("{}: {} ({w})", f.rule_id, f.loc))
        })
        .collect()
}

#[tokio::test]
async fn no_client_reaching_rule_field_uses_a_british_spelling() {
    let (corpus, errors) = load_corpus_lenient(&corpus_path()).await;
    assert!(errors.is_empty(), "corpus must load cleanly: {errors:?}");
    assert!(corpus.iter().count() > 100, "vacuity guard");
    let offenders = spelling_offenders(&corpus);
    assert!(
        offenders.is_empty(),
        "client-reaching rule text must use American spelling ({} offenders):\n  {}",
        offenders.len(),
        offenders.join("\n  ")
    );
}

#[test]
fn the_matcher_catches_inflections_and_ignores_urls_and_american_words() {
    assert_eq!(
        british_words("The behaviour was analysed"),
        ["behaviour", "analysed"]
    );
    assert_eq!(
        british_words("Organisations recognise colours"),
        ["organisations", "recognise", "colours"]
    );
    assert!(british_words("See https://example.com/colour/behaviour").is_empty());
    assert!(british_words("center license categorize favorite behavior analyzed").is_empty());
}
