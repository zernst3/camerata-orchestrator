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

/// D4: a batch whose results could not be fetched may still be running and billed. Every later
/// round and the end-of-run second try left it alone: the findings stay pending, the failed part
/// names the batch and a resume polls it.
#[tokio::test(start_paused = true)]
async fn a_fix_batch_whose_results_cannot_be_fetched_is_never_submitted_again() {
    let stub = Arc::new(SequencedCompleter::new(&[FIX_REPLY]));
    let api = LoopBatch::new(stub.clone());
    api.fail_next_fetch();
    let seam = api.seam();
    let (out, failed) = fix_with(&seam, BATCH, None).await;

    assert_eq!(
        api.submit_calls(),
        1,
        "the finding was not sent again while its first batch may still be billed"
    );
    assert_eq!(stub.calls.load(std::sync::atomic::Ordering::Relaxed), 0);
    assert!(out[0].fix_specific.is_none());
    assert_eq!(failed.len(), 1, "{failed:?}");
    assert!(
        failed[0].reason.contains("msgbatch_0"),
        "the failed part names the live batch: {}",
        failed[0].reason
    );
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

/// D4: the same for the alternatives call: no second try while its batch may still be billed.
#[tokio::test(start_paused = true)]
async fn an_alternatives_batch_whose_results_cannot_be_fetched_is_never_submitted_again() {
    let stub = Arc::new(SequencedCompleter::new(&[ALTERNATIVES_REPLY]));
    let api = LoopBatch::new(stub.clone());
    api.fail_next_fetch();
    let seam = api.seam();
    let result = alternatives_with(&seam, BATCH, None).await;

    assert_eq!(api.submit_calls(), 1, "the call was not sent again");
    assert_eq!(stub.calls.load(std::sync::atomic::Ordering::Relaxed), 0);
    let err = result.expect_err("the call got no reply");
    assert!(
        format!("{err:#}").contains("msgbatch_0"),
        "the error names the live batch: {err:#}"
    );
}

// ── WO7c: a live fix batch is settled finding by finding on resume ───────────────────────────────

mod resume_by_member {
    use super::*;
    use crate::checkpoint::{with_run_checkpoints, AbsorbedLine, BatchMember, StageCheckpoint};

    /// Three fixes, written by position: the first, second and third finding of the batch asked.
    const THREE_FIXES: &str = r#"{"fixes":[
        {"index":0,"fix":"Use `safeInternalPath` instead of `rawRedirect` in app/auth/signout/route.ts for the first site."},
        {"index":1,"fix":"Use `safeInternalPath` instead of `rawRedirect` in app/auth/signout/route.ts for the second site."},
        {"index":2,"fix":"Use `safeInternalPath` instead of `rawRedirect` in app/auth/signout/route.ts for the third site."}]}"#;

    /// A file with the redirect call on lines 2, 22, 42 and 62, so each finding has code round it.
    fn files() -> Vec<(String, String)> {
        let mut text = String::from("export function safeInternalPath(p: string) {}\n");
        for line in 2..=70 {
            if line % 20 == 2 {
                text.push_str("export function handler() { rawRedirect(p) }\n");
            } else {
                text.push_str(&format!("// line {line}\n"));
            }
        }
        vec![("app/auth/signout/route.ts".to_string(), text)]
    }

    fn at(line: usize) -> Finding {
        fx(
            "SEC-OPEN-REDIRECT-1",
            "app/auth/signout/route.ts",
            line,
            "rawRedirect(p) is called with an unvalidated path \u{2014} an open redirect.",
            "rawRedirect(p)",
        )
    }

    async fn run(
        dir: &std::path::Path,
        llm: &dyn LlmPort,
        findings: Vec<Finding>,
        meter: Option<&UsageMeter>,
    ) -> (Vec<Finding>, Vec<FailedPass>) {
        with_run_checkpoints(
            Some(dir.to_path_buf()),
            generate_fix_specifics_in(llm, "o/r", findings, &files(), None, meter, None, BATCH),
        )
        .await
    }

    async fn first_process_with_a_live_batch(
        dir: &std::path::Path,
    ) -> (Arc<LoopBatch>, Arc<SequencedCompleter>) {
        let stub = Arc::new(SequencedCompleter::new(&[THREE_FIXES]));
        let api = LoopBatch::new(stub.clone());
        api.fail_next_fetch();
        let seam = api.seam();
        let (_, failed) = run(dir, &seam, vec![at(2), at(22), at(42)], None).await;
        assert_eq!(api.submit_calls(), 1);
        assert!(
            failed.iter().any(|f| f.reason.contains("msgbatch_0")),
            "the failed part names the live batch: {failed:?}"
        );
        assert_eq!(stub.calls.load(std::sync::atomic::Ordering::Relaxed), 0);
        (api, stub)
    }

    fn fix_of(f: &Finding) -> &str {
        f.fix_specific.as_deref().unwrap_or_default()
    }

    #[tokio::test(start_paused = true)]
    async fn a_live_batch_reaches_its_findings_whatever_batch_the_resumed_round_builds() {
        let dir = tempfile::tempdir().unwrap();
        let (api, _stub) = first_process_with_a_live_batch(dir.path()).await;
        api.heal();

        // A new finding listed first (every index moves) and line 2, which another reply wrote.
        let second = vec![at(62), at(2), at(22), at(42)];
        let keys = crate::inspections::site_keys_of(&second);
        StageCheckpoint::new(dir.path(), "fix_o/r")
            .record_absorbed(&AbsorbedLine {
                key: "r1:elsewhere".to_string(),
                model: "stub".to_string(),
                backend: "stub".to_string(),
                cost_usd: Some(0.01),
                input_tokens: Some(100),
                output_tokens: Some(50),
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
                text: r#"{"fixes":[{"id":"abcdef","fix":"Use `safeInternalPath` instead of `rawRedirect` in app/auth/signout/route.ts, written elsewhere."}]}"#.to_string(),
                members: vec![BatchMember {
                    key: keys[1].clone(),
                    tag: "abcdef".to_string(),
                    round: "r1".to_string(),
                }],
            })
            .unwrap();

        let meter = UsageMeter::default();
        let seam = api.seam();
        let (out, failed) = run(dir.path(), &seam, second, Some(&meter)).await;

        assert!(failed.is_empty(), "{failed:?}");
        assert!(fix_of(&out[1]).ends_with("written elsewhere."), "{out:?}");
        assert!(fix_of(&out[2]).ends_with("for the second site."), "{out:?}");
        assert!(fix_of(&out[3]).ends_with("for the third site."), "{out:?}");
        assert!(
            fix_of(&out[0]).ends_with("for the first site."),
            "the new finding's own request answered it: {out:?}"
        );
        assert_eq!(
            api.submit_calls(),
            2,
            "only the new finding was submitted; the held members were not sent again"
        );
        let notes = meter.notes();
        assert!(
            !notes
                .iter()
                .any(|n| n.contains("no longer") || n.contains("wasted")),
            "nothing was wasted: {notes:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_returned_row_for_a_finding_no_longer_in_the_run_is_disclosed_and_absorbed_nowhere() {
        let dir = tempfile::tempdir().unwrap();
        let (api, _stub) = first_process_with_a_live_batch(dir.path()).await;
        api.heal();

        let meter = UsageMeter::default();
        let seam = api.seam();
        let (out, failed) = run(dir.path(), &seam, vec![at(2), at(22)], Some(&meter)).await;

        assert!(failed.is_empty(), "{failed:?}");
        assert!(fix_of(&out[0]).ends_with("for the first site."), "{out:?}");
        assert!(fix_of(&out[1]).ends_with("for the second site."), "{out:?}");
        assert!(
            !out.iter().any(|f| fix_of(f).contains("third")),
            "the fix for the finding that is gone was written into another finding"
        );
        assert_eq!(api.submit_calls(), 1, "nothing was submitted again");
        let notes = meter.notes();
        assert!(
            notes.iter().any(|n| n.contains("no longer in this run")),
            "the wasted share is on the ledger: {notes:?}"
        );
    }
}
