//! Honest verification labels: a rule whose own sources concede the directive is project policy,
//! or rate every outside source as low authority, is `policy`, not `grounded`.
//!
//! A Fable audit found eight rules marked `grounded` that fail that test. `policy` still arms the
//! rule exactly like `grounded` (see `w17_policy_arms_the_rule.rs`); what changes is that the
//! citation story no longer claims an outside authority the sources do not give. Their sources
//! and notes are kept.

use camerata_rules::{corpus_path, load_corpus_lenient, Verification};

const RELABELLED_POLICY: [&str; 8] = [
    "ARCH-TRUNK-SYNC-1",
    "RUST-ENTITIES-6",
    "RUST-ENTITIES-13",
    "SPIRIT-DOC-DECISIONS-1",
    "ARCH-MIDDLEWARE-FIRST-1",
    "ORCH-PREREVIEW-1",
    "ORCH-TRAINING-CUTOFF-1",
    "PROC-STORY-DOCS-1",
];

#[tokio::test]
async fn the_eight_audited_rules_are_labelled_policy_and_still_arm() {
    let (corpus, errors) = load_corpus_lenient(&corpus_path()).await;
    assert!(errors.is_empty(), "corpus must load cleanly: {errors:?}");

    for id in RELABELLED_POLICY {
        let rule = corpus
            .get_by_id(id)
            .unwrap_or_else(|| panic!("{id} must be present in the bundled corpus"));
        assert_eq!(
            rule.verification(),
            Verification::Policy,
            "{id}: its own sources concede the directive is project policy (or rate every \
             outside source low authority), so it must not be labelled `grounded`"
        );
        assert!(
            !rule.sources.is_empty(),
            "{id}: relabelling keeps the rule's sources"
        );
        assert!(
            rule.is_auto_recommended(),
            "{id}: `policy` arms the rule exactly like `grounded`"
        );
    }
}
