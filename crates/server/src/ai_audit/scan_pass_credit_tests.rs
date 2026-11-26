//! The scan pass's own batch path ([`run_passes_batch`]) and a credit or billing error on a batch
//! ROW. The stage runner (`stage_batch`) stops the run at once on such a row and never resends
//! it; the scan pass must do the same, through the same classifier and the same run health stop,
//! while a transient error row keeps its one resubmission.
//!
//! Every test drives the real pass through the fake Message Batches APIs of
//! `batch_default_tests` (scan pass) and `group_review_batch_tests` (stage runner): no network
//! and no spend. The clock is paused where a poll wait would otherwise be real.

use std::collections::HashSet;
use std::sync::Arc;

use super::batch_default_tests::{eff, meter, FakeBatch, Reply, CREDIT_ERROR};
use super::group_review_batch_tests as stage;
use super::*;
use camerata_llm::failure::FailureClass;

const REPO: &str = "acme/widgets";
const REPO_MAP: &str = "REPO MAP:\nfoo.rs: fn foo()\n";

/// One chunk, three rule-batches of one rule each: three items `c0-b0 c0-b1 c0-b2`.
async fn run_three(api: &dyn BatchApi, meter: &UsageMeter) -> (usize, Vec<PassFailure>) {
    let c0 = vec![("foo.rs".to_string(), "fn foo() {}\n".to_string())];
    let chunks: Vec<&[(String, String)]> = vec![&c0];
    let b0 = vec![("RULE-A-1".to_string(), "do a".to_string())];
    let b1 = vec![("RULE-B-1".to_string(), "do b".to_string())];
    let b2 = vec![("RULE-C-1".to_string(), "do c".to_string())];
    let batches: Vec<&[(String, String)]> = vec![&b0, &b1, &b2];
    let adopted: HashSet<String> = HashSet::new();
    let (_findings, _proposed, _requested, ok, failures) = run_passes_batch(
        api,
        REPO,
        &RepoMap::whole_text(REPO_MAP),
        &crate::prompt_frame::FrameNonce::fixed("0123456789abcdef0123456789abcdef"),
        &adopted,
        None,
        None,
        &chunks,
        None,
        &batches,
        "pass",
        Some(meter),
        false,
    )
    .await
    .expect("the batch path runs");
    (ok, failures)
}

/// The scan pass run with row `c0-b1` failing for credit, on a fresh meter.
async fn scan_pass_with_a_credit_row() -> (FakeBatch, UsageMeter, usize, Vec<PassFailure>) {
    let api = FakeBatch::new(|_round, id| {
        if id == "c0-b1" {
            Reply::CreditError
        } else {
            Reply::Ok
        }
    });
    let m = meter(eff(true), None);
    let (ok, failures) = run_three(&api, &m).await;
    (api, m, ok, failures)
}

// ── 1. A credit row stops the run and is not sent again ────────────────────────────────────

#[tokio::test(start_paused = true)]
async fn a_credit_error_row_in_the_scan_pass_stops_the_run_and_is_not_resent() {
    let (api, m, ok, failures) = scan_pass_with_a_credit_row().await;

    assert_eq!(
        api.submitted_ids(),
        vec![vec!["c0-b0", "c0-b1", "c0-b2"]],
        "the credit row was not resubmitted in a follow-up batch"
    );
    assert_eq!(
        api.realtime_calls(),
        0,
        "the credit row got no real-time second try"
    );
    assert!(
        m.outage_stopped(),
        "the row's credit error reached run health"
    );
    assert_eq!(
        m.health().trigger().map(|t| t.class),
        Some(FailureClass::AuthOrBilling),
        "the run stopped for billing"
    );

    // The two good rows are kept and counted once; the credit row was not billed.
    assert_eq!(ok, 2, "{failures:?}");
    let snap = m.snapshot();
    assert_eq!(snap.calls, 2, "only the two answered rows are on the meter");
    assert!(
        (snap.cost_usd - 0.02).abs() < 1e-9,
        "two rows at the scripted cost, nothing for the credit row: {}",
        snap.cost_usd
    );

    // The failed part is labeled billing, never transport, and says it was not resent.
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert_eq!(failures[0].class, FailureClass::AuthOrBilling);
    assert!(
        failures[0].error.contains("credit balance"),
        "{}",
        failures[0].error
    );
    assert!(
        !failures[0].error.contains("resubmitted"),
        "{}",
        failures[0].error
    );
}

// ── 2. A transient row keeps its one resubmission (guard) ──────────────────────────────────

#[tokio::test(start_paused = true)]
async fn a_transient_error_row_in_the_scan_pass_is_still_resubmitted_once() {
    let api = FakeBatch::new(|round, id| match (round, id) {
        (0, "c0-b1") => Reply::Errored,
        _ => Reply::Ok,
    });
    let m = meter(eff(true), None);
    let (ok, failures) = run_three(&api, &m).await;

    assert_eq!(
        api.submitted_ids(),
        vec![vec!["c0-b0", "c0-b1", "c0-b2"], vec!["c0-b1"]],
        "the overloaded row went out once more, alone"
    );
    assert!(!m.outage_stopped(), "a transient row does not stop the run");
    assert_eq!(ok, 3);
    assert!(failures.is_empty(), "{failures:?}");
    assert_eq!(m.snapshot().calls, 3, "each answered row is counted once");
}

// ── 3. Both batch paths map a credit row to the same stop ──────────────────────────────────

#[tokio::test(start_paused = true)]
async fn the_scan_pass_and_the_stage_runner_stop_the_same_way_on_a_credit_row() {
    // The stage runner (group review on the batch path).
    let mut report = stage::report_of(&[("SEC-NO-RAW-SQL-CONCAT-1", 3)]);
    let stage_api = Arc::new(stage::FakeBatch::new().erroring(CREDIT_ERROR));
    let stage_llm = stage::seam(&stage_api);
    let stage_meter = UsageMeter::default();
    stage::run(&mut report, &stage_llm, true, Some(&stage_meter)).await;
    assert_eq!(
        stage_api.batch_count(),
        1,
        "the stage runner did not resend"
    );

    // The scan pass, with the same row error text.
    let (scan_api, scan_meter, _ok, failures) = scan_pass_with_a_credit_row().await;
    assert_eq!(
        scan_api.submitted_ids().len(),
        1,
        "the scan pass did not resend"
    );

    let trigger = |m: &UsageMeter| m.health().trigger().map(|t| (t.class, t.detail));
    assert!(trigger(&stage_meter).is_some(), "the stage runner stopped");
    assert_eq!(
        trigger(&scan_meter),
        trigger(&stage_meter),
        "the same stop reason and the same message"
    );
    assert_eq!(
        scan_meter.health().stop_phrase(),
        stage_meter.health().stop_phrase(),
        "the same client-facing stop clause"
    );
    let stage_classes: Vec<FailureClass> = report.failed_passes.iter().map(|f| f.class).collect();
    assert!(stage_classes.contains(&FailureClass::AuthOrBilling));
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(failures
        .iter()
        .all(|f| f.class == FailureClass::AuthOrBilling));
}

// ── 4. A credit row in the follow-up round (M2) ────────────────────────────────────────────

#[tokio::test(start_paused = true)]
async fn a_credit_row_in_the_follow_up_round_is_billing_and_not_second_tried() {
    let api = FakeBatch::new(|round, id| match (round, id) {
        (0, "c0-b1") => Reply::Errored,
        (1, "c0-b1") => Reply::CreditError,
        _ => Reply::Ok,
    });
    let m = meter(eff(true), None);
    let (ok, failures) = run_three(&api, &m).await;

    assert_eq!(
        api.submitted_ids(),
        vec![vec!["c0-b0", "c0-b1", "c0-b2"], vec!["c0-b1"]],
        "the transient row went out once more, and no further"
    );
    assert_eq!(api.realtime_calls(), 0, "no real-time second try");
    assert!(
        m.outage_stopped(),
        "the follow-up's credit row stopped the run"
    );
    assert_eq!(
        m.health().trigger().map(|t| t.class),
        Some(FailureClass::AuthOrBilling)
    );
    assert_eq!(ok, 2, "{failures:?}");
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert_eq!(
        failures[0].class,
        FailureClass::AuthOrBilling,
        "{failures:?}"
    );
}

/// The first round has a credit row and a transient row: the credit row stops the run, so the
/// transient row's follow-up is refused. It is reported NOT RUN (never lost), and the note says
/// why in the stop's own words.
#[tokio::test(start_paused = true)]
async fn a_transient_row_beside_a_credit_row_is_reported_not_run_with_the_stop_wording() {
    let api = FakeBatch::new(|_round, id| match id {
        "c0-b1" => Reply::CreditError,
        "c0-b2" => Reply::Errored,
        _ => Reply::Ok,
    });
    let m = meter(eff(true), None);
    let (ok, failures) = run_three(&api, &m).await;

    assert_eq!(
        api.submitted_ids(),
        vec![vec!["c0-b0", "c0-b1", "c0-b2"]],
        "nothing is submitted after the stop"
    );
    assert_eq!(api.realtime_calls(), 0);
    assert_eq!(ok, 1, "{failures:?}");
    let record = m.outage_record().expect("the run stopped");
    assert_eq!(record.not_run_passes.len(), 1, "{record:?}");
    assert_eq!(record.not_run_passes[0].rule_batch, 3, "{record:?}");
    assert_eq!(
        record.not_run_passes[0].class,
        FailureClass::OutageStop,
        "{record:?}"
    );
    assert!(
        failures
            .iter()
            .any(|f| f.class == FailureClass::AuthOrBilling),
        "the credit row is a billing failure: {failures:?}"
    );

    // L1: the note for the refused follow-up names the stop, not the spend limit.
    let notes = m.notes();
    let note = notes
        .iter()
        .find(|n| n.contains("c0-b2"))
        .unwrap_or_else(|| panic!("a note names the refused item: {notes:?}"));
    assert!(
        note.contains(m.stop_phrase()),
        "the note uses the stop wording: {note}"
    );
    assert!(
        !note.contains("spend limit was reached"),
        "a credit stop is not the spend limit: {note}"
    );
}

// ── 5. Through the run journal: a credit stop, then a resume (M1) ──────────────────────────

fn journal_git_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let g = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .current_dir(dir.path())
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}");
    };
    g(&["init", "-q", "-b", "main"]);
    g(&["config", "user.email", "t@example.com"]);
    g(&["config", "user.name", "T"]);
    std::fs::write(dir.path().join("a.txt"), "one\n").unwrap();
    g(&["add", "."]);
    g(&["commit", "-q", "-m", "one"]);
    dir
}

fn journal_run(repo: &tempfile::TempDir) -> Arc<crate::run_store::RunStore> {
    use crate::run_store::{ModelsRecord, NewRun, RunOrigin, RunStore};
    RunStore::create(NewRun {
        pin: crate::run_store::engine_pin::EnginePin::for_run(None, &Default::default()),
        rules: None,
        project_key: "proj-1".to_string(),
        repos: vec![("acme/widgets".to_string(), repo.path().to_path_buf())],
        models: ModelsRecord {
            audit: "claude-a".to_string(),
            calibration: "claude-a".to_string(),
            fix: "claude-a".to_string(),
            backend: "api".to_string(),
            mode: "parallel".to_string(),
            resolved: true,
            group_review: String::new(),
            finder: String::new(),
            finder_model: String::new(),
            scan_exclusions: Vec::new(),
        },
        ai_scan_key: crate::scan_cache::AiScanKey {
            audit_model: "claude-a".to_string(),
            calibration_model: String::new(),
            prompt_schema_version: 2,
            directives_fingerprint: "fp1".to_string(),
            terse_output: true,
            applies_to_routing: true,
        },
        args: serde_json::json!({"client": "x"}),
        estimate_usd: Some(10.0),
        include_alternatives: true,
        origin: RunOrigin::Cli,
        project_id: None,
    })
    .unwrap()
}

/// The two fixes together: the scan pass stops on a credit row, the journal keeps the answered
/// rows, and the resumed attempt sends only the credit row again and replays the rest.
#[tokio::test(start_paused = true)]
#[serial_test::serial(process_env)]
async fn a_credit_stop_then_a_resume_through_the_journal_sends_only_the_credit_row() {
    crate::prompt_frame::with_seed(Some([9u8; crate::prompt_frame::NONCE_BYTES]), async {
        use crate::run_store::journal::{stored_spend, JournalBatchApi};
        let state = tempfile::tempdir().unwrap();
        let _g = camerata_test_env::EnvGuard::set("CAMERATA_STATE_DIR", state.path());
        let repo = journal_git_repo();
        let store = journal_run(&repo);
        let fake = Arc::new(FakeBatch::new(|round, id| match (round, id) {
            (0, "c0-b1") => Reply::CreditError,
            _ => Reply::Ok,
        }));

        // Attempt one: the credit row stops the run; the two answered rows are stored.
        let api = JournalBatchApi::new(fake.clone(), store.clone());
        let m1 = meter(eff(true), None);
        let (ok1, failures1) = run_three(&api, &m1).await;
        assert_eq!(ok1, 2, "{failures1:?}");
        assert!(m1.outage_stopped());
        assert_eq!(failures1.len(), 1, "{failures1:?}");
        assert_eq!(failures1[0].class, FailureClass::AuthOrBilling);
        assert_eq!(fake.submitted_ids(), vec![vec!["c0-b0", "c0-b1", "c0-b2"]]);
        let paid = stored_spend(store.dir());
        assert!((paid - 0.02).abs() < 1e-9, "two answered rows: {paid}");
        drop(api);

        // Attempt two, a resume: a new meter, the same journal.
        let id = store.run_id();
        drop(store);
        let store = crate::run_store::RunStore::open(&id).unwrap();
        let api = JournalBatchApi::new(fake.clone(), store.clone());
        let m2 = meter(eff(true), None);
        let (ok2, failures2) = run_three(&api, &m2).await;
        assert_eq!(ok2, 3, "{failures2:?}");
        assert!(failures2.is_empty(), "{failures2:?}");
        assert!(!m2.outage_stopped());
        assert_eq!(
            fake.submitted_ids(),
            vec![vec!["c0-b0", "c0-b1", "c0-b2"], vec!["c0-b1"]],
            "only the credit row went out again"
        );
        assert_eq!(fake.realtime_calls(), 0);
        let paid = stored_spend(store.dir());
        assert!(
            (paid - 0.03).abs() < 1e-9,
            "each row paid once, the replayed ones not again: {paid}"
        );
    })
    .await;
}
