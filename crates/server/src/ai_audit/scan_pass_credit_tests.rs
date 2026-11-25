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
async fn run_three(api: &FakeBatch, meter: &UsageMeter) -> (usize, Vec<PassFailure>) {
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
    assert!(failures
        .iter()
        .all(|f| f.class == FailureClass::AuthOrBilling));
}
