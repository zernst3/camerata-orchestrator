//! WO7c / F5: fix generation and the alternatives pass go out as Message Batches in a batch run.
//!
//! The detail rewrites are pinned in `detail_regen_tests`. These three stages used to pass no
//! batch API to the runner, so a batch run sent them in real time at full price.

use std::sync::Arc;

use super::*;
use crate::ai_audit::loop_batch::LoopBatch;
use crate::ai_audit::stage_batch::StageRoute;
use crate::cost_cap::RunControls;

const FIX_REPLY: &str = r#"{"fixes":[{"index":0,"fix":"Use `safeInternalPath` instead of `rawRedirect` in app/auth/signout/route.ts."}]}"#;
const ALTERNATIVES_REPLY: &str = r#"{"recommendations":[{"rule_id":"MULTI-RULE-1","recommended_option_id":"opt-a","applicable":true,"evidence":"a.rs:1"}]}"#;

fn capped(cap: f64) -> UsageMeter {
    UsageMeter::with_controls(RunControls {
        review: Default::default(),
        efficiency: Default::default(),
        max_cost_usd: Some(cap),
        estimate_hint_usd: None,
        fix_model: None,
        clone_judge_model: None,
        consensus: false,
    })
}

const BATCH: StageRoute<'static> = StageRoute {
    batch: true,
    job: None,
};

// ── fix generation ──────────────────────────────────────────────────────────────────────────

fn fix_files() -> Vec<(String, String)> {
    vec![(
        "app/auth/signout/route.ts".to_string(),
        "export function safeInternalPath(p: string) {}\nexport function handler() { \
             rawRedirect(p) }"
            .to_string(),
    )]
}

fn redirect_finding() -> Finding {
    fx(
        "SEC-OPEN-REDIRECT-1",
        "app/auth/signout/route.ts",
        2,
        "rawRedirect(p) is called with an unvalidated path \u{2014} an open redirect.",
        "rawRedirect(p)",
    )
}

async fn fix_with(
    llm: &dyn LlmPort,
    route: StageRoute<'_>,
    meter: Option<&UsageMeter>,
) -> (Vec<Finding>, Vec<FailedPass>) {
    generate_fix_specifics_in(
        llm,
        "o/r",
        vec![redirect_finding()],
        &fix_files(),
        None,
        meter,
        None,
        route,
    )
    .await
}

#[tokio::test(start_paused = true)]
async fn a_batch_run_sends_fix_generation_as_a_batch_and_absorbs_the_fix_as_in_real_time() {
    let real_time = SequencedCompleter::new(&[FIX_REPLY]);
    let (expected, _) = fix_with(&real_time, StageRoute::default(), None).await;

    let stub = Arc::new(SequencedCompleter::new(&[FIX_REPLY]));
    let api = LoopBatch::new(stub.clone());
    let seam = api.seam();
    let (out, failed) = fix_with(&seam, BATCH, None).await;

    assert_eq!(api.submit_calls(), 1, "the fix round went out as one batch");
    assert_eq!(
        stub.calls.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "the batch item was the only request"
    );
    assert!(failed.is_empty(), "{failed:?}");
    assert!(out[0].fix_specific.is_some());
    assert_eq!(out[0].fix_specific, expected[0].fix_specific);
}

#[tokio::test(start_paused = true)]
async fn a_real_time_run_submits_no_batch_for_fix_generation() {
    let stub = Arc::new(SequencedCompleter::new(&[FIX_REPLY]));
    let api = LoopBatch::new(stub.clone());
    let seam = api.seam();
    let (out, failed) = fix_with(&seam, StageRoute::default(), None).await;

    assert_eq!(api.submit_calls(), 0);
    assert_eq!(stub.calls.load(std::sync::atomic::Ordering::Relaxed), 1);
    assert!(failed.is_empty(), "{failed:?}");
    assert!(out[0].fix_specific.is_some());
}

#[tokio::test(start_paused = true)]
async fn a_capped_batch_run_prices_the_fix_window_from_its_real_bytes() {
    let stub = Arc::new(SequencedCompleter::new(&[FIX_REPLY]));
    let api = LoopBatch::new(stub.clone());
    let seam = api.seam();
    let meter = capped(0.000_001);
    let (_, _) = fix_with(&seam, BATCH, Some(&meter)).await;

    assert_eq!(api.submit_calls(), 0, "the over-limit window was not sent");
    assert_eq!(stub.calls.load(std::sync::atomic::Ordering::Relaxed), 0);
    let record = meter.cost_cap_record().unwrap();
    assert!(
        record.not_run_stages.iter().any(|s| s.stage == STAGE_FIX),
        "{:?}",
        record.not_run_stages
    );
    assert!(
        record.notes.iter().any(|n| n.contains("NOT submitted")),
        "{:?}",
        record.notes
    );

    let stub = Arc::new(SequencedCompleter::new(&[FIX_REPLY]));
    let api = LoopBatch::new(stub.clone());
    let seam = api.seam();
    let meter = capped(100.0);
    let (out, failed) = fix_with(&seam, BATCH, Some(&meter)).await;
    assert_eq!(api.submit_calls(), 1);
    assert!(failed.is_empty(), "{failed:?}");
    assert!(out[0].fix_specific.is_some());
}

// ── alternatives ────────────────────────────────────────────────────────────────────────────

async fn alternatives_with(
    llm: &dyn LlmPort,
    route: StageRoute<'_>,
    meter: Option<&UsageMeter>,
) -> anyhow::Result<(Vec<RuleRecommendation>, Option<String>)> {
    let files = vec![("a.rs".to_string(), "fn f() {}".to_string())];
    recommend_alternatives_in(
        llm,
        "me/api",
        &files,
        &files,
        &crate::prompt_frame::FrameNonce::generate(),
        &[two_option_alternatives("MULTI-RULE-1", Some("opt-a"))],
        None,
        None,
        meter,
        route,
    )
    .await
}

#[tokio::test(start_paused = true)]
async fn a_batch_run_sends_the_alternatives_call_as_a_batch_and_absorbs_it_as_in_real_time() {
    let real_time = SequencedCompleter::new(&[ALTERNATIVES_REPLY]);
    let (expected, _) = alternatives_with(&real_time, StageRoute::default(), None)
        .await
        .unwrap();

    let stub = Arc::new(SequencedCompleter::new(&[ALTERNATIVES_REPLY]));
    let api = LoopBatch::new(stub.clone());
    let seam = api.seam();
    let (recs, unfinished) = alternatives_with(&seam, BATCH, None).await.unwrap();

    assert_eq!(api.submit_calls(), 1, "the call went out as a batch");
    assert_eq!(stub.calls.load(std::sync::atomic::Ordering::Relaxed), 1);
    assert!(unfinished.is_none());
    assert_eq!(recs.len(), 1);
    assert_eq!(recs.len(), expected.len());
    assert_eq!(recs[0].rule_id, expected[0].rule_id);
    assert_eq!(
        recs[0].recommended_option_id,
        expected[0].recommended_option_id
    );
}

#[tokio::test(start_paused = true)]
async fn a_real_time_run_submits_no_batch_for_the_alternatives_call() {
    let stub = Arc::new(SequencedCompleter::new(&[ALTERNATIVES_REPLY]));
    let api = LoopBatch::new(stub.clone());
    let seam = api.seam();
    let (recs, _) = alternatives_with(&seam, StageRoute::default(), None)
        .await
        .unwrap();

    assert_eq!(api.submit_calls(), 0);
    assert_eq!(stub.calls.load(std::sync::atomic::Ordering::Relaxed), 1);
    assert_eq!(recs.len(), 1);
}

#[tokio::test(start_paused = true)]
async fn a_capped_batch_run_prices_the_alternatives_window_from_its_real_bytes() {
    let stub = Arc::new(SequencedCompleter::new(&[ALTERNATIVES_REPLY]));
    let api = LoopBatch::new(stub.clone());
    let seam = api.seam();
    let meter = capped(0.000_001);
    let result = alternatives_with(&seam, BATCH, Some(&meter)).await;

    assert!(
        result.is_err(),
        "the refused call is an error the caller discloses"
    );
    assert_eq!(api.submit_calls(), 0, "the over-limit window was not sent");
    assert_eq!(stub.calls.load(std::sync::atomic::Ordering::Relaxed), 0);
    let record = meter.cost_cap_record().unwrap();
    assert!(
        record
            .not_run_stages
            .iter()
            .any(|s| s.stage == STAGE_ALTERNATIVES),
        "{:?}",
        record.not_run_stages
    );

    let stub = Arc::new(SequencedCompleter::new(&[ALTERNATIVES_REPLY]));
    let api = LoopBatch::new(stub.clone());
    let seam = api.seam();
    let meter = capped(100.0);
    let (recs, _) = alternatives_with(&seam, BATCH, Some(&meter)).await.unwrap();
    assert_eq!(api.submit_calls(), 1);
    assert_eq!(recs.len(), 1);
}
