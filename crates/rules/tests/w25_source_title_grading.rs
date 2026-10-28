//! W25 — a `[[sources]].title` carries no internal authority/confidence GRADE.
//!
//! A source title is cited verbatim in the client's citation block. An author's own grading of
//! how well the source grounds the rule — "(low authority — the pages establish the mechanism,
//! not the mandate)", "(medium authority — grounds the general pattern)" — is a note to the
//! corpus author about relevance, not something a client reading "ASP.NET Core — Middleware"
//! on their citation should see. Its home is the `[[sources]].relevance` field, which is
//! author-facing and reaches no artifact.
//!
//! This checks only the GRADE prefix (`low`/`medium`/`high`/`low-medium`, with or without
//! "authority"). A parenthetical that is purely "what this source establishes", with no grade,
//! is a legitimate title clause the authoring guide permits and is left alone.

use camerata_rules::{corpus_path, load_corpus_lenient};

fn carries_authority_grade(title: &str) -> bool {
    // A parenthetical opening with a bare confidence grade, optionally the word "authority",
    // then a separator. Matches "(low —", "(low authority —", "(medium authority:", etc.
    let lower = title.to_ascii_lowercase();
    for grade in ["(low-medium", "(low", "(medium", "(high"] {
        if let Some(i) = lower.find(grade) {
            let rest = lower[i + grade.len()..].trim_start();
            let rest = rest.strip_prefix("authority").unwrap_or(rest).trim_start();
            if rest.starts_with('—') || rest.starts_with('-') || rest.starts_with(':') {
                return true;
            }
        }
    }
    false
}

#[tokio::test]
async fn req_w25_no_source_title_carries_an_authority_grade() {
    let (corpus, errors) = load_corpus_lenient(&corpus_path()).await;
    assert!(errors.is_empty(), "corpus must load cleanly: {errors:?}");

    let mut offenders: Vec<String> = Vec::new();
    for rule in corpus.iter() {
        for src in &rule.sources {
            if carries_authority_grade(&src.title) {
                offenders.push(format!(
                    "{}: {:?}",
                    rule.id.0,
                    src.title.chars().take(90).collect::<String>()
                ));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "these source titles ship an internal authority grade into the client's citation \
         block; move the grade to `[[sources]].relevance`:\n  {}",
        offenders.join("\n  ")
    );
}
