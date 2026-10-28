//! The public docs state ONE rule count and ONE domain count, and they are the real ones.
//!
//! A reader who lands on the README sees "N rules across M domains". That claim is only worth
//! making if it cannot rot: a rule added without touching the README, or an old figure left in
//! a paragraph nobody re-read, is a public statement that is quietly false. So this test counts
//! the corpus exactly as the loader does (`load_corpus` over the bundled `principles/`
//! directory: every `.toml` file under it is a rule, and a file that is not a rule fails the
//! load), then reads the reader-facing docs and fails on ANY rule-count or domain-count
//! number that disagrees.
//!
//! What counts as a count claim: a number written within the three words before "rule(s)" or
//! "domain(s)" ("437 rules", "437 TOML rules", "a ~423-rule corpus", "22 domains"). Write the
//! count as plain digits so the check can see it.
//!
//! A "domain" is a top-level folder of `principles/` (`rust/axum` belongs to `rust`).

use std::path::{Path, PathBuf};

use camerata_rules::{load_corpus, DEFAULT_CORPUS_PATH};

/// Docs a reader lands on. Each is checked for stale counts; the README must also carry the
/// canonical "N rules across M domains" sentence.
const CHECKED_DOCS: &[&str] = &["README.md", "docs/OVERVIEW.md"];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Split into lowercase word tokens: whitespace, hyphens and slashes separate words, and
/// surrounding markdown/punctuation is trimmed so `**417` and `rules,` read as `417` / `rules`.
fn words(text: &str) -> Vec<String> {
    text.split(|c: char| c.is_whitespace() || c == '-' || c == '/' || c == '\u{2013}')
        .map(|w| {
            w.trim_matches(|c: char| !c.is_alphanumeric() && c != '~' && c != '+')
                .to_lowercase()
        })
        .filter(|w| !w.is_empty())
        .collect()
}

/// `437`, `~423`, `1,200`, `350+` parse to a number; anything else is `None`.
fn as_count(word: &str) -> Option<u64> {
    let digits = word
        .trim_start_matches('~')
        .trim_end_matches('+')
        .replace(',', "");
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// Every number written within three words before one of `nouns`, with the noun it counts.
fn count_claims(text: &str, nouns: &[&str]) -> Vec<(u64, String)> {
    let toks = words(text);
    let mut out = Vec::new();
    for (i, tok) in toks.iter().enumerate() {
        if !nouns.contains(&tok.as_str()) {
            continue;
        }
        for back in 1..=3 {
            if i < back {
                break;
            }
            if let Some(n) = as_count(&toks[i - back]) {
                let context = toks[i - back..=i].join(" ");
                out.push((n, context));
                break;
            }
        }
    }
    out
}

async fn real_counts() -> (usize, usize) {
    let set = load_corpus(Path::new(DEFAULT_CORPUS_PATH))
        .await
        .expect("the bundled corpus loads: every .toml under principles/ must be a valid rule");
    (set.len(), set.top_level_domains().len())
}

#[tokio::test]
async fn req_docs_state_the_real_rule_and_domain_count() {
    let (rules, domains) = real_counts().await;
    assert!(rules > 0 && domains > 0, "the corpus loaded empty");

    let canonical = format!("{rules} rules across {domains} domains");
    let mut problems: Vec<String> = Vec::new();

    for doc in CHECKED_DOCS {
        let text = std::fs::read_to_string(repo_root().join(doc))
            .unwrap_or_else(|e| panic!("cannot read {doc}: {e}"));

        for (n, ctx) in count_claims(&text, &["rule", "rules"]) {
            if n != rules as u64 {
                problems.push(format!(
                    "{doc}: says \"{ctx}\" but the corpus holds {rules} rules. \
                     Change {n} to {rules}."
                ));
            }
        }
        for (n, ctx) in count_claims(&text, &["domain", "domains"]) {
            if n != domains as u64 {
                problems.push(format!(
                    "{doc}: says \"{ctx}\" but the corpus holds {domains} domains. \
                     Change {n} to {domains}."
                ));
            }
        }

        if *doc == "README.md" && !text.contains(&canonical) {
            problems.push(format!(
                "README.md must state \"{canonical}\" verbatim (the headline count line). \
                 Update the rule-library line to read exactly that."
            ));
        }
    }

    assert!(
        problems.is_empty(),
        "the public docs disagree with the corpus ({rules} rules across {domains} domains, \
         counted the way the loader counts them). Fix:\n  {}",
        problems.join("\n  ")
    );
}

/// The parser must actually see stale numbers, or the test above could pass on a broken
/// scanner. These cases are the shapes the README and OVERVIEW have used.
#[test]
fn req_count_scanner_sees_every_shape_of_count_claim() {
    let cases = [
        ("424 rules across 21 domains", 424),
        ("a corpus of **417 rules across many stacks", 417),
        ("(417 TOML rules as of 2026-10-11)", 417),
        ("the ~423-rule corpus loader", 423),
        ("about 1,200 rules", 1200),
        ("350+ rules", 350),
    ];
    for (text, want) in cases {
        let got = count_claims(text, &["rule", "rules"]);
        assert_eq!(
            got.first().map(|(n, _)| *n),
            Some(want),
            "scanner missed the count in {text:?}"
        );
    }
    assert_eq!(
        count_claims("21 domains", &["domain", "domains"])[0].0,
        21,
        "scanner missed a domain count"
    );
    // Prose with no count claim must stay silent.
    assert!(count_claims(
        "the process rules and the security floor",
        &["rule", "rules"]
    )
    .is_empty());
    assert!(count_claims("per-rule severity bounds", &["rule", "rules"]).is_empty());
}
