//! Retry fixes F2: a stage's effort belongs to the STAGE, not the model (P5), and every reply's
//! output cap covers its text plus the thinking allowance of its effort. No network: every model
//! is a recording stub, and the wire body is built by the same functions the transport uses.

use std::sync::Mutex;

use super::*;
use crate::llm::{LlmPort, LlmRequest, LlmResponse};
use crate::stage_telemetry::Stage;

const REPO: &str = "acme/widgets";

/// Every model that takes an effort setting (the 5.x line), plus both whose registry default
/// (`SetHigh`) forced `high` on every call before the stage chose its own.
const MODELS: [&str; 6] = [
    "claude-opus-5-5",
    "claude-sonnet-5-5",
    "claude-haiku-5-5",
    "claude-opus-5",
    "claude-sonnet-5",
    "claude-fable-5-1",
];

/// The wire effort the transport puts on a request for `model`: the batch path's params are built
/// by the same conformer as the real-time body, and `build_batch_item` is public.
fn wire_effort(req: &LlmRequest, model: &str) -> Option<String> {
    let item = crate::llm::build_batch_item("c0-b0", req, model, crate::llm::BATCH_CACHE_TTL);
    let v = serde_json::to_value(&item).unwrap();
    v["params"]["output_config"]["effort"]
        .as_str()
        .map(str::to_string)
}

/// P5: scan high, detail / calibration / alternatives medium, fix low, and the same on every
/// model: Opus 5.5 and Haiku 5.5 carry the registry's `SetHigh` default, which only fills a
/// request that names no effort.
#[test]
fn each_stage_sends_its_own_effort_on_every_model() {
    let expected = [
        (Stage::ScanPass, "high"),
        (Stage::DetailRegeneration, "medium"),
        (Stage::Calibration, "medium"),
        (Stage::Alternatives, "medium"),
        (Stage::FixGeneration, "low"),
    ];
    for (stage, effort) in expected {
        assert_eq!(stage_effort(stage), Some(effort), "{stage:?}");
        for model in MODELS {
            let req = with_stage_effort(LlmRequest::new("p").with_model(model), stage);
            assert_eq!(
                wire_effort(&req, model).as_deref(),
                Some(effort),
                "{stage:?} on {model}"
            );
        }
    }
    // The stages that keep the model's own default do not send one.
    for stage in [Stage::DeepTier, Stage::CloneJudge] {
        assert_eq!(stage_effort(stage), None, "{stage:?}");
    }
    // The scan is never lowered: it is the product.
    assert_eq!(stage_effort(Stage::ScanPass), Some("high"));
}

/// P5: every stage's `max_tokens` covers its text PLUS the allowance of its effort, so a model
/// that thinks as long as the measured stored replies did still has room to write the answer.
#[test]
fn every_stage_cap_covers_its_text_plus_the_thinking_allowance() {
    use camerata_ui_core::scan as s;
    // (cap for a full batch, text of that batch, effort)
    let rows = [
        (
            scan_max_tokens(15),
            15 * s::SCAN_OUTPUT_TOKENS_PER_RULE,
            s::SCAN_EFFORT,
        ),
        (
            s::regen_call_max_tokens(s::REGEN_BATCH_SIZE),
            s::REGEN_BATCH_SIZE as u32 * s::REGEN_TOKENS_PER_FINDING,
            s::DETAIL_EFFORT,
        ),
        (
            calibration_max_tokens(CALIBRATION_BATCH_SIZE),
            CALIBRATION_BATCH_SIZE as u32 * VERIFY_TOKENS_PER_FINDING,
            s::CALIBRATION_EFFORT,
        ),
        (
            alternatives_max_tokens(25),
            25 * s::ALTERNATIVES_OUTPUT_TOKENS_PER_RULE,
            s::ALTERNATIVES_EFFORT,
        ),
        (
            fix_max_tokens(FIX_BATCH_SIZE),
            FIX_BATCH_SIZE as u32 * FIX_TOKENS_PER_FINDING,
            s::FIX_EFFORT,
        ),
    ];
    for (cap, text, effort) in rows {
        assert!(
            cap >= text + s::thinking_allowance_tokens(effort),
            "cap {cap} must cover text {text} + the {effort} allowance"
        );
    }
    // The start figures: 4,000 for medium, 12,000 for high.
    assert_eq!(s::thinking_allowance_tokens("medium"), 4_000);
    assert_eq!(s::thinking_allowance_tokens("high"), 12_000);
}

/// A model that thinks until its cap and writes nothing: every reply is cut off at the limit with
/// no text, billing `max_tokens` output tokens (the Haiku 5.5 shape from the diagnosis).
struct ThinksUntilTheCap {
    seen: Mutex<Vec<LlmRequest>>,
}

#[async_trait::async_trait]
impl LlmPort for ThinksUntilTheCap {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        let out = u64::from(req.max_tokens);
        self.seen.lock().unwrap().push(req);
        Ok(LlmResponse {
            text: String::new(),
            model: "claude-haiku-5-5".to_string(),
            backend: "api".to_string(),
            cost_usd: Some(0.01),
            input_tokens: Some(1_000),
            output_tokens: Some(out),
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            or_cache_discount: None,
            truncated: true,
        })
    }
    async fn complete_streaming(
        &self,
        req: LlmRequest,
        _on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> anyhow::Result<LlmResponse> {
        self.complete(req).await
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// P5 / P7: a reply cut off at the cap with no text is recorded as what it is, a billed call that
/// bought nothing: every send is a wasted call, and the output tokens it billed are the cap.
#[tokio::test]
async fn a_reply_cut_off_at_its_cap_with_no_text_is_recorded_as_wasted_in_the_telemetry() {
    let llm = ThinksUntilTheCap {
        seen: Mutex::new(Vec::new()),
    };
    let meter = UsageMeter::default();
    let files: Vec<(String, String)> = vec![("a.rs".to_string(), "fn a() {}".to_string())];
    let chunks: Vec<&[(String, String)]> = vec![files.as_slice()];
    let rules: Vec<(String, String)> = (0..3)
        .map(|i| (format!("RULE-{i}"), format!("rule {i}")))
        .collect();
    let batches: Vec<&[(String, String)]> = vec![rules.as_slice()];
    let adopted: std::collections::HashSet<String> =
        rules.iter().map(|(id, _)| id.clone()).collect();
    run_passes(
        &llm,
        REPO,
        "REPO MAP:\na.rs: fn a()\n",
        &crate::prompt_frame::FrameNonce::generate(),
        &adopted,
        None,
        None,
        None,
        &chunks,
        None,
        &batches,
        1,
        "pass",
        "caps",
        Some(&meter),
        false,
    )
    .await;
    let seen = llm.seen.lock().unwrap().clone();
    assert!(!seen.is_empty(), "the pass was sent");
    let telemetry = meter.stage_telemetry(1_000);
    let scan = telemetry
        .stages
        .iter()
        .find(|s| s.stage == Stage::ScanPass)
        .expect("the scan stage was recorded");
    assert_eq!(
        scan.usage.wasted_calls, scan.usage.calls,
        "every call of a pass that only ever came back cut off with no text is wasted"
    );
    let billed: u64 = seen.iter().map(|r| u64::from(r.max_tokens)).sum();
    assert_eq!(
        scan.usage.output_tokens, billed,
        "the output billed is what each reply spent: its whole cap"
    );
}

// ── P10: generous caps, headroom, the runaway ceiling, a retry that changes the request ──────

/// P10: a cap is the work with headroom plus the thinking allowance, within the runaway ceiling,
/// and the ceiling is a guard and not a budget: nothing a real batch asks for comes near it.
#[test]
fn a_cap_is_the_work_with_headroom_plus_thinking_within_the_runaway_ceiling() {
    use camerata_ui_core::scan as s;
    assert_eq!(s::OUTPUT_HEADROOM_FACTOR, 2);
    // A full 15-rule scan at high effort: 2 x 9,600 + 12,000.
    assert_eq!(scan_max_tokens(15), 31_200);
    // A full detail batch at medium: 2 x 4,500 + 4,000.
    assert_eq!(s::regen_call_max_tokens(s::REGEN_BATCH_SIZE), 13_000);
    // A full fix batch at low: 2 x 6,250 + 2,000.
    assert_eq!(fix_max_tokens(FIX_BATCH_SIZE), 14_500);
    assert_eq!(calibration_max_tokens(CALIBRATION_BATCH_SIZE), 16_000);
    // Every real batch is well under the ceiling; a runaway request is held at it.
    for cap in [
        scan_max_tokens(15),
        s::regen_call_max_tokens(s::REGEN_BATCH_SIZE),
        fix_max_tokens(FIX_BATCH_SIZE),
        calibration_max_tokens(CALIBRATION_BATCH_SIZE),
        alternatives_max_tokens(25),
    ] {
        assert!(cap < s::AGGREGATE_MAX_TOKENS_CEILING, "{cap}");
    }
    assert_eq!(scan_max_tokens(10_000), s::AGGREGATE_MAX_TOKENS_CEILING);
    // The ceiling follows the model's own limit on the wire: nothing above it is sent.
    const {
        assert!(s::AGGREGATE_MAX_TOKENS_CEILING <= 128_000);
    }
}

/// P10 / P4: the truncation retry stays for a true runaway and CHANGES the request: the second
/// send asks for a higher cap, and a reply already cut off at the ceiling is not sent again
/// unchanged (there is no higher cap to ask for).
#[tokio::test]
async fn the_truncation_retry_raises_the_cap_and_is_never_the_same_request_twice() {
    async fn caps_for(n_rules: usize) -> Vec<u32> {
        let llm = ThinksUntilTheCap {
            seen: Mutex::new(Vec::new()),
        };
        let files: Vec<(String, String)> = vec![("a.rs".to_string(), "fn a() {}".to_string())];
        let chunks: Vec<&[(String, String)]> = vec![files.as_slice()];
        let rules: Vec<(String, String)> = (0..n_rules)
            .map(|i| (format!("RULE-{i}"), format!("rule {i}")))
            .collect();
        let batches: Vec<&[(String, String)]> = vec![rules.as_slice()];
        let adopted: std::collections::HashSet<String> =
            rules.iter().map(|(id, _)| id.clone()).collect();
        run_passes(
            &llm,
            REPO,
            "REPO MAP:\na.rs: fn a()\n",
            &crate::prompt_frame::FrameNonce::generate(),
            &adopted,
            None,
            None,
            None,
            &chunks,
            None,
            &batches,
            1,
            "pass",
            "caps",
            None,
            false,
        )
        .await;
        let seen = llm.seen.lock().unwrap();
        seen.iter().map(|r| r.max_tokens).collect()
    }
    // A normal batch: one retry, at a higher cap (the cut-off reply was a runaway).
    let caps = caps_for(15).await;
    assert_eq!(caps.len(), 2, "the first send and one retry: {caps:?}");
    assert!(caps[1] > caps[0], "the retry raises the cap: {caps:?}");
    // A request already at the ceiling cannot be raised: it is not sent a second time.
    let caps = caps_for(1_000).await;
    assert_eq!(
        caps,
        vec![camerata_ui_core::scan::AGGREGATE_MAX_TOKENS_CEILING],
        "a cut-off at the ceiling is not re-sent unchanged"
    );
}

// ── P10: the spend limit reserves against what a request may cost ────────────────────────────

fn capped_meter(cap: f64) -> UsageMeter {
    UsageMeter::with_controls(crate::cost_cap::RunControls {
        max_cost_usd: Some(cap),
        ..crate::cost_cap::RunControls::default()
    })
}

/// A scan-sized request on Opus 5.5 ($4 / $20 per million) with a 128,000-token cap.
fn opus_request_with_a_128k_cap() -> LlmRequest {
    LlmRequest::new("x".repeat(3_000))
        .with_model("claude-opus-5-5")
        .with_max_tokens(128_000)
}

fn paid_reply(model: &str, cost: f64) -> LlmResponse {
    LlmResponse {
        text: String::new(),
        model: model.to_string(),
        backend: "api".to_string(),
        cost_usd: Some(cost),
        input_tokens: Some(1_000),
        output_tokens: Some(100),
        cache_read_input_tokens: 0,
        cache_creation_input_tokens: 0,
        or_cache_discount: None,
        truncated: false,
    }
}

#[test]
fn the_request_ceiling_prices_the_input_and_the_whole_cap() {
    let ceiling = request_ceiling_usd(&opus_request_with_a_128k_cap()).unwrap();
    // 128,000 output tokens at $20 per million is $2.56; the 3,000-character prompt adds a
    // thousand input tokens at $4 per million.
    assert!((ceiling - (2.56 + 0.004)).abs() < 1e-6, "{ceiling}");
    // A cap above what the model can write is priced at what it can write.
    let huge = LlmRequest::new("p")
        .with_model("claude-opus-5-5")
        .with_max_tokens(900_000);
    let priced = request_ceiling_usd(&huge).unwrap();
    assert!((priced - 2.56).abs() < 0.001, "{priced}");
    // A model with no price adds nothing (the cap's own pre-flight refuses it).
    let unpriced = LlmRequest::new("p")
        .with_model("claude-from-the-future")
        .with_max_tokens(1_000);
    assert_eq!(request_ceiling_usd(&unpriced), None);
}

/// The spend limit's overshoot bound holds with a 128k cap. Before, the reserve was the most a
/// past call had cost, so after one cheap call every pass was admitted and each could cost
/// $2.56 (more than the whole limit between them). Now a pass in flight is reserved at what its
/// request may cost: admission stops while the in-flight reserve plus spend reaches the limit,
/// and the total at worst is the limit plus ONE request's ceiling.
#[tokio::test]
async fn the_spend_limit_overshoot_bound_holds_with_a_128k_cap() {
    let cap = 10.0;
    let meter = capped_meter(cap);
    let req = opus_request_with_a_128k_cap();
    let ceiling = request_ceiling_usd(&req).unwrap();

    // A cheap first call has been seen: the old reserve would now be a cent.
    meter.record_stage(Stage::ScanPass, &paid_reply("claude-opus-5-5", 0.01));
    let spent_before = meter.spent_usd();
    meter.note_request(&req);

    // Every pass the limit admits is in flight at once, and each then costs its whole ceiling.
    let mut permits = Vec::new();
    while let Ok(permit) = meter.admit().await {
        permits.push(permit);
        assert!(permits.len() < 100, "the limit never closed");
    }
    let admitted = permits.len();
    let worst_case_total = spent_before + admitted as f64 * ceiling;
    assert!(
        worst_case_total <= cap + ceiling,
        "{admitted} passes at ${ceiling:.3} each: ${worst_case_total:.2} against a ${cap} limit \
         (bound: limit + one request = ${:.2})",
        cap + ceiling
    );
    // The cheap-reserve behaviour would have admitted the whole limit's worth of cents.
    assert!(admitted <= 4, "admitted {admitted}");
    assert!(admitted >= 3, "the limit still lets work run: {admitted}");
}

/// The first pass of a run is no longer admitted against a zero reserve: with a request's
/// ceiling known, the second concurrent pass is judged against it at once instead of waiting for
/// a first cost to arrive, and it is refused when the first pass's reserve already reaches the
/// limit.
#[tokio::test]
async fn the_reserve_exists_before_any_call_has_cost_anything() {
    let meter = capped_meter(2.0);
    let first = meter.admit().await.expect("the first pass is admitted");
    meter.note_request(&opus_request_with_a_128k_cap());
    // $2.56 reserved for the pass in flight against a $2 limit: nothing more is admitted, and the
    // pass does not wait for a cost that may never be small.
    assert!(meter.admit().await.is_err());
    drop(first);
}

/// With no limit set the reserve is not consulted at all: no request is noted and nothing is
/// ever refused.
#[tokio::test]
async fn a_run_with_no_limit_is_never_refused_by_the_reserve() {
    let meter = UsageMeter::default();
    meter.note_request(&opus_request_with_a_128k_cap());
    let mut permits = Vec::new();
    for _ in 0..50 {
        permits.push(meter.admit().await.expect("no limit, no refusal"));
    }
    assert!(!meter.retry_blocked());
    assert!(!meter.cap_reached());
    // However much has been spent, no stage, repo or batch submission is refused on cost.
    meter.record_stage(Stage::ScanPass, &paid_reply("claude-opus-5-5", 1_000_000.0));
    assert!(!meter.stage_blocked("acme/widgets", STAGE_CALIBRATION));
    assert!(!meter.repo_blocked("acme/widgets"));
    assert_eq!(meter.batch_refusal_reason(Some(1_000_000.0)), None);
    assert_eq!(meter.batch_refusal_reason(None), None);
    assert!(meter.admit().await.is_ok());
    // And the controls a run builds by default carry no limit and refuse no model.
    let controls = crate::cost_cap::RunControls::default();
    assert_eq!(controls.max_cost_usd, None);
    assert_eq!(
        controls.refuse_if_cap_unenforceable(&[(
            "scan".to_string(),
            "claude-from-the-future".to_string()
        )]),
        None
    );
}

// ── P8: a calibration reply cut off at the cap is salvaged, and only the rest is asked again ──

fn finding_n(n: usize) -> Finding {
    Finding {
        repo: REPO.to_string(),
        path: format!("src/f{n}.rs"),
        line: n + 1,
        rule_id: "AI-CAL".to_string(),
        severity: "medium".to_string(),
        snippet: format!("let marker_{n} = {n};"),
        detail: format!("DETAIL-MARKER-{n}"),
        ..Finding::default()
    }
}

fn verdict(index: usize, severity: &str) -> String {
    format!(
        r#"{{"index":{index},"severity":"{severity}","confidence":"high","reason":"checked","effort":"low"}}"#
    )
}

/// Answers calibration calls from a script, in order, and records every request.
struct Scripted {
    replies: Mutex<Vec<(String, bool)>>,
    seen: Mutex<Vec<LlmRequest>>,
}

impl Scripted {
    fn new(replies: Vec<(String, bool)>) -> Self {
        Self {
            replies: Mutex::new(replies),
            seen: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait::async_trait]
impl LlmPort for Scripted {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        let max = u64::from(req.max_tokens);
        self.seen.lock().unwrap().push(req);
        let mut replies = self.replies.lock().unwrap();
        let (text, truncated) = if replies.len() > 1 {
            replies.remove(0)
        } else {
            replies[0].clone()
        };
        Ok(LlmResponse {
            text,
            model: "claude-sonnet-5-5".to_string(),
            backend: "api".to_string(),
            cost_usd: Some(0.01),
            input_tokens: Some(1_000),
            output_tokens: Some(if truncated { max } else { 100 }),
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            or_cache_discount: None,
            truncated,
        })
    }
    async fn complete_streaming(
        &self,
        req: LlmRequest,
        _on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> anyhow::Result<LlmResponse> {
        self.complete(req).await
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

async fn calibrate(
    llm: &Scripted,
    n: usize,
    meter: Option<&UsageMeter>,
) -> (Vec<Finding>, Vec<FailedPass>) {
    verify_findings(
        llm,
        REPO,
        (0..n).map(finding_n).collect(),
        None,
        None,
        meter,
        false,
        "This repository has many code files.",
    )
    .await
}

fn severity_of(findings: &[Finding], n: usize) -> String {
    findings
        .iter()
        .find(|f| f.detail.contains(&format!("DETAIL-MARKER-{n}")))
        .unwrap_or_else(|| panic!("finding {n} missing"))
        .severity
        .clone()
}

/// The reply holds verdicts 0 and 1 in full and stops in the middle of the third. Those two are
/// kept; the second request carries ONLY findings 2, 3 and 4 (numbered 0 to 2), and its verdicts
/// land on the right findings. Two calls in all, and the second is a different, smaller request.
#[tokio::test]
async fn a_cut_off_calibration_reply_keeps_its_complete_verdicts_and_only_the_rest_is_asked_again()
{
    let cut = format!(
        r#"{{"verdicts":[{},{},{{"index":2,"severity":"hi"#,
        verdict(0, "low"),
        verdict(1, "high")
    );
    let rest = format!(
        r#"{{"verdicts":[{},{},{}]}}"#,
        verdict(0, "low"),
        verdict(1, "critical"),
        verdict(2, "high")
    );
    let llm = Scripted::new(vec![(cut, true), (rest, false)]);
    let meter = UsageMeter::default();
    let (out, failed) = calibrate(&llm, 5, Some(&meter)).await;

    let seen = llm.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 2, "the call and one re-ask");
    // The re-ask is a different request: the unanswered findings alone, and a cap sized for 3.
    for answered in [0, 1] {
        assert!(
            !seen[1]
                .prompt
                .contains(&format!("DETAIL-MARKER-{answered}")),
            "finding {answered} was answered and is not asked again"
        );
    }
    for unanswered in [2, 3, 4] {
        assert!(
            seen[1]
                .prompt
                .contains(&format!("DETAIL-MARKER-{unanswered}")),
            "finding {unanswered} is asked again"
        );
    }
    assert_ne!(seen[0].prompt, seen[1].prompt);
    assert_eq!(seen[1].max_tokens, calibration_max_tokens(3));
    assert_ne!(seen[0].max_tokens, seen[1].max_tokens);
    // Every verdict reached its own finding: 0 and 1 from the cut-off reply, 2 to 4 from the
    // re-ask (local 0, 1, 2 mapped back to 2, 3, 4).
    assert_eq!(severity_of(&out, 0), "low");
    assert_eq!(severity_of(&out, 1), "high");
    assert_eq!(severity_of(&out, 2), "low");
    assert_eq!(severity_of(&out, 3), "critical");
    assert_eq!(severity_of(&out, 4), "high");
    assert!(out.iter().all(|f| f.effort.as_deref() == Some("low")));
    assert!(
        failed.is_empty(),
        "every finding has its estimate: {failed:?}"
    );
    // The cut-off call is wasted by the share it left unanswered: counted once.
    let calibration = meter
        .stage_telemetry(1_000)
        .stages
        .into_iter()
        .find(|s| s.stage == Stage::Calibration)
        .unwrap();
    assert_eq!(calibration.usage.calls, 2);
    assert_eq!(calibration.usage.wasted_calls, 1);
}

/// A reply cut off before the first verdict answered nothing: ALL findings are asked again, alone
/// (a smaller request than a repeat of the same one would be only if the batch were smaller; the
/// cap is the same sum, and the call is recorded as wasted whole).
#[tokio::test]
async fn a_reply_cut_off_before_any_verdict_is_wasted_whole_and_every_finding_is_asked_again() {
    let cut = r#"{"verdicts":[{"index":0,"sever"#.to_string();
    let all = format!(
        r#"{{"verdicts":[{},{},{}]}}"#,
        verdict(0, "low"),
        verdict(1, "low"),
        verdict(2, "low")
    );
    let llm = Scripted::new(vec![(cut, true), (all, false)]);
    let meter = UsageMeter::default();
    let (out, _) = calibrate(&llm, 3, Some(&meter)).await;
    assert_eq!(llm.seen.lock().unwrap().len(), 2);
    assert!((0..3).all(|n| severity_of(&out, n) == "low"));
    let calibration = meter
        .stage_telemetry(1_000)
        .stages
        .into_iter()
        .find(|s| s.stage == Stage::Calibration)
        .unwrap();
    assert_eq!(calibration.usage.wasted_calls, 1);
}

/// The re-ask is itself one retry, not a loop: a second cut-off keeps the verdicts it did write
/// and the findings still unanswered pass through as the scan left them. Three calls never happen.
#[tokio::test]
async fn a_second_cut_off_keeps_what_it_answered_and_does_not_ask_a_third_time() {
    let first = format!(r#"{{"verdicts":[{},{{"index":1,"sev"#, verdict(0, "high"));
    let second = format!(
        r#"{{"verdicts":[{},{{"index":1,"sev"#,
        verdict(0, "critical")
    );
    let llm = Scripted::new(vec![(first, true), (second, true)]);
    let (out, _) = calibrate(&llm, 4, None).await;
    assert_eq!(llm.seen.lock().unwrap().len(), 2, "one re-ask, never more");
    assert_eq!(severity_of(&out, 0), "high");
    assert_eq!(
        severity_of(&out, 1),
        "critical",
        "the re-ask's first verdict"
    );
    assert_eq!(severity_of(&out, 2), "medium", "never reached: unchanged");
    assert_eq!(severity_of(&out, 3), "medium", "never reached: unchanged");
    assert_eq!(out.len(), 4, "none dropped");
}

/// The spend limit stops the re-ask like any retry: the verdicts that were written out in full
/// still apply, the rest pass through, and no further call is sent.
#[tokio::test]
async fn the_re_ask_goes_through_the_spend_limit() {
    let cut = format!(r#"{{"verdicts":[{},{{"index":1,"sev"#, verdict(0, "high"));
    let llm = Scripted::new(vec![(cut, true)]);
    // A limit the first call has already reached.
    let meter = UsageMeter::with_controls(crate::cost_cap::RunControls {
        max_cost_usd: Some(0.005),
        ..crate::cost_cap::RunControls::default()
    });
    let (out, _) = calibrate(&llm, 3, Some(&meter)).await;
    assert_eq!(
        llm.seen.lock().unwrap().len(),
        1,
        "no re-ask past the limit"
    );
    assert_eq!(severity_of(&out, 0), "high");
    assert_eq!(severity_of(&out, 1), "medium");
}

/// `apply_verdicts` reads a cut-off reply on its own account too: complete verdicts apply, the
/// unfinished one is not guessed at, and a reply with none leaves the findings as they were.
#[test]
fn apply_verdicts_reads_the_complete_verdicts_of_a_reply_cut_off_mid_object() {
    let findings: Vec<Finding> = (0..3).map(finding_n).collect();
    let raw = format!(
        r#"{{"verdicts":[{},{},{{"index":2,"severity":"cri"#,
        verdict(0, "low"),
        verdict(1, "high")
    );
    let out = apply_verdicts(&raw, findings.clone());
    assert_eq!(severity_of(&out, 0), "low");
    assert_eq!(severity_of(&out, 1), "high");
    assert_eq!(
        severity_of(&out, 2),
        "medium",
        "the unfinished verdict is not guessed at"
    );

    let none = apply_verdicts(r#"{"verdicts":[{"index":0,"sev"#, findings.clone());
    assert_eq!(none.len(), 3);
    assert!(none.iter().all(|f| f.severity == "medium"));
}
