//! Every clone-judge request passes the size check (design A7) before it is sent: one that does
//! not fit the model's window is refused, disclosed as a failed part, and never sent or cut.

use camerata_checks::clone_rules::RuleFacts;

use super::*;

const RULE: CloneRule = CloneRule::SingleSourceLogic;

/// The one candidate of `synthetic_set(n, 0)` at `index`, made far over any model window by a
/// decision token of `bytes` bytes (the judge input caps the number of tokens, not their size).
fn make_huge(set: &mut CandidateSet, index: usize, bytes: usize) {
    let candidate = &mut set.by_rule.get_mut(&RULE).unwrap()[index];
    candidate.facts.rule = RuleFacts::Logic {
        decision_tokens: vec!["x".repeat(bytes)],
        member_decision_tokens: Vec::new(),
    };
}

#[tokio::test]
async fn an_oversized_request_is_refused_before_any_send_and_disclosed_as_a_failed_part() {
    let (all, mut set) = synthetic_set(1, 0);
    make_huge(&mut set, 0, 2_000_000);
    let stub = StubJudge::answering("same_decision", "the premium discount calculation");

    let outcome = judge(Some(&stub), &all, set).await;

    assert_eq!(stub.call_count(), 0, "nothing was sent");
    assert_eq!(outcome.model_calls, 0, "no model call is counted");
    assert!(outcome.findings.is_empty());
    assert_eq!(outcome.failed.len(), 1, "{:#?}", outcome.failed);
    let failed = &outcome.failed[0];
    assert_eq!(failed.pass, FAILED_PASS_NAME);
    assert!(failed.reason.starts_with(RULE.id()), "{}", failed.reason);
    assert!(
        failed.reason.contains("does not fit") && failed.reason.contains("not sent"),
        "{}",
        failed.reason
    );
    // The candidate is undecided, so the rule is reported as not fully assessed, never clean.
    let counts = &outcome.counts[&RULE];
    assert_eq!((counts.found, counts.judged, counts.unjudged), (1, 0, 1));
    assert_eq!(outcome.not_assessed.len(), 1);
}

#[tokio::test]
async fn only_the_oversized_batch_is_refused_and_the_other_batches_are_still_judged() {
    // Seventeen candidates are two batches (16 and 1). The second is the oversized one.
    let (all, mut set) = synthetic_set(BATCH_SIZE + 1, 0);
    make_huge(&mut set, BATCH_SIZE, 2_000_000);
    let stub = StubJudge::answering("different", "");

    let outcome = judge(Some(&stub), &all, set).await;

    assert_eq!(stub.call_count(), 1, "the first batch went out, the second did not");
    assert_eq!(outcome.model_calls, 1);
    assert_eq!(outcome.failed.len(), 1);
    let counts = &outcome.counts[&RULE];
    assert_eq!((counts.found, counts.judged, counts.unjudged), (17, 16, 1));
}
