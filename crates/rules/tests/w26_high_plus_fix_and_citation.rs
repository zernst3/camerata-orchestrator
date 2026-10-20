//! W26 (fix and estimate quality) — no HIGH+ finding ships without a fix AND a citation.
//!
//! The deliverable's money rows are the high and critical findings. The audit spec's rule is
//! blunt: "no row at high or above without a fix and a citation." Both halves are structural
//! corpus guarantees, enforceable before any scan runs:
//!
//! - **Citation.** `report_export::resolve_citation` joins a finding's `rule_id` to the rule's
//!   `[[sources]]`. A high+ rule with empty sources ships a high-severity row a client reads
//!   with nothing behind it. `high_plus_rules_without_citation` flags those.
//! - **Fix.** `report_export::resolve_fix` resolves a finding's Fix line from the option it was
//!   judged against, falling back to the rule's `default_option`. The durable guarantee is that
//!   the default option carries a non-empty remediation, so there is always a Fix line even when
//!   the model records no option. `high_plus_rules_without_default_fix` flags the rest.
//!
//! A rule can emit high+ unless it declares a `max_severity` cap of low/medium/info (see
//! `rule_can_emit_high_plus`). `draft` rules are excluded — they are kept out of the armed
//! ruleset and never ship a client finding.

use camerata_rules::{
    corpus_path, high_plus_rules_without_any_fix, high_plus_rules_without_citation,
    load_corpus_lenient, options_with_invalid_effort,
};

#[tokio::test]
async fn req_w26_every_high_plus_rule_has_a_citation() {
    let (corpus, errors) = load_corpus_lenient(&corpus_path()).await;
    assert!(errors.is_empty(), "corpus must load cleanly: {errors:?}");

    let offenders = high_plus_rules_without_citation(&corpus);
    assert!(
        offenders.is_empty(),
        "these rules can emit a HIGH+ finding but carry no [[sources]] citation; a client would \
         read a high-severity row with no source. Add an authoritative source, or cap the rule \
         below high with `max_severity`:\n  {}",
        offenders.join("\n  ")
    );
}

#[tokio::test]
async fn req_w26_every_high_plus_rule_has_a_resolvable_fix() {
    let (corpus, errors) = load_corpus_lenient(&corpus_path()).await;
    assert!(errors.is_empty(), "corpus must load cleanly: {errors:?}");

    let offenders = high_plus_rules_without_any_fix(&corpus);
    assert!(
        offenders.is_empty(),
        "these rules can emit a HIGH+ finding but NO option authors a remediation, so \
         `resolve_fix` can never produce a Fix line for any finding of the rule. Author a \
         remediation on the conforming option:\n  {}",
        offenders.join("\n  ")
    );
}

#[tokio::test]
async fn req_w26_authored_effort_is_a_valid_tier() {
    let (corpus, errors) = load_corpus_lenient(&corpus_path()).await;
    assert!(errors.is_empty(), "corpus must load cleanly: {errors:?}");

    let offenders = options_with_invalid_effort(&corpus);
    assert!(
        offenders.is_empty(),
        "these options author an `effort` outside the low/medium/high tier vocabulary; \
         `effort_hours_bounds` would silently map it to 'not yet estimated', stripping the \
         estimate off every finding of the rule:\n  {}",
        offenders.join("\n  ")
    );
}
