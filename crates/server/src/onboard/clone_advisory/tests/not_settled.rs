//! What the operator sees of a clone judge call. A call the model answered is done, an honest
//! "unsure" included, and its line says what was not settled; only a call that really failed
//! looks failed, with the reason. The ledger does not change: an unsettled rule is not checked.

use super::*;
use crate::transcript::TranscriptStore;
use camerata_api_types::audit::{FindingKind, NotAssessedReason};

const RULE: CloneRule = CloneRule::SingleSourceLogic;

const SESSION: &str = "clone-judge:ARCH-SINGLE-SOURCE-LOGIC-1:acme/shop";

/// Three candidates; the model answers `different` for all but the first, which it is unsure of.
async fn one_of_three_unsure(store: &TranscriptStore) -> JudgeOutcome {
    let (all, set) = synthetic_set(3, 0);
    let first = set.by_rule[&RULE][0].id.clone();
    let stub = StubJudge::new(move |req, _| {
        let ids = asked_ids(req);
        let items: Vec<serde_json::Value> = ids
            .iter()
            .map(|id| {
                let word = if *id == first { "unsure" } else { "different" };
                serde_json::json!({ "id": id, "verdict": word, "concept": "" })
            })
            .collect();
        Ok(serde_json::json!({ "verdicts": items }).to_string())
    });
    let mut cache = VerdictCache::default();
    judge_clone_candidates(
        Some(&stub),
        request(&all, set),
        &mut cache,
        None,
        Some((store, "k")),
    )
    .await
}

#[tokio::test]
async fn a_call_that_answered_with_one_unsure_is_done_and_says_what_was_not_settled() {
    let store = TranscriptStore::new();
    let outcome = one_of_three_unsure(&store).await;

    let agent = store
        .get("k")
        .into_iter()
        .find(|a| a.session_id == SESSION)
        .expect("the clone judge row");
    assert_eq!(agent.status, "done", "{agent:#?}");
    assert!(
        agent
            .output
            .contains("Done: 2 judged, 1 unsure (held for a reviewer's decision)"),
        "{}",
        agent.output
    );

    // The ledger side is exactly as before: the rule is not fully assessed, not clean.
    let counts = &outcome.counts[&RULE];
    assert_eq!((counts.judged, counts.unjudged, counts.unsure), (2, 1, 1));
    assert!(!counts.fully_assessed());
    // The unsure candidate is a held Queue row now, not a second surface for the rule.
    assert!(outcome.not_assessed.is_empty());
    assert!(outcome.findings.iter().any(|f| f.needs_review));
    let mut ledger = crate::scan_ledger::ScanLedger::default();
    record_in_ledger(&mut ledger, "acme/shop", &outcome);
}

#[tokio::test]
async fn the_unsure_row_for_an_unsure_answer_reads_in_the_operators_words() {
    let store = TranscriptStore::new();
    let outcome = one_of_three_unsure(&store).await;
    let row = outcome
        .findings
        .iter()
        .find(|f| f.needs_review)
        .expect("an unsure row");
    assert_eq!(row.kind, Some(FindingKind::Defect));
    assert!(
        row.detail.contains("could not tell whether"),
        "{}",
        row.detail
    );
    assert!(
        !row.detail.contains("could not be decided") && !row.detail.contains("declined"),
        "{}",
        row.detail
    );
}

#[tokio::test]
async fn the_not_assessed_row_for_an_all_unsure_rule_reads_in_the_operators_words() {
    let (all, set) = synthetic_set(3, 0);
    let stub = StubJudge::answering("unsure", "");
    let mut cache = VerdictCache::default();
    let outcome =
        judge_clone_candidates(Some(&stub), request(&all, set), &mut cache, None, None).await;
    let row = &outcome.not_assessed[0];
    let Some(FindingKind::NotAssessed(kind)) = row.kind.clone() else {
        panic!("a not_assessed row");
    };
    // The ledger class is as before; only the words change.
    assert_eq!(kind.reason, NotAssessedReason::ModelDeclined);
    assert_eq!(
        kind.reason_detail,
        "the model was unsure about 3 of 3 candidates; 0 judged"
    );
    assert!(
        !row.detail.contains("could not be decided") && !row.detail.contains("declined"),
        "{}",
        row.detail
    );
}

#[tokio::test]
async fn a_call_that_errored_looks_failed_with_the_reason() {
    let all = discount_repo("0.2", "0.15");
    let stub = StubJudge::new(|_, _| anyhow::bail!("connection reset by peer"));
    let store = TranscriptStore::new();
    let mut cache = VerdictCache::default();
    let _ = judge_clone_candidates(
        Some(&stub),
        request(&all, set_for(&all, &[RULE])),
        &mut cache,
        None,
        Some((&store, "k")),
    )
    .await;
    let agent = &store.get("k")[0];
    assert_eq!(agent.status, "blocked", "{agent:#?}");
    assert!(
        agent.output.contains("Did not finish:") && agent.output.contains("connection reset"),
        "{}",
        agent.output
    );
}

#[tokio::test]
async fn a_call_whose_answer_could_not_be_used_looks_failed_with_the_reason() {
    let all = discount_repo("0.2", "0.15");
    let stub = StubJudge::new(|_, _| Ok("I think they are the same.".to_string()));
    let store = TranscriptStore::new();
    let mut cache = VerdictCache::default();
    let _ = judge_clone_candidates(
        Some(&stub),
        request(&all, set_for(&all, &[RULE])),
        &mut cache,
        None,
        Some((&store, "k")),
    )
    .await;
    let agent = &store.get("k")[0];
    assert_eq!(agent.status, "blocked", "{agent:#?}");
    assert!(
        agent.output.contains("not a verdict object"),
        "{}",
        agent.output
    );
}
