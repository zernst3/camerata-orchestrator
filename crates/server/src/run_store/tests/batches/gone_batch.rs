//! #356: a batch whose results expired, or that the provider no longer has, before it was fetched
//! no longer hands its id back on every resume; and a resend chain has one owner per link.
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

/// Two batches whose credit-stopped rows are the same request (`i2`): one from the three items,
/// one from `i4` and `i2`.
fn other_set() -> Vec<BatchItem> {
    vec![named("i4"), named("i2")]
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_resend_is_owned_by_one_chain_and_after_a_restart_each_id_reads_its_own_rows() {
    seeded(async {
        let (_s, _g) = state_dir();
        let repo = git_repo();
        let store = new_run(&repo);
        let fake = Arc::new(Ledger::failing(&["i2"]));
        {
            let api = journal::JournalBatchApi::new(fake.clone(), store.clone());
            let a = api.submit(three()).await.unwrap();
            api.fetch(&a, CacheTtl::FiveMinutes).await.unwrap();
            let b = api.submit(other_set()).await.unwrap();
            api.fetch(&b, CacheTtl::FiveMinutes).await.unwrap();
        }
        let store = reopen(store);
        fake.restore_credit();
        let (resend_a, resend_b) = {
            let api = journal::JournalBatchApi::new(fake.clone(), store.clone());
            let ra = api.submit(three()).await.unwrap();
            let rb = api.submit(other_set()).await.unwrap();
            (ra, rb)
            // The process stops before either resend is fetched.
        };
        assert_ne!(
            resend_a, resend_b,
            "a resend already linked from one batch is not adopted by a second"
        );
        assert_eq!(fake.sent()[2..], [s(&["i2"]), s(&["i2"])]);

        // A restart: the stage polls and fetches each batch by the id it saved.
        let store = reopen(store);
        let api = journal::JournalBatchApi::new(fake.clone(), store.clone());
        let rows = api.fetch(&resend_a, CacheTtl::FiveMinutes).await.unwrap();
        assert_eq!(ids_of(&rows), ["i1", "i2", "i3"]);
        assert!(rows.iter().all(|r| r.response.is_some()));
        let rows = api.fetch(&resend_b, CacheTtl::FiveMinutes).await.unwrap();
        assert_eq!(ids_of(&rows), ["i4", "i2"]);
        assert!(rows.iter().all(|r| r.response.is_some()));
        assert_eq!(fake.sent().len(), 4, "nothing more is submitted");
    })
    .await;
}

/// A run saved before the rule above can hold a batch linked from two others: the first batch
/// of the three items, and the batch of `i4` and `i2`. After a restart the resend is read as
/// part of the chain whose first batch was submitted first (then the lower file name). Returns
/// the items a fetch of the resend by id answers, after each of three restarts.
async fn read_a_resend_linked_from_two(three_first: bool) -> Vec<Vec<String>> {
    let (_s, _g) = state_dir();
    let repo = git_repo();
    let store = new_run(&repo);
    let fake = Arc::new(Ledger::failing(&["i2"]));
    {
        let api = journal::JournalBatchApi::new(fake.clone(), store.clone());
        let a = api.submit(three()).await.unwrap();
        api.fetch(&a, CacheTtl::FiveMinutes).await.unwrap();
        let b = api.submit(other_set()).await.unwrap();
        api.fetch(&b, CacheTtl::FiveMinutes).await.unwrap();
    }
    let store = reopen(store);
    fake.restore_credit();
    let resend = {
        let api = journal::JournalBatchApi::new(fake.clone(), store.clone());
        api.submit(three()).await.unwrap()
    };
    // Link the second batch to the same resend, the way an older build could, and date the two.
    let batches = store.dir().join("batches");
    let mut files: Vec<(std::path::PathBuf, serde_json::Value)> = std::fs::read_dir(&batches)
        .unwrap()
        .map(|e| {
            let path = e.unwrap().path();
            let v = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            (path, v)
        })
        .collect();
    let link = files
        .iter()
        .find_map(|(_, v)| (!v["follow"].is_null()).then(|| v["follow"].clone()))
        .expect("the first batch links its resend");
    let (early, late) = ("2001-01-01T00:00:00Z", "2002-01-01T00:00:00Z");
    for (path, v) in &mut files {
        let of_three = v["custom_ids"] == serde_json::json!(["i1", "i2", "i3"]);
        let of_other = v["custom_ids"] == serde_json::json!(["i4", "i2"]);
        if of_other {
            v["follow"] = link.clone();
        }
        if of_three || of_other {
            let first = of_three == three_first;
            v["submitted_at"] = serde_json::json!(if first { early } else { late });
            std::fs::write(&*path, serde_json::to_vec(&*v).unwrap()).unwrap();
        }
    }
    let mut read = Vec::new();
    let mut store = store;
    for _ in 0..3 {
        store = reopen(store);
        let api = journal::JournalBatchApi::new(fake.clone(), store.clone());
        let rows = api.fetch(&resend, CacheTtl::FiveMinutes).await.unwrap();
        read.push(ids_of(&rows));
    }
    read
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_resend_linked_from_two_batches_on_disk_is_read_with_the_one_submitted_first() {
    seeded(async {
        let three_items = s(&["i1", "i2", "i3"]);
        let other_items = s(&["i4", "i2"]);
        assert_eq!(
            read_a_resend_linked_from_two(true).await,
            vec![three_items.clone(); 3],
            "the three items were submitted first: their chain owns the resend"
        );
        assert_eq!(
            read_a_resend_linked_from_two(false).await,
            vec![other_items.clone(); 3],
            "i4 and i2 were submitted first: their chain owns the resend"
        );
    })
    .await;
}
