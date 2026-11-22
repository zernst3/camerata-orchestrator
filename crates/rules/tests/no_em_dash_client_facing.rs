//! House style: client-facing text carries no em dashes.
//!
//! A rule's text reaches a client in the report's citation block, its "Fix:" line, the defect
//! headline and detail, and in the AGENTS.md / CONVENTIONS.md directives. Those are the same
//! fields `w25_no_vendor_name_client_facing.rs` treats as client-facing, and this gate uses the
//! same set so the two cannot drift apart:
//! - the rule `title` and every `[[sources]].title` (the citation block);
//! - every option's `directive`, `remediation`, `label`, `finding_headline` and `finding_detail`;
//! - a `violates` option's `why` (interpolated into the client's defect detail);
//! - the rule `summary`, but ONLY for a rule with no options, because
//!   `resolved_directive` falls back to it when there is no directive to show.
//!
//! The field walk itself lives in `tests/common/mod.rs`, shared with the serial-comma gate.
//!
//! Deliberately OUT of scope (author/operator-facing, never shown to a client): `qualifies`,
//! `[decision].why`, `[decision].question`, `[[sources]].relevance`, and a NON-`violates`
//! option's `why`.
//!
//! The replacement is by meaning (a colon for an explanation or title separator, a comma for an
//! aside, parentheses for a parenthetical), never a hyphen standing in for the dash.

mod common;

use camerata_rules::{corpus_path, load_corpus_lenient, RuleSet};
use common::{client_facing_fields, client_reaching_fields};

/// U+2014, spelled as an escape so this file does not itself contain the character.
const EM_DASH: char = '\u{2014}';

fn has_em_dash(text: &str) -> bool {
    text.contains(EM_DASH)
}

/// Every `<rule id>: <field>` whose text carries an em dash, over the client-facing fields.
fn em_dash_offenders(corpus: &RuleSet) -> Vec<String> {
    client_facing_fields(corpus)
        .into_iter()
        .filter(|f| has_em_dash(f.text))
        .map(|f| format!("{}: {}", f.rule_id, f.loc))
        .collect()
}

#[tokio::test]
async fn no_client_facing_rule_field_carries_an_em_dash() {
    let (corpus, errors) = load_corpus_lenient(&corpus_path()).await;
    assert!(errors.is_empty(), "corpus must load cleanly: {errors:?}");
    assert!(
        corpus.iter().count() > 100,
        "vacuity guard: the corpus loaded too few rules for this gate to mean anything"
    );

    let offenders = em_dash_offenders(&corpus);
    assert!(
        offenders.is_empty(),
        "client-facing rule text must carry no em dash; use a colon for an explanation or title \
         separator, a comma for an aside, parentheses for a parenthetical ({} offenders):\n  {}",
        offenders.len(),
        offenders.join("\n  ")
    );
}

/// Every dash-like character a writer can use as a dash: U+2012 figure dash, U+2013 en dash,
/// U+2014 em dash, U+2015 horizontal bar, U+2212 minus sign.
const DASHES: [char; 5] = ['\u{2012}', '\u{2013}', EM_DASH, '\u{2015}', '\u{2212}'];

/// Every `<rule id>: <field>` over EVERY client-reaching field (see
/// `common::client_reaching_fields`) whose text carries any of [`DASHES`].
fn dash_offenders_everywhere(corpus: &RuleSet) -> Vec<String> {
    client_reaching_fields(corpus)
        .into_iter()
        .filter(|f| f.text.contains(DASHES))
        .map(|f| format!("{}: {}", f.rule_id, f.loc))
        .collect()
}

#[tokio::test]
async fn no_client_reaching_rule_field_carries_a_dash() {
    let (corpus, errors) = load_corpus_lenient(&corpus_path()).await;
    assert!(errors.is_empty(), "corpus must load cleanly: {errors:?}");
    assert!(corpus.iter().count() > 100, "vacuity guard");
    let offenders = dash_offenders_everywhere(&corpus);
    assert!(
        offenders.is_empty(),
        "text that can reach a client must carry no em dash, en dash or look-alike ({} \
         offenders):\n  {}",
        offenders.len(),
        offenders.join("\n  ")
    );
}

/// The wide gate must be able to fail: a dash planted in every field of the wide list is
/// reported for each one (sources included), and the author-facing `relevance` is not.
#[tokio::test]
async fn the_wide_dash_gate_reports_a_dash_planted_in_every_client_reaching_field() {
    for dash in DASHES {
        let (_dir, corpus) = common::load_planted(&format!("a {dash} b")).await;
        let offenders = dash_offenders_everywhere(&corpus);
        for expected in common::PLANTED_LOCATIONS
            .iter()
            .chain(common::PLANTED_SOURCE_LOCATIONS)
        {
            assert!(
                offenders.iter().any(|o| o.contains(expected)),
                "the gate failed to flag `{expected}` for U+{:04X}: {offenders:#?}",
                dash as u32
            );
        }
        assert!(
            !offenders.iter().any(|o| o.contains("relevance")),
            "relevance is author-facing and must not be flagged: {offenders:#?}"
        );
    }
}

/// The gate must be able to fail: a synthetic rule with a dash in each client-facing slot is
/// flagged in every one of them, and a dash in an operator-facing slot is not.
#[tokio::test]
async fn the_em_dash_gate_fires_on_every_client_facing_field_and_ignores_operator_fields() {
    let dash = EM_DASH;
    let body = format!(
        r#"id = "SYNTH-DASH-1"
title = "Title {dash} with a dash"
enforcement = "prose"
kind = "decision"
inspectable = true
default = true
verification = "policy"
qualifies = "Operator text {dash} may keep its dash."

[[sources]]
url = "https://example.com/x"
title = "Docs {dash} source"
relevance = "Operator text {dash} may keep its dash."

[decision]
question = "Operator question {dash} may keep its dash?"
default = "the-correct-option"
why = "Operator text {dash} may keep its dash."

[[option]]
id = "the-correct-option"
label = "Label {dash} dash"
directive = "Directive {dash} dash."
remediation = "Remediation {dash} dash."
finding_headline = "Headline {dash} dash."
finding_detail = "Detail {dash} dash."
why = "Non-violates why {dash} may keep its dash."

[[option]]
id = "the-defect-option"
violates = true
label = "Clean label"
directive = "Clean directive."
why = "Violates why {dash} dash."
"#
    );
    let dir = tempfile::tempdir().expect("temp dir");
    let universal = dir.path().join("universal");
    std::fs::create_dir_all(&universal).expect("mkdir universal");
    std::fs::write(universal.join("synth-dash-1.toml"), body).expect("write fixture");

    let (corpus, errors) = load_corpus_lenient(dir.path()).await;
    assert!(errors.is_empty(), "fixture must load: {errors:?}");

    let offenders = em_dash_offenders(&corpus);
    for expected in [
        "title",
        "source `Docs",
        "the-correct-option.directive",
        "the-correct-option.remediation",
        "the-correct-option.label",
        "the-correct-option.finding_headline",
        "the-correct-option.finding_detail",
        "the-defect-option.why (violates)",
    ] {
        assert!(
            offenders.iter().any(|o| o.contains(expected)),
            "the gate failed to flag `{expected}`: {offenders:#?}"
        );
    }
    assert_eq!(
        offenders.len(),
        8,
        "operator-facing fields must not be flagged: {offenders:#?}"
    );
}
