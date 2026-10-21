//! W32 (rule metadata for report credibility) — security rules carry a recognised CWE code,
//! and every authored credibility code is well-formed.
//!
//! A finding that prints "CWE-89" next to a SQL-injection headline is exactly the
//! externally-recognised anchor the investor-side report research asked for; a security finding
//! with no such code reads as the auditor's ungrounded opinion. These are data-only corpus
//! guarantees — no detection change.

use camerata_rules::{
    corpus_path, load_corpus_lenient, rules_with_malformed_credibility_metadata,
    security_rules_without_cwe,
};

#[tokio::test]
async fn req_w32_every_security_rule_has_a_cwe() {
    let (corpus, errors) = load_corpus_lenient(&corpus_path()).await;
    assert!(errors.is_empty(), "corpus must load cleanly: {errors:?}");

    let offenders = security_rules_without_cwe(&corpus);
    assert!(
        offenders.is_empty(),
        "these SEC- rules carry no `cwe` code; a security finding needs a recognised anchor. \
         Add the CWE its [[sources]] already cite:\n  {}",
        offenders.join("\n  ")
    );
}

#[tokio::test]
async fn req_w32_credibility_metadata_is_well_formed() {
    let (corpus, errors) = load_corpus_lenient(&corpus_path()).await;
    assert!(errors.is_empty(), "corpus must load cleanly: {errors:?}");

    let offenders = rules_with_malformed_credibility_metadata(&corpus);
    assert!(
        offenders.is_empty(),
        "these rules carry a malformed `cwe` (must be `CWE-<digits>`) or `superseded_since` \
         (must be a 4-digit year); it would render a broken anchor in the client report:\n  {}",
        offenders.join("\n  ")
    );
}
