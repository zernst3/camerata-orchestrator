//! A parsed rule serializes and reads back as the same rule (#560). A run keeps the rule library
//! its inspection loaded in this form (the server's `run_store::rule_snapshot`), and every later
//! export reads it back and recomputes the digest the run pinned, so the round trip must keep
//! every rule's content hash and the corpus digest exactly.

use camerata_rules::{corpus_path, corpus_version, load_corpus_lenient, Rule, RuleSet};

#[tokio::test]
async fn every_bundled_rule_reads_back_with_the_same_content_hash_and_corpus_digest() {
    let (corpus, errors) = load_corpus_lenient(&corpus_path()).await;
    assert!(errors.is_empty(), "the bundled corpus loads: {errors:?}");
    assert!(!corpus.is_empty());

    let rules: Vec<&Rule> = corpus.iter().collect();
    let json = serde_json::to_vec(&rules).expect("the rules serialize");
    let back: Vec<Rule> = serde_json::from_slice(&json).expect("the rules read back");

    assert_eq!(back.len(), corpus.len());
    for (before, after) in corpus.iter().zip(&back) {
        assert_eq!(before.id, after.id);
        assert_eq!(
            before.content_hash(),
            after.content_hash(),
            "{} changed in the round trip",
            before.id.0
        );
        assert_eq!(before.version, after.version);
        assert_eq!(before.last_reviewed, after.last_reviewed);
    }
    let mut set = RuleSet::default();
    for rule in back {
        set.push(rule);
    }
    assert_eq!(corpus_version(&set), corpus_version(&corpus));

    // A second round trip is byte for byte the first: the form is stable.
    let rules_again: Vec<&Rule> = set.iter().collect();
    assert_eq!(serde_json::to_vec(&rules_again).expect("serialize"), json);
}

#[test]
fn an_unknown_rule_kind_does_not_read() {
    let err = serde_json::from_str::<camerata_rules::RuleKind>("\"a_kind_from_the_future\"")
        .expect_err("an unknown kind is an error, never a default");
    assert!(err.to_string().contains("a_kind_from_the_future"), "{err}");
}
