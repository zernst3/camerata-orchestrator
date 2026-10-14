//! W17 item 3: the 10 `go/` rules whose authored `qualifies` was scoped into a `[[sources]]`
//! block, discarded by `RuleSource`, and silently replaced in `Rule::summary` by
//! `[decision].why`.
//!
//! ## The measurement, re-done
//!
//! W14 reported 29 occurrences across 17 `go/` rule files, 10 of them on rules with no
//! rule-level `qualifies`. All three counts confirmed. But the 29 are TWO shapes, and only one of
//! them is a defect:
//!
//! - **10 occurrences / 10 `go/*.toml` rules / NO rule-level `qualifies`.** One nested occurrence
//!   each, and in every case the text is rule-level in substance ("A function's parameters should
//!   use narrow interfaces…", "Enforced by a static-analysis gate…"). The rule shipped a summary
//!   its author did not write. FIXED: moved to the rule level, which is what this file proves.
//! - **19 occurrences / 7 `go/testing/*.toml` rules that ALREADY carry a correct rule-level
//!   `qualifies`.** 2 to 4 per file, each annotating the SOURCE it sits under ("t.TempDir()
//!   returns a temporary directory unique to each test…", "golangci-lint thelper: 'Detects tests
//!   helpers which do not start with the t.Helper() method.'"). NOT moved, and the rule-level one
//!   is the text kept, for three reasons: nothing is lost (summary is already correct for all 7);
//!   they cannot all move, since TOML forbids the 2-to-4 duplicate rule-level keys that would
//!   require; and folding per-source citation detail into a rule's derived summary would corrupt
//!   text that reads correctly today. Deleting 19 paragraphs of authored prose, or adding a real
//!   `[[sources]].note` field to hold them, is a corpus-content decision for the owner —
//!   `req_w14_the_misplaced_qualifies_roster_has_not_grown` pins them at 19/7 meanwhile.
//!
//!   **Superseded by W18** (prose only; no assertion in this file changed). The owner ruled
//!   "retain it if it would be useful", so `[[sources]]` got a real field — `relevance`, not
//!   `note` — and all 19 were migrated into it. The corpus now carries ZERO nested `qualifies`
//!   and `crates/rules/tests/w18_source_relevance.rs` fails the build on any new one. The
//!   paragraph above is kept as the record of WHY the 19 could not simply move up a level,
//!   which is still the reason they needed a field of their own.
//!
//! ## Why `summary` matters
//!
//! `Rule::summary` is derived `qualifies` -> `[decision].why` -> `title` (lib.rs, "Derive
//! summary"). It is the FALLBACK directive text for a rule with no resolvable option: the
//! arm/emit path (`server/src/lib.rs`), the rule-drift diff's `resolved_directive`, and the
//! rule-drift baseline HASH all read it. So for these 10 rules the wrong text was both emitted
//! and hashed — and the failure was perfectly silent, because a `qualifies` key in the wrong
//! table is not a parse error, not a gate failure, and not a warning. It is simply not there
//! (gotcha 9).

use camerata_rules::{
    corpus_rules_losing_qualifies_text_to_sources, load_corpus, RuleSet, DEFAULT_CORPUS_PATH,
};

/// The 10 rules whose authored `qualifies` W17 moved to the rule level, each with a distinctive
/// opening fragment of that authored text.
///
/// Fragments rather than whole paragraphs, and fragments rather than nothing: asserting only
/// "summary != decision.why" would pass if someone replaced the authored text with anything at
/// all, and asserting the full paragraph would make this test a copy of the corpus that has to be
/// re-pasted on every wording tweak. The opening clause is the part that identifies WHICH text
/// won.
const MOVED: [(&str, &str); 10] = [
    (
        "GO-ACCEPT-INTERFACES-RETURN-STRUCTS-1",
        "A function's parameters should use narrow interfaces (e.g.",
    ),
    (
        "GO-CONTEXT-PROPAGATION-1",
        "A context.Context carrying a deadline, timeout, or cancell",
    ),
    (
        "GO-ERROR-WRAPPING-WITH-FMT-W-1",
        "When an error is returned from a lower layer and needs con",
    ),
    (
        "GO-ERRORS-MUST-BE-CHECKED-1",
        "Enforced by a static-analysis gate (errcheck, golangci-lin",
    ),
    (
        "GO-GOROUTINE-LEAKS-DEFER-CLEANUP-1",
        "A deferred function is paired with every resource acquire ",
    ),
    (
        "GO-LOGGING-STRUCTURED-1",
        "Structured logging (JSON or key-value format) is easier to",
    ),
    (
        "GO-NO-GOROUTINE-GLOBALS-1",
        "Package-level variables that are not read-only or protecte",
    ),
    (
        "GO-PACKAGE-BOUNDARIES-CLEAR-1",
        "Circular imports are caught at build time and cause compil",
    ),
    (
        "GO-SMALL-INTERFACES-1",
        "Small, focused interfaces are easier to implement, mock, a",
    ),
    (
        "GO-SQL-PARAMETERIZED-1",
        "Enforced by a static-analysis gate (Bandit-equivalent or s",
    ),
];

async fn corpus() -> Option<RuleSet> {
    let path = std::path::Path::new(DEFAULT_CORPUS_PATH);
    if !path.exists() {
        return None;
    }
    Some(load_corpus(path).await.expect("corpus loads"))
}

fn corpus_dir() -> Option<&'static std::path::Path> {
    let path = std::path::Path::new(DEFAULT_CORPUS_PATH);
    path.exists().then_some(path)
}

/// W17 item 3, THE test: each of the 10 rules' `summary` is now its authored `qualifies` and is
/// NOT `[decision].why`.
///
/// Both halves are asserted per rule, and both are needed. "summary starts with the authored
/// fragment" proves the authored text won; "summary != decision_why" proves the fallback is no
/// longer what is being read. Either alone could pass for the wrong reason — a rule whose
/// `[decision].why` happened to open with the same words would satisfy the first, and any
/// non-empty `qualifies` would satisfy the second.
///
/// FAILS BEFORE THE FIX: every one of the 10 returns `[decision].why`. Proven by
/// `git stash push -- crates/rules/principles/` and re-running: 10 failures, each reporting the
/// `[decision].why` paragraph where the authored `qualifies` is expected.
#[tokio::test]
async fn req_w17_the_ten_moved_rules_summarise_with_their_authored_qualifies() {
    let Some(set) = corpus().await else {
        return;
    };
    for (id, fragment) in MOVED {
        let rule = set
            .get_by_id(id)
            .unwrap_or_else(|| panic!("{id} must be present in the bundled corpus"));

        assert!(
            rule.summary.starts_with(fragment),
            "{id}: `summary` must be the AUTHORED `qualifies`, which opens {fragment:?}.\n\
             Got: {:?}\n\
             A `qualifies` authored below a `[[sources]]` header is scoped into the source table \
             and discarded by `RuleSource`, so `summary` falls back to `[decision].why` with no \
             error of any kind (gotcha 9).",
            rule.summary
        );

        let why = rule.decision_why.as_deref().unwrap_or_else(|| {
            panic!("{id} has a `[decision].why`; it is the fallback under test")
        });
        assert_ne!(
            rule.summary.trim(),
            why.trim(),
            "{id}: `summary` must no longer BE `[decision].why` — that equality is exactly the \
             silent fallback this fix removes"
        );
    }
}

/// W17 item 3, the corpus-wide gate: NO rule anywhere authors `qualifies` inside a `[[sources]]`
/// block while having no rule-level `qualifies`. That pairing is the only harmful shape, and it
/// now fails the build.
///
/// Scoped to the harmful shape deliberately. The 19 per-source annotations on the 7
/// `go/testing/` rules are left as authored and pinned by
/// `req_w14_the_misplaced_qualifies_roster_has_not_grown`; failing the build on them too would
/// force a corpus-content decision (delete the prose, or add a real `[[sources]].note` field)
/// that the owner has not made. See `SOURCE_FIELDS`' doc comment.
///
/// FAILS BEFORE THE FIX: 10 offenders, one per `go/*.toml` rule above.
#[tokio::test]
async fn req_w17_no_corpus_rule_loses_its_qualifies_text_to_a_sources_block() {
    let Some(dir) = corpus_dir() else {
        return;
    };
    let offenders = corpus_rules_losing_qualifies_text_to_sources(dir)
        .await
        .expect("corpus is readable");
    assert!(
        offenders.is_empty(),
        "{} rule(s) author `qualifies` inside `[[sources]]` with no rule-level `qualifies`, so \
         they ship a summary their author did not write:\n  {}",
        offenders.len(),
        offenders.join("\n  ")
    );
}

/// W17 item 3 FIRE PROOF: the gate reports the harmful shape and stays silent on the benign one.
/// Corpus-independent, so the gate cannot go inert once the corpus is clean — which, the corpus
/// now being clean, is the only way it could be proven to work at all.
///
/// Three cases, because the distinction between them IS the fix's reasoning: nested with no
/// rule-level key FIRES (text lost); nested WITH a rule-level key does not (the per-source
/// annotation shape, 19 of which ship deliberately); and a correctly authored rule-level
/// `qualifies` also does not, with its `summary` proven to be that text.
#[tokio::test]
async fn req_w17_the_losing_qualifies_gate_fires_only_on_the_harmful_shape() {
    let harmful = r#"
id = "W17-Q-LOST-1"
title = "W17 lost-qualifies fixture"
domain = "universal"
layer = "universal"
enforcement = "structured"
kind = "defect"
default = false
verification = "grounded"

[decision]
why = "The fallback text, which must NOT become this rule's summary."

[[sources]]
url = "https://example.org"
title = "T"
qualifies = "The authored text the engine throws away."
"#;
    let benign = r#"
id = "W17-Q-ANNOTATED-1"
title = "W17 per-source-annotation fixture"
domain = "universal"
layer = "universal"
enforcement = "structured"
kind = "defect"
default = false
verification = "grounded"

qualifies = "The rule-level text the engine reads."

[[sources]]
url = "https://example.org"
title = "T"
qualifies = "A note about THIS source specifically, not about the rule."
"#;
    let dir = tempfile::tempdir().expect("temp dir");
    let universal = dir.path().join("universal");
    std::fs::create_dir_all(&universal).expect("mkdir universal");
    std::fs::write(universal.join("w17-q-lost-1.toml"), harmful).expect("write");
    std::fs::write(universal.join("w17-q-annotated-1.toml"), benign).expect("write");

    let offenders = corpus_rules_losing_qualifies_text_to_sources(dir.path())
        .await
        .expect("fixture dir is readable");
    assert_eq!(
        offenders.len(),
        1,
        "the gate must fire on exactly the rule with no rule-level `qualifies`: {offenders:#?}"
    );
    assert!(
        offenders[0].contains("W17-Q-LOST-1"),
        "the offender must be the rule that lost its text: {:?}",
        offenders[0]
    );
    assert!(
        !offenders.iter().any(|o| o.contains("W17-Q-ANNOTATED-1")),
        "a per-source annotation on a rule WITH rule-level `qualifies` loses nothing and must \
         not fire: {offenders:#?}"
    );

    // And the behaviour the gate is a proxy for, asserted directly on both fixtures.
    let set = load_corpus(dir.path()).await.expect("fixtures load");
    let lost = set.get_by_id("W17-Q-LOST-1").expect("fixture loads");
    assert_eq!(
        lost.summary, "The fallback text, which must NOT become this rule's summary.",
        "the harmful shape's summary IS `[decision].why` — that is the defect, demonstrated"
    );
    let annotated = set.get_by_id("W17-Q-ANNOTATED-1").expect("fixture loads");
    assert_eq!(
        annotated.summary, "The rule-level text the engine reads.",
        "the benign shape's summary is its rule-level `qualifies`; the nested note is ignored, \
         which is why leaving the 19 in place changes nothing a client sees"
    );
}
