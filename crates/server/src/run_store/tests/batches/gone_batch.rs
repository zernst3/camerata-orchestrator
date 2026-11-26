//! #356: a batch whose results expired, or that the provider no longer has, before it was fetched
//! no longer hands its id back on every resume.
//!
//! The stage polls a batch it left running by id before it submits anything again
//! (`stage_batch::resume_batches`). These tests do the same through the journal: a poll of the
//! gone id, then the stage's resubmission of the set.

use super::*;

fn s(ids: &[&str]) -> Vec<String> {
    ids.iter().map(|i| i.to_string()).collect()
}

/// Mark `id` as one the provider no longer has.
fn drop_at_provider(fake: &Ledger, id: &str) {
    fake.gone.lock().unwrap().push(id.to_string());
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_batch_gone_before_its_first_fetch_is_sent_once_more_and_a_second_resume_sends_nothing() {
    seeded(async {
        let (_s, _g) = state_dir();
        let repo = git_repo();
        let store = new_run(&repo);
        let fake = Arc::new(Ledger::failing(&[]));
        let first = {
            let api = journal::JournalBatchApi::new(fake.clone(), store.clone());
            api.submit(three()).await.unwrap()
            // The process stops here: the batch was submitted and never fetched.
        };
        // Its results expire before the next attempt.
        drop_at_provider(&fake, &first);

        let store = reopen(store);
        let api = journal::JournalBatchApi::new(fake.clone(), store.clone());
        // The stage polls the batch it left running: the provider says it is gone.
        let err = api.poll(&first).await.unwrap_err();
        assert!(
            format!("{err:#}").contains("not_found_error"),
            "the provider's answer reaches the stage: {err:#}"
        );
        // The stage's recovery submits the unfinished items once more.
        let id = api.submit(three()).await.unwrap();
        assert_ne!(id, first, "the gone batch is not handed back again");
        assert_eq!(
            fake.sent(),
            vec![s(&["i1", "i2", "i3"]), s(&["i1", "i2", "i3"])],
            "the rows the gone batch owed go out once, in one new batch"
        );
        assert_eq!(api.poll(&id).await.unwrap().processing_status, "ended");
        let rows = api.fetch(&id, CacheTtl::FiveMinutes).await.unwrap();
        assert_eq!(ids_of(&rows), ["i1", "i2", "i3"]);
        assert!(rows.iter().all(|r| r.response.is_some()));
        assert!((journal::stored_spend(store.dir()) - 1.5).abs() < 1e-9);
        drop(api);

        // A second resume: every row is stored, nothing is sent or fetched again.
        let fetches = fake.fetches.load(Ordering::SeqCst);
        let store = reopen(store);
        let api = journal::JournalBatchApi::new(fake.clone(), store.clone());
        let id = api.submit(three()).await.unwrap();
        assert_ne!(
            id, first,
            "the gone batch is not handed back on a later resume"
        );
        assert_eq!(api.poll(&id).await.unwrap().processing_status, "ended");
        let rows = api.fetch(&id, CacheTtl::FiveMinutes).await.unwrap();
        assert_eq!(ids_of(&rows), ["i1", "i2", "i3"]);
        assert!(rows.iter().all(|r| r.response.is_some()));
        assert_eq!(fake.sent().len(), 2, "nothing is submitted a third time");
        assert_eq!(fake.fetches.load(Ordering::SeqCst), fetches);
        assert!((journal::stored_spend(store.dir()) - 1.5).abs() < 1e-9);
    })
    .await;
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_credit_stopped_row_whose_resend_batch_is_gone_is_sent_once_more_not_twice() {
    seeded(async {
        let (_s, _g) = state_dir();
        let (store, fake, _repo) = after_a_credit_stop().await;
        fake.restore_credit();
        let resend = {
            let api = journal::JournalBatchApi::new(fake.clone(), store.clone());
            api.submit(three()).await.unwrap()
            // The process stops here: the resend of i2 was submitted and never fetched.
        };
        assert_eq!(fake.sent()[1], s(&["i2"]));
        drop_at_provider(&fake, &resend);

        let store = reopen(store);
        let api = journal::JournalBatchApi::new(fake.clone(), store.clone());
        assert!(api.poll(&resend).await.is_err(), "the resend is gone");
        let id = api.submit(three()).await.unwrap();
        assert_ne!(id, resend, "the gone resend is not handed back again");
        assert_eq!(
            fake.sent(),
            vec![s(&["i1", "i2", "i3"]), s(&["i2"]), s(&["i2"])],
            "only the row the gone resend owed goes out, once; i1 and i3 are not paid again"
        );
        let rows = api.fetch(&id, CacheTtl::FiveMinutes).await.unwrap();
        assert_eq!(ids_of(&rows), ["i1", "i2", "i3"]);
        assert!(rows.iter().all(|r| r.response.is_some()));
        assert_eq!(
            rows[0].response.as_ref().unwrap().cost_usd,
            Some(0.0),
            "a stored row is replayed, not billed again"
        );
        assert!((journal::stored_spend(store.dir()) - 1.5).abs() < 1e-9);
        drop(api);

        let store = reopen(store);
        let api = journal::JournalBatchApi::new(fake.clone(), store.clone());
        let id = api.submit(three()).await.unwrap();
        let rows = api.fetch(&id, CacheTtl::FiveMinutes).await.unwrap();
        assert_eq!(ids_of(&rows), ["i1", "i2", "i3"]);
        assert!(rows.iter().all(|r| r.response.is_some()));
        assert_eq!(fake.sent().len(), 3, "i2 is not sent a third time");
        assert!((journal::stored_spend(store.dir()) - 1.5).abs() < 1e-9);
    })
    .await;
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_batch_that_ended_with_every_request_expired_and_no_results_counts_as_gone() {
    seeded(async {
        let (_s, _g) = state_dir();
        let repo = git_repo();
        let store = new_run(&repo);
        let fake = Arc::new(Ledger::failing(&[]));
        let first = {
            let api = journal::JournalBatchApi::new(fake.clone(), store.clone());
            api.submit(three()).await.unwrap()
        };
        fake.expired_empty.lock().unwrap().push(first.clone());

        let store = reopen(store);
        let api = journal::JournalBatchApi::new(fake.clone(), store.clone());
        let status = api.poll(&first).await.unwrap();
        assert_eq!(status.processing_status, "ended");
        let err = api.fetch(&first, CacheTtl::FiveMinutes).await.unwrap_err();
        assert!(
            crate::ai_audit::stage_batch::batch_is_gone(&err),
            "the stage reads it as gone, so its recovery runs: {err:#}"
        );
        let id = api.submit(three()).await.unwrap();
        assert_ne!(id, first);
        assert_eq!(fake.sent().len(), 2, "its rows go out once more");
        let rows = api.fetch(&id, CacheTtl::FiveMinutes).await.unwrap();
        assert!(rows.iter().all(|r| r.response.is_some()));
    })
    .await;
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_gone_batch_is_answered_from_the_journal_on_a_later_poll() {
    seeded(async {
        let (_s, _g) = state_dir();
        let repo = git_repo();
        let store = new_run(&repo);
        let fake = Arc::new(Ledger::failing(&[]));
        let first = {
            let api = journal::JournalBatchApi::new(fake.clone(), store.clone());
            api.submit(three()).await.unwrap()
        };
        drop_at_provider(&fake, &first);
        let store = reopen(store);
        {
            let api = journal::JournalBatchApi::new(fake.clone(), store.clone());
            assert!(api.poll(&first).await.is_err());
        }
        let polls = fake.polls.load(Ordering::SeqCst);
        let store = reopen(store);
        let api = journal::JournalBatchApi::new(fake.clone(), store);
        let err = api.poll(&first).await.unwrap_err();
        assert!(crate::ai_audit::stage_batch::batch_is_gone(&err), "{err:#}");
        let err = api.fetch(&first, CacheTtl::FiveMinutes).await.unwrap_err();
        assert!(crate::ai_audit::stage_batch::batch_is_gone(&err), "{err:#}");
        assert_eq!(
            fake.polls.load(Ordering::SeqCst),
            polls,
            "the provider is not asked again about a batch it said it no longer has"
        );
    })
    .await;
}
