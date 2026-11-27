//! The judge's repair turn, its failure record, and what a stored judge reply does on a resume
//! (#589). Scripted model only; the run store is a real one in a scratch state directory.

use std::sync::Arc;

use camerata_api_types::audit::FindingKind;

use super::*;
use crate::ai_audit::rejection_reason_tests::{call_files, new_run, seeded, state_dir};
use crate::run_store::journal::{EntryStatus, JournalLlm};
use crate::run_store::{FinishFailure, RunStore};

const RULE: CloneRule = CloneRule::SingleSourceLogic;

fn finish_failures(outcome: &JudgeOutcome) -> Vec<FinishFailure> {
    outcome
        .failed
        .iter()
        .map(crate::run_store::finish_failure_of)
        .collect()
}

/// Close the store the way a finished process would and open the run again, as a resume does.
fn reopen(store: Arc<RunStore>) -> Arc<RunStore> {
    let id = store.run_id();
    drop(store);
    RunStore::open(&id).unwrap()
}

#[tokio::test]
async fn a_malformed_reply_is_repaired_once_in_the_same_run() {
    let all = discount_repo("0.2", "0.15");
    let stub = StubJudge::new(|req, call| {
        if call == 0 {
            Ok("They look alike to me, but I cannot say more.".to_string())
        } else {
            Ok(verdicts(&asked_ids(req), "same_decision", "the discount"))
        }
    });
    let outcome = judge(Some(&stub), &all, set_for(&all, &[RULE])).await;
    assert_eq!(stub.call_count(), 2, "the first ask and one repair");
    assert_eq!(outcome.findings.len(), 2, "the repaired reply is judged");
    assert!(outcome.failed.is_empty(), "{:?}", outcome.failed);
    assert_eq!(outcome.model_calls, 2, "the repair is counted as a call");
    let first = stub.request(0);
    let repair = stub.request(1);
    assert!(repair.prompt.starts_with(&first.prompt));
    assert!(
        repair.prompt.contains("verdict object only"),
        "{}",
        repair.prompt
    );
    assert_eq!(repair.cache_breakpoints, first.cache_breakpoints);
}

#[tokio::test]
async fn a_good_reply_is_never_repaired() {
    let all = discount_repo("0.2", "0.15");
    let stub = StubJudge::answering("same_decision", "the discount");
    let _ = judge(Some(&stub), &all, set_for(&all, &[RULE])).await;
    assert_eq!(stub.call_count(), 1);
}

#[tokio::test]
async fn a_judge_that_always_answers_garbage_ends_as_a_disclosed_failure_after_two_calls_per_batch()
{
    let (all, set) = synthetic_set(40, 0); // batches of 16, 16, 8
    let stub = StubJudge::new(|_, _| Ok("I have no verdicts to give.".to_string()));
    let outcome = judge(Some(&stub), &all, set).await;
    assert_eq!(stub.call_count(), 6, "exactly 2 calls per batch");
    assert_eq!(
        outcome.failed.len(),
        1,
        "one disclosed failure for the rule"
    );
    assert_eq!(outcome.failed[0].pass, FAILED_PASS_NAME);
    assert_eq!(counts(&outcome).unjudged, 40);
    assert!(outcome.findings.is_empty());
}

fn counts(outcome: &JudgeOutcome) -> &JudgeCounts {
    &outcome.counts[&RULE]
}

#[tokio::test]
async fn the_failure_record_has_real_attempts_the_batch_and_a_capped_redacted_sample() {
    let (all, set) = synthetic_set(20, 0); // batches of 16 and 4; both fail
    let long = format!(
        "Sorry. My key is sk-ant-api03-AAAAAAAAAAAAAAAAAAAAAAAA1234. {}",
        "padding ".repeat(500)
    );
    let stub = StubJudge::new(move |_, _| Ok(long.clone()));
    let outcome = judge(Some(&stub), &all, set).await;
    let attempts = &outcome.failed[0].attempts;
    assert_eq!(attempts.len(), 4, "two asks for each of the two batches");
    assert!(attempts[0].message.contains("candidate batch 1 of 2"));
    assert!(attempts[0].message.contains("attempt 1"));
    assert!(attempts[1].message.contains("attempt 2"));
    assert!(attempts[2].message.contains("candidate batch 2 of 2"));
    assert!(attempts[0].message.contains("Sorry. My key is"));
    for a in attempts {
        assert!(!a.message.contains("AAAAAAAA"), "{}", a.message);
        assert!(a.message.chars().count() <= 400, "{}", a.message.len());
        assert_eq!(a.model, JUDGE);
    }
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_failed_parse_is_stored_as_failed_so_it_is_never_replayed_as_a_success() {
    seeded(async {
        let (_s, _g) = state_dir();
        let repo = tempfile::tempdir().unwrap();
        let store = new_run(repo.path());
        let all = discount_repo("0.2", "0.15");
        // A JSON object that parses but is not a verdict object: stored as a good reply by the
        // journal, so only the judge's own rejection can set it aside.
        let stub = StubJudge::new(|_, _| Ok(r#"{"verdicts":"same_decision"}"#.to_string()));
        let journal = JournalLlm::new(&stub, store.clone());
        let outcome = judge(Some(&journal), &all, set_for(&all, &[RULE])).await;
        assert_eq!(outcome.failed.len(), 1);
        let files = call_files(&store);
        assert_eq!(files.len(), 2, "the first ask and the repair");
        for file in &files {
            let entry = &file.entries[&0];
            assert_eq!(entry.status, EntryStatus::Rejected, "{entry:?}");
            assert!(entry
                .reason
                .as_deref()
                .unwrap()
                .contains("no verdict object"));
        }
    })
    .await;
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_repaired_batch_stores_the_bad_reply_as_failed_and_the_repair_as_good() {
    seeded(async {
        let (_s, _g) = state_dir();
        let repo = tempfile::tempdir().unwrap();
        let store = new_run(repo.path());
        let all = discount_repo("0.2", "0.15");
        let stub = StubJudge::new(|req, call| {
            if call == 0 {
                Ok(r#"{"verdicts":"maybe"}"#.to_string())
            } else {
                Ok(verdicts(&asked_ids(req), "same_decision", "the discount"))
            }
        });
        let journal = JournalLlm::new(&stub, store.clone());
        let outcome = judge(Some(&journal), &all, set_for(&all, &[RULE])).await;
        assert!(outcome.failed.is_empty());
        let files = call_files(&store);
        let rejected = files
            .iter()
            .filter(|f| f.entries[&0].status == EntryStatus::Rejected)
            .count();
        let done = files
            .iter()
            .filter(|f| f.entries[&0].status == EntryStatus::Done)
            .count();
        assert_eq!((rejected, done), (1, 1));
    })
    .await;
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_resume_after_one_malformed_batch_replays_every_good_batch_and_re_asks_only_the_bad_one()
{
    seeded(async {
        let (_s, _g) = state_dir();
        let repo = tempfile::tempdir().unwrap();
        let store = new_run(repo.path());
        let (all, set) = synthetic_set(40, 0); // batches of 16, 16, 8
        let (all2, set2) = synthetic_set(40, 0);
        // The middle batch is garbage on both asks; the others are good.
        let first_run = StubJudge::new(|req, _| {
            let ids = asked_ids(req);
            if ids.len() == 16 && ids[0].contains(":0000000000000010:") {
                Ok("no verdicts here".to_string())
            } else {
                Ok(verdicts(&ids, "same_decision", "the discount"))
            }
        });
        let journal = JournalLlm::new(&first_run, store.clone());
        let outcome = judge(Some(&journal), &all, set).await;
        assert_eq!(outcome.failed.len(), 1, "one batch failed");
        assert_eq!(first_run.call_count(), 4, "good, bad twice, good");
        crate::run_store::journal::quarantine_rejected(&store, &finish_failures(&outcome));
        let files = call_files(&store);
        let done = files
            .iter()
            .filter(|f| f.entries[&0].status == EntryStatus::Done)
            .count();
        assert_eq!(done, 2, "both good batches stay good");

        drop(journal);
        let store = reopen(store);
        let resumed = StubJudge::answering("same_decision", "the discount");
        let journal = JournalLlm::new(&resumed, store.clone());
        let outcome = judge(Some(&journal), &all2, set2).await;
        assert_eq!(
            resumed.call_count(),
            1,
            "only the bad batch is asked again; the good ones cost nothing"
        );
        assert!(outcome.failed.is_empty(), "{:?}", outcome.failed);
        assert_eq!(counts(&outcome).confirmed, 40);
        assert!(outcome
            .findings
            .iter()
            .all(|f| !matches!(f.kind, Some(FindingKind::NotAssessed(_)))));
    })
    .await;
}

/// A reply the new parser reads (braces in prose around the verdict object) is a good reply to
/// the store too: stored good, and replayed on a resume at no cost.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_reply_with_braces_in_prose_is_stored_good_and_replays_on_resume() {
    seeded(async {
        let (_s, _g) = state_dir();
        let repo = tempfile::tempdir().unwrap();
        let store = new_run(repo.path());
        let all = discount_repo("0.2", "0.15");
        let first = StubJudge::new(|req, _| {
            Ok(format!(
                "I compared {{both copies}}.\n{}\nNote the {{rate}} differs.",
                verdicts(&asked_ids(req), "same_decision", "the discount")
            ))
        });
        let journal = JournalLlm::new(&first, store.clone());
        let outcome = judge(Some(&journal), &all, set_for(&all, &[RULE])).await;
        assert_eq!(outcome.findings.len(), 2);
        let files = call_files(&store);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].entries[&0].status, EntryStatus::Done);

        drop(journal);
        let store = reopen(store);
        let resumed = StubJudge::answering("same_decision", "the discount");
        let journal = JournalLlm::new(&resumed, store.clone());
        let outcome = judge(Some(&journal), &all, set_for(&all, &[RULE])).await;
        assert_eq!(resumed.call_count(), 0, "the stored reply is replayed");
        assert_eq!(outcome.findings.len(), 2);
    })
    .await;
}

#[test]
fn two_different_verdict_objects_in_one_reply_are_not_a_reading() {
    use super::prompt::find_verdict_object;
    let a = r#"{"verdicts":[{"id":"a","verdict":"same_decision","concept":"x"}]}"#;
    let b = r#"{"verdicts":[{"id":"a","verdict":"different","concept":""}]}"#;
    // A draft, a quoted or an example object can beat the real one: never guess which.
    assert!(find_verdict_object(&format!("Draft: {a}\nFinal: {b}")).is_none());
    // The same object twice (a fenced copy beside the bare one) is one reading.
    assert!(find_verdict_object(&format!("{a}\n```json\n{a}\n```")).is_some());
}

#[tokio::test]
async fn a_reply_with_two_different_verdict_objects_goes_to_the_repair_turn() {
    let all = discount_repo("0.2", "0.15");
    let stub = StubJudge::new(|req, call| {
        let ids = asked_ids(req);
        if call == 0 {
            Ok(format!(
                "Quoted from the code: {}\nMy answer: {}",
                verdicts(&ids, "different", ""),
                verdicts(&ids, "same_decision", "the discount")
            ))
        } else {
            Ok(verdicts(&ids, "same_decision", "the discount"))
        }
    });
    let outcome = judge(Some(&stub), &all, set_for(&all, &[RULE])).await;
    assert_eq!(stub.call_count(), 2);
    assert!(outcome.failed.is_empty());
    assert_eq!(outcome.findings.len(), 2);
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_reply_with_no_verdict_for_any_asked_id_is_rejected_and_repaired() {
    seeded(async {
        let (_s, _g) = state_dir();
        let repo = tempfile::tempdir().unwrap();
        let store = new_run(repo.path());
        let all = discount_repo("0.2", "0.15");
        // Parses as a verdict object, but names no candidate that was asked about.
        let stub = StubJudge::new(|req, call| {
            if call == 0 {
                Ok(verdicts(&["not-asked".to_string()], "same_decision", "x"))
            } else {
                Ok(verdicts(&asked_ids(req), "same_decision", "the discount"))
            }
        });
        let journal = JournalLlm::new(&stub, store.clone());
        let outcome = judge(Some(&journal), &all, set_for(&all, &[RULE])).await;
        assert_eq!(stub.call_count(), 2, "an empty reading is repaired");
        assert_eq!(outcome.findings.len(), 2);
        let files = call_files(&store);
        let rejected = files
            .iter()
            .filter(|f| f.entries[&0].status == EntryStatus::Rejected)
            .count();
        assert_eq!(rejected, 1, "never stored good with the batch unjudged");
    })
    .await;
}

#[tokio::test]
async fn a_judge_that_only_names_unknown_ids_ends_as_a_disclosed_failure() {
    let all = discount_repo("0.2", "0.15");
    let stub =
        StubJudge::new(|_, _| Ok(verdicts(&["not-asked".to_string()], "same_decision", "x")));
    let outcome = judge(Some(&stub), &all, set_for(&all, &[RULE])).await;
    assert_eq!(stub.call_count(), 2);
    assert_eq!(outcome.failed.len(), 1);
}

/// A failure of another stage that shares the Other kind (the deep tier) sets aside the calls of
/// that stage, never the good batches of the clone judge, which record their own rejections.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_deep_tier_failure_leaves_the_good_judge_batches_replayable() {
    seeded(async {
        let (_s, _g) = state_dir();
        let repo = tempfile::tempdir().unwrap();
        let store = new_run(repo.path());
        let (all, set) = synthetic_set(40, 0);
        let stub = StubJudge::answering("same_decision", "the discount");
        let journal = JournalLlm::new(&stub, store.clone());
        // One more Other-kind call that is not the judge's (a deep lens).
        journal
            .complete(LlmRequest::new("Repository: acme/shop\nlens question").with_model("m"))
            .await
            .unwrap();
        let outcome = judge(Some(&journal), &all, set).await;
        assert!(outcome.failed.is_empty());
        crate::run_store::journal::quarantine_rejected(
            &store,
            &[FinishFailure {
                repo: "acme/shop".to_string(),
                pass: "deep tier, lens one".to_string(),
                class: "content".to_string(),
                files: Vec::new(),
                scope_note: false,
                per_reply_rejections: false,
            }],
        );
        let files = call_files(&store);
        let status_of = |judge_owned: bool| {
            files
                .iter()
                .filter(|f| f.files.is_empty() != judge_owned)
                .map(|f| f.entries[&0].status)
                .collect::<Vec<_>>()
        };
        assert!(
            status_of(true).iter().all(|s| *s == EntryStatus::Done),
            "good judge batches stay good: {:?}",
            status_of(true)
        );
        assert_eq!(
            status_of(false),
            vec![EntryStatus::Rejected],
            "the deep call is set aside"
        );
    })
    .await;
}

/// A batch that was repaired in the run that stopped is resumed from its repair reply: the first
/// ask was turned down and would only be turned down again, so it is not billed a second time.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_resume_of_a_repaired_batch_goes_straight_to_the_repair_reply_at_zero_calls() {
    seeded(async {
        let (_s, _g) = state_dir();
        let repo = tempfile::tempdir().unwrap();
        let store = new_run(repo.path());
        let all = discount_repo("0.2", "0.15");
        let first_run = StubJudge::new(|req, call| {
            if call == 0 {
                Ok(r#"{"verdicts":"maybe"}"#.to_string())
            } else {
                Ok(verdicts(&asked_ids(req), "same_decision", "the discount"))
            }
        });
        let journal = JournalLlm::new(&first_run, store.clone());
        let outcome = judge(Some(&journal), &all, set_for(&all, &[RULE])).await;
        assert!(outcome.failed.is_empty());
        assert_eq!(first_run.call_count(), 2);

        drop(journal);
        let store = reopen(store);
        let resumed = StubJudge::answering("same_decision", "the discount");
        let journal = JournalLlm::new(&resumed, store.clone());
        let outcome = judge(Some(&journal), &all, set_for(&all, &[RULE])).await;
        assert_eq!(resumed.call_count(), 0, "neither ask is sent again");
        assert_eq!(outcome.findings.len(), 2);
    })
    .await;
}

/// The transport failure message goes through the same redaction as the reply sample before it is
/// kept in the failure record.
#[tokio::test]
async fn a_transport_attempt_message_is_redacted_like_the_reply_sample() {
    let all = discount_repo("0.2", "0.15");
    let stub = StubJudge::new(|_, _| {
        Err(anyhow::anyhow!(
            "connect failed for postgres://admin:hunter2hunter2@db.internal/app and key AKIAIOSFODNN7EXAMPLE9"
        ))
    });
    let outcome = judge(Some(&stub), &all, set_for(&all, &[RULE])).await;
    let message = &outcome.failed[0].attempts[0].message;
    assert!(message.contains("connect failed"), "{message}");
    assert!(!message.contains("hunter2hunter2"), "{message}");
    assert!(!message.contains("AKIAIOSFODNN7EXAMPLE9"), "{message}");
}

/// The prompt asks for ONE object, but a model that answers one object per candidate gives
/// disjoint verdicts: they are merged. Only a conflict (the same id, different answers) is no
/// reading.
#[test]
fn objects_with_disjoint_ids_are_merged_and_only_a_conflict_is_refused() {
    use super::prompt::{find_verdict_object, parse_verdicts};
    let one = r#"{"verdicts":[{"id":"a","verdict":"same_decision","concept":"the discount"}]}"#;
    let two = r#"{"verdicts":[{"id":"b","verdict":"different","concept":""}]}"#;
    let merged = find_verdict_object(&format!("{one}\n{two}")).expect("disjoint ids merge");
    assert_eq!(merged["verdicts"].as_array().unwrap().len(), 2);
    let asked: std::collections::HashSet<&str> = ["a", "b"].into_iter().collect();
    let read = parse_verdicts(&format!("{one}\n{two}"), RULE, &asked);
    assert!(read.parsed);
    assert_eq!(read.verdicts.len(), 2);
    let clash = r#"{"verdicts":[{"id":"a","verdict":"different","concept":""}]}"#;
    assert!(find_verdict_object(&format!("{one}\n{clash}")).is_none());
}
