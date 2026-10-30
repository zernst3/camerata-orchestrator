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
