//! W25 — the product never names itself in anything a client reads.
//!
//! Owner ruling (2026-10-20): "Camerata should not be addressed at all in anything client
//! facing. Camerata should be able to use the directives without it itself being mentioned by
//! name." No one else operates the engine, so there is no reader for whom the vendor name is
//! useful in a deliverable; a convention is stated in the client's terms, and how the engine
//! tracks or enforces it is the engine's own concern.
//!
//! Scope — CLIENT-FACING fields only:
//! - every option's `directive` (emitted into AGENTS.md/CONVENTIONS.md), `remediation` (the
//!   Fix line), `label`, `finding_headline`, `finding_detail`;
//! - a `violates` option's `why` (interpolated into the client's defect detail);
//! - the rule `title`, and every `[[sources]].title` (the citation block).
//!
//! Deliberately OUT of scope (author/operator, never shown to a client): `qualifies`,
//! `[decision].why`, `[decision].question`, `[[sources]].relevance`, and a NON-`violates`
//! option's `why`. The engine's own name legitimately lives in those — that is where the
//! "how Camerata tracks this" mechanics belong.

use camerata_rules::{corpus_path, load_corpus_lenient};

const VENDOR: &[&str] = &["Camerata", "camerata", ".camerata"];

/// Owner ruling (2026-10-20): the vendor name must not appear in ANY client-facing field,
/// including the `camerata:allow` suppression token. The earlier carve-out assumed the token
/// was "functionally required in client-facing text, the same way an eslint suppression names
/// eslint" — but that assumption is false for this product's delivery model: the client does
/// not run the engine (the operator runs the scan and hands over a report), so a client-facing
/// Fix line telling them to "add a `// camerata:allow` comment" names a tool they don't run and
/// an action they can't take. The suppression token's only legitimate homes are the engine
/// source (never shipped) and author/operator-facing fields (`qualifies`, `[decision].why`,
/// non-`violates` `why`) — never a field that reaches the product package.
fn names_vendor(text: &str) -> bool {
    VENDOR.iter().any(|v| text.contains(v))
}

#[tokio::test]
async fn req_w25_no_client_facing_field_names_the_vendor() {
    let (corpus, errors) = load_corpus_lenient(&corpus_path()).await;
    assert!(errors.is_empty(), "corpus must load cleanly: {errors:?}");

    let mut offenders: Vec<String> = Vec::new();
    let mut flag = |rid: &str, loc: String| offenders.push(format!("{rid}: {loc}"));

    for rule in corpus.iter() {
        if names_vendor(&rule.title) {
            flag(&rule.id.0, "title".into());
        }
        for s in &rule.sources {
            if names_vendor(&s.title) {
                flag(
                    &rule.id.0,
                    format!("source `{}`", s.title.chars().take(40).collect::<String>()),
                );
            }
        }
        for o in &rule.options {
            if names_vendor(&o.directive) {
                flag(&rule.id.0, format!("{}.directive", o.id));
            }
            if o.remediation.as_deref().is_some_and(names_vendor) {
                flag(&rule.id.0, format!("{}.remediation", o.id));
            }
            if names_vendor(&o.label) {
                flag(&rule.id.0, format!("{}.label", o.id));
            }
            if o.finding_headline.as_deref().is_some_and(names_vendor) {
                flag(&rule.id.0, format!("{}.finding_headline", o.id));
            }
            if o.finding_detail.as_deref().is_some_and(names_vendor) {
                flag(&rule.id.0, format!("{}.finding_detail", o.id));
            }
            // A violates option's `why` reaches the client's defect detail; a non-violates
            // `why` is model/operator-facing and may name the engine.
            if o.violates && names_vendor(&o.why) {
                flag(&rule.id.0, format!("{}.why (violates)", o.id));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "the product must not name itself in client-facing text; state the convention in the \
         client's terms and move 'how the engine tracks it' to `qualifies`:\n  {}",
        offenders.join("\n  ")
    );
}
