//! W18: `[[sources]].relevance` — a real home for the per-source annotation that 19 authored
//! paragraphs had been writing into `qualifies`, where `RuleSource` silently dropped it.
//!
//! ## What W17 left open
//!
//! W17 measured 29 `qualifies` keys nested inside `[[sources]]` blocks and found they were two
//! different shapes. It fixed the harmful one (10 occurrences that were rule-level text one level
//! too deep, costing those rules their `Rule::summary`) and deliberately left the other:
//!
//! > 19 occurrences across 7 `go/testing/*.toml` rules that ALREADY carry a rule-level
//! > `qualifies`. These are not misplaced rule-level text at all: each one annotates the SOURCE it
//! > sits under. [...] Deleting 19 paragraphs of authored prose, or adding a real
//! > `[[sources]].note` field for them, is a corpus-content decision for the owner.
//!
//! The owner ruled: *"What is this field again? I'd rather retain it if it would be useful."*
//! Keep the content, give it a field. W18 is that field.
//!
//! ## Why the content is worth a field
//!
//! Every one of the 19 says what the cited source actually states and how that bears on the rule
//! — e.g. "cmd/go documents the //go:build directive: custom tags specified via -tags flag at
//! build/test time. A file with //go:build integration is only compiled when ...". That is
//! provenance a later reader needs in order to re-verify the citation without re-reading the
//! source. It is not a second rule summary and must never be concatenated into one.
//!
//! ## Why it is `relevance` and not `qualifies`, `note`, or a verb
//!
//! - **Not `qualifies`.** That is the rule-level "when this applies" summary (`Rule::summary`).
//!   Reusing the same key name at two levels for two unrelated meanings is precisely the
//!   ambiguity that produced the 29-occurrence defect in the first place.
//! - **Not `note`.** `[[stack_exception]].note` already exists and already means "internal
//!   documentation, never rendered" — a second, differently-scoped `note` would make the corpus
//!   vocabulary ambiguous, and the name constrains nothing, so the next author with a stray
//!   paragraph puts it there too.
//! - **Not a verb (`substantiates`, `grounds`).** `qualifies`/`violates` are the corpus's
//!   verb-shaped keys and both read as assertions ABOUT THE RULE; a verb here invites the same
//!   level confusion. `grounding` additionally collides with the `verification = "grounded"`
//!   vocabulary, where "grounded" is a status, not prose.
//!
//! `relevance` says exactly one thing — why this source is relevant to this rule — and cannot
//! be read as a statement about the rule itself.

use camerata_rules::{
    corpus_files_with_unknown_fields, corpus_rules_with_qualifies_nested_in_sources, load_corpus,
    DEFAULT_CORPUS_PATH,
};
use std::path::Path;

/// Write `files` into a `universal/` subdirectory of a fresh temp dir, the layout `load_corpus`
/// expects (domain is folder-derived). Same helper shape as the W14/W17 suites.
fn synthetic_corpus(files: &[(&str, String)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp dir");
    let domain = dir.path().join("universal");
    std::fs::create_dir_all(&domain).expect("mkdir universal");
    for (name, body) in files {
        std::fs::write(domain.join(name), body).expect("write fixture");
    }
    dir
}

/// The real shipped corpus, or `None` when it is not on disk (mirrors the W14/W17 suites, which
/// skip rather than fail so the crate stays testable in isolation).
fn corpus_dir() -> Option<&'static Path> {
    let p = Path::new(DEFAULT_CORPUS_PATH);
    p.is_dir().then_some(p)
}

/// W18, THE test: an annotation authored on a `[[sources]]` block survives the load.
///
/// THE RED PROOF. Before `RuleSource::relevance` existed this failed at runtime, not at compile
/// time — which is the whole defect in one line: serde does not reject a key the struct has no
/// field for, it drops it. The authored paragraph parsed, passed every gate, and was simply not
/// there afterwards. Actual output before the fix:
///
/// ```text
/// the annotation authored on the `[[sources]]` block is NOT in the loaded RuleSource —
/// serde dropped it silently, which is the W17/W18 defect:
/// [RuleSource { url: "https://pkg.go.dev/cmd/go#hdr-Build_constraints",
///  title: "cmd/go - Build constraints", linter: None }]
/// ```
///
/// Asserted through the `Debug` rendering rather than by naming the field, so the assertion is
/// about the TEXT SURVIVING rather than about one spelling of one field — it is still a real
/// assertion if the field is ever renamed, and it was a compilable, runnable red before the field
/// existed. The typed assertion follows it, because surviving into some field is necessary but
/// not sufficient: it has to land in the right one.
#[tokio::test]
async fn req_w18_a_source_annotation_survives_the_load() {
    let body = r#"
id = "W18-RELEVANCE-1"
title = "W18 source-annotation fixture"
domain = "universal"
layer = "universal"
enforcement = "structured"
kind = "defect"
default = false
verification = "grounded"

qualifies = "The rule-level text, which is about the RULE."

[[sources]]
url = "https://pkg.go.dev/cmd/go#hdr-Build_constraints"
title = "cmd/go - Build constraints"
relevance = "W18-SENTINEL: cmd/go documents the //go:build directive, which is what makes this rule's mechanism real."
"#;
    let dir = synthetic_corpus(&[("w18-relevance-1.toml", body.to_string())]);
    let set = load_corpus(dir.path()).await.expect("fixture loads");
    let rule = set
        .get_by_id("W18-RELEVANCE-1")
        .expect("fixture rule loads");

    let rendered = format!("{:?}", rule.sources);
    assert!(
        rendered.contains("W18-SENTINEL"),
        "the annotation authored on the `[[sources]]` block is NOT in the loaded RuleSource — \
         serde dropped it silently, which is the W17/W18 defect:\n{rendered}"
    );

    // And it landed in `relevance`, not in some other field that happens to hold the bytes.
    let source = rule
        .sources
        .first()
        .expect("the fixture authors one source");
    assert_eq!(
        source.relevance.as_deref(),
        Some(
            "W18-SENTINEL: cmd/go documents the //go:build directive, which is what makes this \
             rule's mechanism real."
        ),
        "the authored annotation must be `RuleSource::relevance`, verbatim"
    );

    // The rule-level `qualifies` is untouched by any of this: `summary` is still the rule's own
    // text, and the per-source annotation is NOT concatenated into it. That separation is the
    // entire reason the 19 could not simply be moved up a level.
    assert_eq!(
        rule.summary, "The rule-level text, which is about the RULE.",
        "`relevance` must not reach `Rule::summary` — the two fields describe different things"
    );
}

/// W18: a source with no `relevance` key loads with `None`, and an absent annotation is not an
/// error. The field is optional and most sources will never carry one, so a loader that required
/// it would reject 410 of the 417 shipped rules.
#[tokio::test]
async fn req_w18_relevance_is_optional() {
    let body = r#"
id = "W18-NO-RELEVANCE-1"
title = "W18 unannotated-source fixture"
domain = "universal"
layer = "universal"
enforcement = "structured"
kind = "defect"
default = false
verification = "grounded"

[[sources]]
url = "https://example.org"
title = "T"
linter = "golangci-lint: errcheck"
"#;
    let dir = synthetic_corpus(&[("w18-no-relevance-1.toml", body.to_string())]);
    let set = load_corpus(dir.path()).await.expect("fixture loads");
    let rule = set.get_by_id("W18-NO-RELEVANCE-1").expect("fixture loads");
    let source = rule.sources.first().expect("one source");
    assert_eq!(
        source.relevance, None,
        "an unannotated source must load with `relevance: None`, not fail and not invent text"
    );
    assert_eq!(
        source.linter.as_deref(),
        Some("golangci-lint: errcheck"),
        "the pre-existing source fields must be unaffected by the new one"
    );
}

/// W18: `relevance` is registered in `SOURCE_FIELDS`, so the unknown-field gate accepts it — and
/// a MISSPELLING of it still fires.
///
/// `RuleToml` deliberately does not set `serde(deny_unknown_fields)`;
/// `corpus_files_with_unknown_fields` does that job across seven vocabularies. A new field that
/// is not registered there would be reported as a typo on all 19 shipped uses, and a new field
/// registered carelessly (e.g. by widening the gate) would let `relevence` through to be dropped
/// exactly the way `qualifies` was. Both halves are asserted, because only asserting the first
/// would pass against a gate that had been disabled.
#[tokio::test]
async fn req_w18_the_unknown_field_gate_accepts_relevance_and_still_rejects_a_typo() {
    let fixture = |key: &str| {
        format!(
            r#"
id = "W18-VOCAB-1"
title = "W18 source-vocabulary fixture"
domain = "universal"
layer = "universal"
enforcement = "structured"
kind = "defect"
default = false
verification = "grounded"

[[sources]]
url = "https://example.org"
title = "T"
{key} = "Annotation text."
"#
        )
    };

    let dir = synthetic_corpus(&[("w18-vocab-1.toml", fixture("relevance"))]);
    let offenders = corpus_files_with_unknown_fields(dir.path())
        .await
        .expect("fixture dir is readable");
    assert!(
        offenders.is_empty(),
        "`relevance` is a declared `[[sources]]` field and must not be reported as unknown: \
         {offenders:#?}"
    );
    // Declaring a field accepted is pointless if the loader chokes on it.
    let set = load_corpus(dir.path()).await.expect("fixture loads");
    assert_eq!(set.iter().count(), 1, "the fixture rule must load");

    let dir = synthetic_corpus(&[("w18-vocab-1.toml", fixture("relevence"))]);
    let offenders = corpus_files_with_unknown_fields(dir.path())
        .await
        .expect("fixture dir is readable");
    assert_eq!(
        offenders.len(),
        1,
        "a misspelled `relevance` must still be reported — it is dropped silently otherwise, \
         which is the entire defect W18 exists to retire: {offenders:#?}"
    );
    assert!(
        offenders[0].contains("relevence") && offenders[0].contains("[[sources]][0]"),
        "the offender must name the typo and its location: {:?}",
        offenders[0]
    );
}

/// W18 GATE: no corpus rule authors `qualifies` inside a `[[sources]]` block, at all.
///
/// This is the general shape. The two pre-existing gates are both narrower:
/// `corpus_rules_with_qualifies_misplaced_into_sources` reports the shape but is ADVISORY (a
/// pinned roster, which accepted 19 of them), and `corpus_rules_losing_qualifies_text_to_sources`
/// fails the build only on the subset that ALSO lacks a rule-level `qualifies`. The 19 sat
/// precisely in the gap: build-legal, reported, and silently discarded for as long as anyone
/// cared to leave them there.
///
/// With `relevance` existing there is no longer any shape of nested `qualifies` that is anything
/// other than an author error, so the gap closes.
#[tokio::test]
async fn req_w18_no_corpus_rule_authors_qualifies_inside_a_sources_block() {
    let Some(dir) = corpus_dir() else {
        return;
    };
    let offenders = corpus_rules_with_qualifies_nested_in_sources(dir)
        .await
        .expect("corpus is readable");
    assert!(
        offenders.is_empty(),
        "{} `[[sources]]` block(s) author `qualifies`, which `RuleSource` does not carry and \
         therefore discards:\n  {}",
        offenders.len(),
        offenders.join("\n  ")
    );
}

/// W18 GATE FIRE PROOF: the gate fires on BOTH nested shapes — the one that loses the rule its
/// summary and the one that merely loses the annotation — and stays silent on correctly authored
/// files. Corpus-independent, so it cannot go inert now that the corpus is clean; that is the
/// failure mode `corpus_rules_with_qualifies_misplaced_into_sources` had no guard against.
#[tokio::test]
async fn req_w18_the_nested_qualifies_gate_fires_on_both_shapes() {
    // Shape 1: nested, with NO rule-level `qualifies` — W17's harmful shape, the rule's summary
    // falls back to `[decision].why`.
    let lost = r#"
id = "W18-NESTED-LOST-1"
title = "W18 nested-with-no-rule-level fixture"
domain = "universal"
layer = "universal"
enforcement = "structured"
kind = "defect"
default = false
verification = "grounded"

[[sources]]
url = "https://example.org"
title = "T"
qualifies = "Rule-level text written one level too deep."
"#;
    // Shape 2: nested, WITH a rule-level `qualifies` — the 19's shape. Nothing reaches
    // `Rule::summary`, so W17's build-failing gate stays silent; the annotation is still dropped.
    let annotated = r#"
id = "W18-NESTED-ANNOTATED-1"
title = "W18 nested-with-rule-level fixture"
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
qualifies = "A note about THIS source, which belongs in `relevance`."
"#;
    // Correctly authored: the same annotation, in the field that holds it.
    let correct = r#"
id = "W18-CORRECT-1"
title = "W18 correctly-authored fixture"
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
relevance = "A note about THIS source, in the field that holds it."
"#;
    let dir = synthetic_corpus(&[
        ("w18-nested-lost-1.toml", lost.to_string()),
        ("w18-nested-annotated-1.toml", annotated.to_string()),
        ("w18-correct-1.toml", correct.to_string()),
    ]);

    let offenders = corpus_rules_with_qualifies_nested_in_sources(dir.path())
        .await
        .expect("fixture dir is readable");
    assert_eq!(
        offenders.len(),
        2,
        "the gate must fire on BOTH nested shapes and only those: {offenders:#?}"
    );
    assert!(
        offenders.iter().any(|o| o.contains("W18-NESTED-LOST-1")),
        "the no-rule-level shape must fire: {offenders:#?}"
    );
    assert!(
        offenders
            .iter()
            .any(|o| o.contains("W18-NESTED-ANNOTATED-1")),
        "the per-source-annotation shape — the one the 19 were, which the pre-W18 gates let \
         through — must now fire: {offenders:#?}"
    );
    assert!(
        !offenders.iter().any(|o| o.contains("W18-CORRECT-1")),
        "a source using `relevance` is correct and must not fire: {offenders:#?}"
    );
    assert!(
        offenders.iter().all(|o| o.contains("relevance")),
        "every offender line must name the field to use instead, or the gate is a riddle: \
         {offenders:#?}"
    );
}

/// W18: the 19 migrated annotations are actually loaded off the real corpus, keyed by rule and by
/// the opening fragment of each paragraph.
///
/// Without this, every other test in this file would pass against a corpus in which the migration
/// deleted the text instead of moving it — the E2 inertness failure mode, where a mechanism is
/// fully "traced" and fully green while the data it operates on does not exist. Fragments rather
/// than whole paragraphs, the same choice (and for the same reason) as W17's `MOVED` table.
#[tokio::test]
async fn req_w18_the_nineteen_migrated_annotations_are_loaded_from_the_corpus() {
    let Some(dir) = corpus_dir() else {
        return;
    };
    /// (rule id, source index, opening fragment of the authored annotation).
    const MIGRATED: [(&str, usize, &str); 19] = [
        (
            "GO-TESTING-BLACK-BOX-PKG-TEST-1",
            0,
            "The testing package documentation describes the two legal package declarations",
        ),
        (
            "GO-TESTING-BLACK-BOX-PKG-TEST-1",
            1,
            "cmd/go documents: 'Files declaring package name_test are compiled",
        ),
        (
            "GO-TESTING-COLOCATED-TEST-FILES-1",
            1,
            "The official Go tutorial shows test files co-located with source files",
        ),
        (
            "GO-TESTING-COLOCATED-TEST-FILES-1",
            2,
            "go test documentation: 'Files whose names begin with _ or .",
        ),
        (
            "GO-TESTING-DETERMINISTIC-NO-TIME-SLEEP-1",
            0,
            "t.TempDir() returns a temporary directory unique to each test",
        ),
        (
            "GO-TESTING-DETERMINISTIC-NO-TIME-SLEEP-1",
            1,
            "t.Setenv(key, value) sets an environment variable for the test duration",
        ),
        (
            "GO-TESTING-DETERMINISTIC-NO-TIME-SLEEP-1",
            2,
            "t.Cleanup(f) registers f to run when the test ends in LIFO order",
        ),
        (
            "GO-TESTING-DETERMINISTIC-NO-TIME-SLEEP-1",
            3,
            "golangci-lint tparallel detects inappropriate usage of t.Parallel()",
        ),
        (
            "GO-TESTING-HELPER-T-HELPER-1",
            0,
            "Official testing package: 'Helper marks the calling function as a test helper",
        ),
        (
            "GO-TESTING-HELPER-T-HELPER-1",
            1,
            "Go test comments wiki: 'Call t.Helper() to attribute failures to the call site",
        ),
        (
            "GO-TESTING-HELPER-T-HELPER-1",
            2,
            "golangci-lint thelper: 'Detects tests helpers which do not start with",
        ),
        (
            "GO-TESTING-INTEGRATION-BUILD-TAGS-1",
            0,
            "cmd/go documents the //go:build directive: custom tags specified via -tags flag",
        ),
        (
            "GO-TESTING-INTEGRATION-BUILD-TAGS-1",
            1,
            "go test documentation: build tags (//go:build) control which files are compiled",
        ),
        (
            "GO-TESTING-MOCK-AT-INTERFACE-BOUNDARY-1",
            0,
            "testify/mock provides the canonical Go mock pattern",
        ),
        (
            "GO-TESTING-MOCK-AT-INTERFACE-BOUNDARY-1",
            1,
            "Go interfaces are satisfied implicitly",
        ),
        (
            "GO-TESTING-MOCK-AT-INTERFACE-BOUNDARY-1",
            2,
            "Interfaces should be defined by the consumer, not the producer",
        ),
        (
            "GO-TESTING-TABLE-DRIVEN-T-RUN-1",
            0,
            "Canonical Go Wiki guidance: 'Table driven testing is not a tool",
        ),
        (
            "GO-TESTING-TABLE-DRIVEN-T-RUN-1",
            1,
            "Official testing package docs for t.Run: each subtest has a hierarchical name",
        ),
        (
            "GO-TESTING-TABLE-DRIVEN-T-RUN-1",
            2,
            "Go testing wiki guidance: use t.Run subtests for table-driven tests",
        ),
    ];

    let set = load_corpus(dir).await.expect("the shipped corpus loads");
    for (id, index, fragment) in MIGRATED {
        let rule = set
            .get_by_id(id)
            .unwrap_or_else(|| panic!("{id} must be in the shipped corpus"));
        let source = rule
            .sources
            .get(index)
            .unwrap_or_else(|| panic!("{id} must have a source at index {index}"));
        let relevance = source.relevance.as_deref().unwrap_or_else(|| {
            panic!(
                "{id} sources[{index}] ({}) must carry the authored annotation in `relevance`; \
                 it was migrated out of a nested `qualifies`, not deleted",
                source.title
            )
        });
        assert!(
            relevance.starts_with(fragment),
            "{id} sources[{index}]: the migrated annotation must be the authored text verbatim, \
             opening {fragment:?}, got {relevance:?}"
        );
    }

    // The migration must not have invented annotations anywhere else, and must not have missed
    // one — but `relevance` is no longer exclusively the go/testing migration's.
    //
    // W25 (2026-10-20) moved authority GRADES out of client-facing source titles ("ASP.NET
    // Core — Middleware (low authority — …)") into `relevance`, which is author-facing and
    // reaches no artifact. That is the field's intended second use, not unreviewed spread, so
    // the count rises from 19 to 19 + 13. The guard's PURPOSE survives: a count other than the
    // reviewed total still fails, so a 33rd `relevance` appearing without being added here is
    // caught exactly as before. Each of the 13 is a source whose title carried an authority
    // grade; `w25_source_title_grading` pins that no title carries one anymore.
    const W25_AUTHORITY_NOTE_SOURCES: usize = 13;
    // Corpus text work (2026-10-28): sources added to replace a non-http or missing citation,
    // each carrying a note on what the source does and does not establish (for example, that a
    // cited page documents a mechanism while the rule's requirement is the project's own
    // policy). Same author-facing use as the W25 moves. Rules: PROC-PR-CONCURRENCY-1 (2),
    // UI-UTC-DATES-1 (2), JAVASCRIPT-ANGULAR-SMART-PRESENTATIONAL-PATTERN-1 (1),
    // SPIRIT-OPTIMIZE-1 (3: the CWE pages define weaknesses to avoid, not a build-in-by-default
    // directive, which is why that rule is labelled policy).
    const CORPUS_TEXT_SOURCE_NOTES: usize = 8;
    let total: usize = set
        .iter()
        .map(|r| r.sources.iter().filter(|s| s.relevance.is_some()).count())
        .sum();
    assert_eq!(
        total,
        19 + W25_AUTHORITY_NOTE_SOURCES + CORPUS_TEXT_SOURCE_NOTES,
        "expected the 19 migrated go/testing annotations plus the {W25_AUTHORITY_NOTE_SOURCES} \
         W25 authority-note moves plus the {CORPUS_TEXT_SOURCE_NOTES} corpus-text source notes; \
         a different count means the migration lost one, or `relevance` is spreading beyond \
         those three reviewed populations"
    );
}

/// W18: the 7 migrated rules' `summary` is unchanged by the migration.
///
/// The migration touched files whose rule-level `qualifies` is the text clients can see (through
/// `Rule::summary` -> the arm/emit fallback directive and the rule-drift baseline hash). Moving a
/// key inside a `[[sources]]` block must not disturb that, and "must not" is worth asserting
/// rather than assuming, because the hash makes a one-character change a visible drift event.
#[tokio::test]
async fn req_w18_the_migration_did_not_change_any_rule_summary() {
    let Some(dir) = corpus_dir() else {
        return;
    };
    /// (rule id, the opening fragment of the rule-level `qualifies` that is its summary).
    const UNCHANGED: [(&str, &str); 7] = [
        (
            "GO-TESTING-BLACK-BOX-PKG-TEST-1",
            "Go allows two package declarations in _test.go",
        ),
        (
            "GO-TESTING-COLOCATED-TEST-FILES-1",
            "Every Go source file foo.go has its unit tests",
        ),
        (
            "GO-TESTING-DETERMINISTIC-NO-TIME-SLEEP-1",
            "A Go test that calls time.Sleep is a flaky tes",
        ),
        (
            "GO-TESTING-HELPER-T-HELPER-1",
            "A test helper is any function that takes *test",
        ),
        (
            "GO-TESTING-INTEGRATION-BUILD-TAGS-1",
            "Integration tests — tests that require a live ",
        ),
        (
            "GO-TESTING-MOCK-AT-INTERFACE-BOUNDARY-1",
            "In Go, a type that satisfies an interface sati",
        ),
        (
            "GO-TESTING-TABLE-DRIVEN-T-RUN-1",
            "When a test exercises the same function across",
        ),
    ];
    let set = load_corpus(dir).await.expect("the shipped corpus loads");
    for (id, fragment) in UNCHANGED {
        let rule = set
            .get_by_id(id)
            .unwrap_or_else(|| panic!("{id} must be in the shipped corpus"));
        assert!(
            rule.summary.starts_with(fragment),
            "{id}: the migration must not have touched the rule-level `qualifies` that is this \
             rule's summary; expected it to open {fragment:?}, got {:?}",
            rule.summary
        );
    }
}
