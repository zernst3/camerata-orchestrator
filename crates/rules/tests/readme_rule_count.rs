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
//! `docs/USER_GUIDE.md` is a long document full of numbered lists, so it is held to the exact
//! sentence shapes only: "N rules across M domains" and "N corpus rules" (line breaks inside the
//! sentence are fine). Any other number near the word "rules" there is left alone.
//!
//! A "domain" is a top-level folder of `principles/` (`rust/axum` belongs to `rust`).
//!
//! Beyond those two docs, EVERY markdown file in the repository is checked for the headline
//! shape alone: "N rules across M domains". A user guide, a contributing note or a technical
//! doc that quotes the headline must quote the real one. Other count shapes are only checked
//! in `CHECKED_DOCS`, because dated plans and audit records legitimately say "all 417 rules"
//! about the corpus as it stood on their date.

use std::path::{Path, PathBuf};

use camerata_rules::{load_corpus, DEFAULT_CORPUS_PATH};

/// Docs a reader lands on. Each is checked for stale counts; the README must also carry the
/// canonical "N rules across M domains" sentence.
const CHECKED_DOCS: &[&str] = &[
    "README.md",
    "docs/OVERVIEW.md",
    "docs/RULES_REFERENCE.md",
    "docs/RULE_AUTHORING.md",
    "docs/ARCHITECTURE.md",
    "docs/TECHNICAL.md",
    "docs/ENFORCEMENT.md",
];

/// Docs held to the exact sentence shapes only (see the module doc).
const SENTENCE_CHECKED_DOCS: &[&str] = &["docs/USER_GUIDE.md"];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Marks a sentence, clause or list-item boundary in the token stream. It is never a word and
/// never a number, so a count claim cannot reach across it.
const BOUNDARY: &str = "|";

/// An ISO date such as `2026-10-27` (digits only, 4-2-2). It is one token, never three numbers.
fn is_iso_date(word: &str) -> bool {
    let parts: Vec<&str> = word.split('-').collect();
    matches!(parts.as_slice(), [y, m, d]
        if y.len() == 4 && m.len() == 2 && d.len() == 2
            && [y, m, d].iter().all(|p| p.chars().all(|c| c.is_ascii_digit())))
}

/// Split into lowercase word tokens: whitespace, hyphens and slashes separate words, and
/// surrounding markdown/punctuation is trimmed so `**417` and `rules,` read as `417` / `rules`.
///
/// Two things are kept out of the number stream so they cannot pose as a count: an ISO date
/// becomes the single word `date`, and a word that ends in sentence punctuation (`3.`, `27,`,
/// `4:`) is followed by a [`BOUNDARY`] token, so a numbered-list marker ("3. Rules ...") or a
/// date closing a clause ("as of 2026-10-27, rules ...") never sits next to the noun it precedes.
fn words(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for raw in text.split_whitespace() {
        let core = raw.trim_matches(|c: char| !c.is_alphanumeric() && c != '~' && c != '+');
        if is_iso_date(core) {
            out.push("date".to_string());
        } else {
            for piece in core.split(|c: char| c == '-' || c == '/' || c == '\u{2013}') {
                let w = piece
                    .trim_matches(|c: char| !c.is_alphanumeric() && c != '~' && c != '+')
                    .to_lowercase();
                if !w.is_empty() {
                    out.push(w);
                }
            }
        }
        if raw.ends_with(['.', ',', ':', ';', '!', '?', ')']) && !out.is_empty() {
            out.push(BOUNDARY.to_string());
        }
    }
    out
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
            if i < back || toks[i - back] == BOUNDARY {
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

/// Every "N rules across M domains" claim, as (rules, domains, the matched words). A
/// descriptive "TOML" between the number and "rules" ("417 TOML rules across 22 domains") is
/// still the headline claim.
fn headline_claims(text: &str) -> Vec<(u64, u64, String)> {
    let toks = words(text);
    let mut out = Vec::new();
    for i in 1..toks.len() {
        if toks[i] != "rules" || toks.get(i + 1).map(String::as_str) != Some("across") {
            continue;
        }
        if toks.get(i + 3).map(String::as_str) != Some("domains") {
            continue;
        }
        let start = if toks[i - 1] == "toml" && i >= 2 {
            i - 2
        } else {
            i - 1
        };
        if let (Some(n), Some(m)) = (as_count(&toks[start]), as_count(&toks[i + 2])) {
            out.push((n, m, toks[start..=i + 3].join(" ")));
        }
    }
    out
}

/// The counts stated in the exact sentence shapes "N rules across M domains" (rule count, domain
/// count) and "N corpus rules" (rule count only), wherever they sit in the text, across line breaks.
fn sentence_claims(text: &str) -> Vec<(u64, Option<u64>, String)> {
    let toks = words(text);
    let mut out = Vec::new();
    for i in 0..toks.len() {
        let Some(n) = as_count(&toks[i]) else {
            continue;
        };
        let at = |k: usize| toks.get(i + k).map(String::as_str);
        if matches!(at(1), Some("rules")) && matches!(at(2), Some("across")) {
            if let Some(m) = at(3).and_then(as_count) {
                if matches!(at(4), Some("domains")) {
                    out.push((n, Some(m), toks[i..=i + 4].join(" ")));
                }
            }
        } else if matches!(at(1), Some("corpus")) && matches!(at(2), Some("rules")) {
            out.push((n, None, toks[i..=i + 2].join(" ")));
        }
    }
    out
}

/// Whether the repo-wide walk skips a directory: build output, vendored packages and every
/// hidden directory (VCS internals, tool state, nested worktrees), except `.github`, whose
/// templates a reader does see.
fn is_skipped_dir(name: &str) -> bool {
    matches!(name, "target" | "node_modules") || (name.starts_with('.') && name != ".github")
}

/// Every markdown file under the repo root, as a repo-relative path with `/` separators,
/// sorted so a failure lists them in a stable order.
fn markdown_docs() -> Vec<String> {
    let root = repo_root();
    let mut out = Vec::new();
    let mut stack = vec![root.clone()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("cannot list {}: {e}", dir.display()));
        for entry in entries {
            let entry = entry.unwrap_or_else(|e| panic!("cannot read an entry: {e}"));
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            let file_type = entry
                .file_type()
                .unwrap_or_else(|e| panic!("cannot stat {}: {e}", path.display()));
            if file_type.is_dir() {
                if !is_skipped_dir(&name) {
                    stack.push(path);
                }
            } else if file_type.is_file() && name.ends_with(".md") {
                let rel = path
                    .strip_prefix(&root)
                    .expect("walked paths sit under the repo root")
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join("/");
                out.push(rel);
            }
        }
    }
    out.sort();
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

    for doc in SENTENCE_CHECKED_DOCS {
        let text = std::fs::read_to_string(repo_root().join(doc))
            .unwrap_or_else(|e| panic!("cannot read {doc}: {e}"));
        for (n, m, ctx) in sentence_claims(&text) {
            if n != rules as u64 {
                problems.push(format!(
                    "{doc}: says \"{ctx}\" but the corpus holds {rules} rules. \
                     Change {n} to {rules}."
                ));
            }
            if let Some(m) = m.filter(|m| *m != domains as u64) {
                problems.push(format!(
                    "{doc}: says \"{ctx}\" but the corpus holds {domains} domains. \
                     Change {m} to {domains}."
                ));
            }
        }
    }

    assert!(
        problems.is_empty(),
        "the public docs disagree with the corpus ({rules} rules across {domains} domains, \
         counted the way the loader counts them). Fix:\n  {}",
        problems.join("\n  ")
    );
}

#[tokio::test]
async fn req_every_doc_quoting_the_headline_states_the_real_counts() {
    let (rules, domains) = real_counts().await;
    assert!(rules > 0 && domains > 0, "the corpus loaded empty");

    let docs = markdown_docs();
    assert!(
        docs.iter().any(|d| d == "README.md"),
        "the repo-wide walk did not find README.md, so it is not walking the repo root"
    );

    let mut problems: Vec<String> = Vec::new();
    for doc in &docs {
        let text = std::fs::read_to_string(repo_root().join(doc))
            .unwrap_or_else(|e| panic!("cannot read {doc}: {e}"));
        for (n, m, ctx) in headline_claims(&text) {
            if n != rules as u64 || m != domains as u64 {
                problems.push(format!(
                    "{doc}: says \"{ctx}\". Change it to \"{rules} rules across {domains} domains\"."
                ));
            }
        }
    }

    assert!(
        problems.is_empty(),
        "docs quote a stale headline count (the corpus holds {rules} rules across {domains} \
         domains, counted the way the loader counts them). Fix:\n  {}",
        problems.join("\n  ")
    );
}

/// The headline scanner must see the shapes the docs actually use and nothing looser.
#[test]
fn req_headline_scanner_sees_only_the_exact_shape() {
    let seen = [
        (
            "The library holds 437 rules across 22 domains, each one",
            437,
            22,
        ),
        (
            "a corpus of **424 rules across 21 domains** covering",
            424,
            21,
        ),
        (
            "(**417 rules across 20 domains** as of 2026-10-11)",
            417,
            20,
        ),
        ("(for example \"1,200 rules across 30 domains\")", 1200, 30),
        ("a corpus of 417 TOML rules across 22 domains", 417, 22),
    ];
    for (text, want_rules, want_domains) in seen {
        let got = headline_claims(text);
        assert_eq!(
            got.first().map(|(n, m, _)| (*n, *m)),
            Some((want_rules, want_domains)),
            "headline scanner missed the claim in {text:?}"
        );
    }
    let ignored = [
        "all 417 rules were classified",
        "13 rules across `RULE_REGISTRY`",
        "enforces rules across four check layers",
        "437 rules across many stacks",
    ];
    for text in ignored {
        assert!(
            headline_claims(text).is_empty(),
            "headline scanner matched a shape it must ignore: {text:?}"
        );
    }
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

/// The sentence scanner must see the shapes the user guide uses, wrapped lines included, and
/// stay silent on numbered lists.
#[test]
fn req_sentence_scanner_reads_the_exact_shapes_only() {
    let wrapped = "the full corpus (**437 rules across 22 domains** as of today: Rust,\nPython) and 437\ncorpus rules are emit-only";
    let got = sentence_claims(wrapped);
    assert_eq!(got.len(), 2, "{got:?}");
    assert_eq!((got[0].0, got[0].1), (437, Some(22)));
    assert_eq!((got[1].0, got[1].1), (437, None));
    assert!(
        sentence_claims("1. Pick rules\n2. Add 3 rules to the repo\n4. Review rules").is_empty()
    );
    assert!(sentence_claims("26 of the rules across 3 repos").is_empty());
}

/// A date or a numbered-list marker next to the word "rules" is not a count. "as of
/// 2026-10-27, rules ..." and "3. Rules ..." must stay silent, while a real count right next
/// to them is still seen.
#[test]
fn req_count_scanner_ignores_dates_and_numbered_list_items() {
    let silent = [
        "As of 2026-10-27, rules are loaded from the bundled folder.",
        "As of 2026-10-27 rules are loaded from the bundled folder.",
        "1. Rules come first\n2. Domains follow\n3. Rules again",
        "Step 3. Rules are validated.",
        "See section 4: rules for each domain.",
    ];
    for text in silent {
        assert!(
            count_claims(text, &["rule", "rules"]).is_empty(),
            "count scanner read a date or list marker as a rule count in {text:?}"
        );
        assert!(
            count_claims(text, &["domain", "domains"]).is_empty(),
            "count scanner read a date or list marker as a domain count in {text:?}"
        );
    }
    let got = count_claims("As of 2026-10-27, the library holds 440 rules.", &["rule", "rules"]);
    assert_eq!(got.first().map(|(n, _)| *n), Some(440), "{got:?}");
    let got = count_claims("3. The library holds 440 rules across 22 domains", &["domains"]);
    assert_eq!(got.first().map(|(n, _)| *n), Some(22), "{got:?}");
    // The headline scanner must keep reading the shape through a trailing date.
    let got = headline_claims("(**417 rules across 20 domains** as of 2026-10-11)");
    assert_eq!(got.first().map(|(n, m, _)| (*n, *m)), Some((417, 20)));
}

/// The walk must skip build output and hidden tool state, and still read `.github`.
#[test]
fn req_markdown_walk_skips_build_output_and_hidden_dirs_only() {
    for skipped in ["target", "node_modules", ".git", ".claude", ".cache"] {
        assert!(is_skipped_dir(skipped), "{skipped} must be skipped");
    }
    for walked in [".github", "docs", "crates", "tools"] {
        assert!(!is_skipped_dir(walked), "{walked} must be walked");
    }
}
