//! The recheck of a saved run rebuilds each judge request from the repo; its digest must be the
//! one the scan sent, for several candidates and several batches, with the rule corpus present.

use super::*;
use crate::onboard::clone_advisory::redo::{plan_set, RedoInput};
use crate::run_store::journal::request_key;

const RULE: CloneRule = CloneRule::SingleSourceLogic;

#[tokio::test]
async fn rebuilt_requests_have_the_digests_the_scan_sent_for_many_candidates_batches_and_the_corpus(
) {
    let corpus = camerata_rules::load_corpus(&camerata_rules::corpus_path())
        .await
        .expect("the bundled corpus loads");
    let (all, set) = synthetic_set(40, 0); // batches of 16, 16, 8
    let seed = Some([7u8; crate::prompt_frame::NONCE_BYTES]);
    let stub = StubJudge::answering("same_decision", "the discount");
    let mut cache = VerdictCache::default();
    crate::prompt_frame::with_seed(
        seed,
        judge_clone_candidates(
            Some(&stub),
            JudgeRequest {
                repo: "acme/shop",
                files: &all,
                set: set.clone(),
                judge_model: JUDGE,
                corpus: Some(&corpus),
                unavailable_reason: "",
            },
            &mut cache,
            None,
            None,
        ),
    )
    .await;
    let sent: Vec<String> = (0..stub.call_count())
        .map(|i| request_key(&stub.request(i)))
        .collect();
    assert_eq!(sent.len(), 3, "three batches were asked");
    let dir = tempfile::tempdir().unwrap();
    let armed = armed(&[RULE]);
    let plan = crate::prompt_frame::with_seed(seed, async {
        plan_set(
            &RedoInput {
                repo: "acme/shop",
                files: &all,
                armed: &armed,
                rule: RULE,
                judge_model: JUDGE,
                corpus: Some(&corpus),
                run_dir: dir.path(),
            },
            set,
        )
    })
    .await;
    let rebuilt: Vec<String> = plan.batches.iter().map(|b| b.key.clone()).collect();
    assert_eq!(
        rebuilt, sent,
        "the rebuilt requests are the ones the scan sent"
    );
}
