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
//! Deliberately OUT of scope (author/operator-facing, never shown to a client): `qualifies`,
//! `[decision].why`, `[decision].question`, `[[sources]].relevance`, and a NON-`violates`
//! option's `why`.
//!
//! The replacement is by meaning (a colon for an explanation or title separator, a comma for an
//! aside, parentheses for a parenthetical), never a hyphen standing in for the dash.

use camerata_rules::{corpus_path, load_corpus_lenient, RuleSet};

/// U+2014, spelled as an escape so this file does not itself contain the character.
const EM_DASH: char = '\u{2014}';

fn has_em_dash(text: &str) -> bool {
    text.contains(EM_DASH)
}

/// Every `<rule id>: <field>` whose text carries an em dash, over the client-facing fields.
fn em_dash_offenders(corpus: &RuleSet) -> Vec<String> {
    let mut offenders: Vec<String> = Vec::new();
    let mut flag = |rid: &str, loc: String| offenders.push(format!("{rid}: {loc}"));

    for rule in corpus.iter() {
        if has_em_dash(&rule.title) {
            flag(&rule.id.0, "title".into());
        }
        if rule.options.is_empty() && has_em_dash(&rule.summary) {
            flag(&rule.id.0, "summary (option-less rule)".into());
        }
        for s in &rule.sources {
            if has_em_dash(&s.title) {
                flag(
                    &rule.id.0,
                    format!("source `{}`", s.title.chars().take(40).collect::<String>()),
                );
            }
        }
        for o in &rule.options {
            if has_em_dash(&o.directive) {
                flag(&rule.id.0, format!("{}.directive", o.id));
            }
            if o.remediation.as_deref().is_some_and(has_em_dash) {
                flag(&rule.id.0, format!("{}.remediation", o.id));
            }
            if has_em_dash(&o.label) {
                flag(&rule.id.0, format!("{}.label", o.id));
            }
            if o.finding_headline.as_deref().is_some_and(has_em_dash) {
                flag(&rule.id.0, format!("{}.finding_headline", o.id));
            }
            if o.finding_detail.as_deref().is_some_and(has_em_dash) {
                flag(&rule.id.0, format!("{}.finding_detail", o.id));
            }
            if o.violates && has_em_dash(&o.why) {
                flag(&rule.id.0, format!("{}.why (violates)", o.id));
            }
        }
    }
    offenders
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
