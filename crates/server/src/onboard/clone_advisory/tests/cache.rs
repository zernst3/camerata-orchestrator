//! The content-keyed verdict cache: hits, misses and what is never cached.

use super::*;

const RULE: CloneRule = CloneRule::SingleSourceLogic;

async fn judge_with(
    llm: Option<&dyn LlmPort>,
    all: &[(String, String)],
    cache: &mut VerdictCache,
    model: &str,
) -> JudgeOutcome {
    let mut req = request(all, set_for(all, &[RULE]));
    req.judge_model = model;
    judge_clone_candidates(llm, req, cache, None, None).await
}

#[tokio::test]
async fn cached_verdict_is_reused_without_a_model_call() {
    let all = discount_repo("0.2", "0.15");
    let mut cache = VerdictCache::default();
    let first = StubJudge::answering("same_decision", "the premium discount calculation");
    let a = judge_with(Some(&first), &all, &mut cache, JUDGE).await;
    assert_eq!(first.call_count(), 1);
    assert_eq!(a.findings.len(), 1);
    assert_eq!(cache.len(), 1);

    // A re-scan of the unchanged tree: the same finding, no call, no spend.
    let second = StubJudge::new(|_, _| panic!("a cached verdict must not be re-asked"));
    let b = judge_with(Some(&second), &all, &mut cache, JUDGE).await;
    assert_eq!(second.call_count(), 0);
    assert_eq!(b.model_calls, 0);
    assert_eq!(b.findings.len(), 1);
    assert_eq!(
        b.findings[0].detail, a.findings[0].detail,
        "the same concept, from the cache"
    );
    assert_eq!(b.counts[&RULE].cached, 1);
    assert_eq!(b.counts[&RULE].judged, 1);
    assert_eq!(b.judge_model, None, "no call, no model stamped");
}

#[tokio::test]
async fn a_cached_negative_verdict_is_reused_too() {
    let all = discount_repo("0.2", "0.15");
    let mut cache = VerdictCache::default();
    let first = StubJudge::answering("boilerplate", "");
    let _ = judge_with(Some(&first), &all, &mut cache, JUDGE).await;
    let second = StubJudge::new(|_, _| panic!("must not be re-asked"));
    let b = judge_with(Some(&second), &all, &mut cache, JUDGE).await;
    assert!(b.findings.is_empty());
    assert_eq!(b.counts[&RULE].rejected, 1);
    assert_eq!(b.counts[&RULE].cached, 1);
}

#[tokio::test]
async fn an_unsure_verdict_is_not_cached_and_is_retried_next_scan() {
    let all = discount_repo("0.2", "0.15");
    let mut cache = VerdictCache::default();
    let first = StubJudge::answering("unsure", "");
    let _ = judge_with(Some(&first), &all, &mut cache, JUDGE).await;
    assert!(cache.is_empty(), "unsure is not cached");
    let second = StubJudge::answering("same_decision", "the discount");
    let b = judge_with(Some(&second), &all, &mut cache, JUDGE).await;
    assert_eq!(second.call_count(), 1, "asked again");
    assert_eq!(b.findings.len(), 1);
}

#[tokio::test]
async fn a_failed_or_unparsed_batch_is_not_cached() {
    let all = discount_repo("0.2", "0.15");
    let mut cache = VerdictCache::default();
    let bad = StubJudge::new(|_, _| Ok("nonsense".to_string()));
    let _ = judge_with(Some(&bad), &all, &mut cache, JUDGE).await;
    assert!(cache.is_empty());
}

#[tokio::test]
async fn a_judge_model_change_invalidates_every_cached_verdict() {
    let all = discount_repo("0.2", "0.15");
    let mut cache = VerdictCache::default();
    let first = StubJudge::answering("different", "");
    let _ = judge_with(Some(&first), &all, &mut cache, "model-a").await;
    let other = StubJudge::answering("same_decision", "the x");
    let b = judge_with(Some(&other), &all, &mut cache, "model-b").await;
    assert_eq!(
        other.call_count(),
        1,
        "a different judge model is a different question"
    );
    assert_eq!(b.findings.len(), 1);
    // Both verdicts live side by side; going back to model-a hits again.
    assert_eq!(cache.len(), 2);
    let again = StubJudge::new(|_, _| panic!("model-a is cached"));
    let c = judge_with(Some(&again), &all, &mut cache, "model-a").await;
    assert!(c.findings.is_empty());
}

#[tokio::test]
async fn offline_never_reads_the_cache() {
    let all = discount_repo("0.2", "0.15");
    let mut cache = VerdictCache::default();
    let first = StubJudge::answering("same_decision", "the x");
    let _ = judge_with(Some(&first), &all, &mut cache, JUDGE).await;
    let offline = judge_with(None, &all, &mut cache, JUDGE).await;
    assert!(
        offline.findings.is_empty(),
        "no model, no findings, even from a cache"
    );
    assert_eq!(offline.not_assessed.len(), 1);
}

fn key_of(all: &[(String, String)], rule: CloneRule, model: &str) -> String {
    let set = set_for(all, &[rule]);
    cache_key(&set.by_rule[&rule][0], model)
}

#[test]
fn the_key_is_content_not_paths_so_a_moved_file_hits() {
    let a = discount_repo("0.2", "0.15");
    let moved = files(&[
        ("packages/web/cart.ts", &discount("applyDiscount", "0.2")),
        ("packages/api/summary.ts", &discount("quoteTotal", "0.15")),
    ]);
    assert_eq!(key_of(&a, RULE, JUDGE), key_of(&moved, RULE, JUDGE));
}

#[test]
fn the_key_changes_with_the_model_the_rule_and_the_differences() {
    let a = discount_repo("0.2", "0.15");
    let base = key_of(&a, RULE, JUDGE);
    assert_ne!(base, key_of(&a, RULE, "another-model"));
    assert_ne!(base, key_of(&a, CloneRule::NamingOneConcept, JUDGE));
    // A different disagreement is a different question.
    assert_ne!(base, key_of(&discount_repo("0.2", "0.18"), RULE, JUDGE));
    // Agreeing copies are a different question from disagreeing ones.
    assert_ne!(base, key_of(&discount_repo("0.2", "0.2"), RULE, JUDGE));
}

#[test]
fn a_family_whose_tokens_change_misses() {
    let a = discount_repo("0.2", "0.2");
    let longer = files(&[
        (
            "src/cart/total.ts",
            &discount("applyDiscount", "0.2")
                .replace("base + order.shipping", "base + order.shipping + order.tax"),
        ),
        (
            "src/checkout/summary.ts",
            &discount("quoteTotal", "0.2")
                .replace("base + order.shipping", "base + order.shipping + order.tax"),
        ),
    ]);
    assert_ne!(key_of(&a, RULE, JUDGE), key_of(&longer, RULE, JUDGE));
}

#[test]
fn unsure_and_empty_verdicts_are_refused_by_the_cache() {
    let mut cache = VerdictCache::default();
    for word in ["unsure", ""] {
        assert!(!cache.insert(
            "k".to_string(),
            CachedVerdict {
                verdict: word.to_string(),
                concept: String::new(),
                judged_at: "2026-10-22T00:00:00Z".to_string(),
            }
        ));
    }
    assert!(cache.is_empty());
}

#[test]
fn trim_drops_the_oldest_first_and_keeps_the_newest() {
    let mut cache = VerdictCache::default();
    for day in 1..=5 {
        cache.insert(
            format!("k{day}"),
            CachedVerdict {
                verdict: "different".to_string(),
                concept: String::new(),
                judged_at: format!("2026-10-0{day}T00:00:00Z"),
            },
        );
    }
    cache.trim(3);
    assert_eq!(cache.len(), 3);
    assert!(cache.get("k1").is_none() && cache.get("k2").is_none());
    assert!(cache.get("k5").is_some());
    assert_eq!(MAX_CACHED_VERDICTS, 4_000);
}

#[test]
fn the_cache_round_trips_through_json_and_an_older_manifest_loads_empty() {
    let mut cache = VerdictCache::default();
    cache.insert(
        "abc".to_string(),
        CachedVerdict {
            verdict: "same_decision".to_string(),
            concept: "the discount".to_string(),
            judged_at: "2026-10-22T00:00:00Z".to_string(),
        },
    );
    let json = serde_json::to_string(&cache).unwrap();
    assert_eq!(serde_json::from_str::<VerdictCache>(&json).unwrap(), cache);

    // The cache rides in the scan manifest, additively.
    let manifest = crate::scan_cache::ManifestBuilder::new()
        .with_clone_verdicts(cache.clone())
        .finish();
    let back: crate::scan_cache::ScanManifest =
        serde_json::from_str(&serde_json::to_string(&manifest).unwrap()).unwrap();
    assert_eq!(back.clone_verdicts, cache);
    let old = r#"{"version":1,"rules_fingerprint":"x","files":{},"findings":[],"ai_covered":true}"#;
    let loaded: crate::scan_cache::ScanManifest = serde_json::from_str(old).unwrap();
    assert!(loaded.clone_verdicts.is_empty());
    assert!(
        loaded.is_current(),
        "adding the cache did not bump the manifest version"
    );
}

// ---- the key covers every copy's decision tokens (W29 Wave C review, D5) ----

/// The discount repo with the tested property renamed in BOTH copies: the copies still agree or
/// still disagree exactly as before, but the decision they write is a different one.
fn renamed_everywhere(rate_a: &str, rate_b: &str, from: &str, to: &str) -> Vec<(String, String)> {
    files(&[
        (
            "src/cart/total.ts",
            &discount("applyDiscount", rate_a).replace(from, to),
        ),
        (
            "src/checkout/summary.ts",
            &discount("quoteTotal", rate_b).replace(from, to),
        ),
    ])
}

#[test]
fn renaming_a_tested_property_in_every_copy_changes_the_key() {
    for (rate_a, rate_b) in [("0.2", "0.15"), ("0.2", "0.2")] {
        let before = renamed_everywhere(rate_a, rate_b, "user.isPremium", "user.isPremium");
        let after = renamed_everywhere(rate_a, rate_b, "user.isPremium", "user.isAdmin");
        assert_ne!(
            key_of(&before, RULE, JUDGE),
            key_of(&after, RULE, JUDGE),
            "agree={}: the decision changed, so the old verdict does not apply",
            rate_a == rate_b
        );
    }
}

#[tokio::test]
async fn the_cache_repro_a_rename_in_both_copies_forces_a_re_judge() {
    let before = renamed_everywhere("0.2", "0.15", "user.isPremium", "user.isPremium");
    let after = renamed_everywhere("0.2", "0.15", "user.isPremium", "user.isAdmin");
    let mut cache = VerdictCache::default();
    let first = StubJudge::answering("same_decision", "the premium discount");
    let a = judge_with(Some(&first), &before, &mut cache, JUDGE).await;
    assert_eq!((a.findings.len(), first.call_count()), (1, 1));

    let second = StubJudge::answering("different", "");
    let b = judge_with(Some(&second), &after, &mut cache, JUDGE).await;
    assert_eq!(
        second.call_count(),
        1,
        "the changed decision is asked again"
    );
    assert!(
        b.findings.is_empty(),
        "and the new answer is the one that counts"
    );
    assert_eq!(b.counts[&RULE].cached, 0);
}

#[test]
fn the_key_does_not_depend_on_which_copy_sorts_first() {
    // Swap which file holds which copy: the paths sort the other way round, so the "first" copy
    // is the other one. The multiset of decision tokens, and the key, are the same.
    let a = files(&[
        ("src/a.ts", &discount("applyDiscount", "0.2")),
        ("src/b.ts", &discount("quoteTotal", "0.15")),
    ]);
    let b = files(&[
        ("src/a.ts", &discount("applyDiscount", "0.15")),
        ("src/b.ts", &discount("quoteTotal", "0.2")),
    ]);
    assert_eq!(key_of(&a, RULE, JUDGE), key_of(&b, RULE, JUDGE));
}
