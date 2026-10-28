//! Rank 1: every model-calling stage folds its usage into the meter UNDER ITS OWN STAGE, so the
//! per-stage telemetry table can say where an inspection's money went. The aggregate
//! [`UsageMeter::snapshot`] must stay exactly the sum (it feeds the client-visible
//! `actual_usage`, a protected shape), so each test also checks the aggregate.

use super::*;
use crate::llm::{LlmPort, LlmRequest, LlmResponse};
use crate::stage_telemetry::Stage;

/// Answers every call with canned text and a fixed, fully-reported usage.
struct UsageStub {
    text: String,
}

#[async_trait::async_trait]
impl LlmPort for UsageStub {
    async fn complete(&self, _req: LlmRequest) -> anyhow::Result<LlmResponse> {
        Ok(LlmResponse {
            text: self.text.clone(),
            model: "stub".to_string(),
            backend: "api".to_string(),
            cost_usd: Some(0.5),
            input_tokens: Some(1_000),
            output_tokens: Some(100),
            cache_read_input_tokens: 200,
            cache_creation_input_tokens: 300,
            or_cache_discount: None,
            truncated: false,
        })
    }
    async fn complete_streaming(
        &self,
        req: LlmRequest,
        on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> anyhow::Result<LlmResponse> {
        on_delta(&self.text);
        self.complete(req).await
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// The stages the meter saw, and the aggregate call count, for a quick two-sided assertion.
fn stages_seen(meter: &UsageMeter) -> Vec<Stage> {
    meter
        .stage_telemetry(1_000)
        .stages
        .iter()
        .map(|s| s.stage)
        .collect()
}

fn fixture_finding() -> Finding {
    Finding {
        repo: "o/r".to_string(),
        path: "a.rs".to_string(),
        line: 3,
        rule_id: "ARCH-1".to_string(),
        severity: "high".to_string(),
        snippet: "let x = 1;".to_string(),
        detail: "a real defect".to_string(),
        ..Finding::default()
    }
}

#[test]
fn record_stage_keeps_the_aggregate_equal_to_the_sum_of_the_stages() {
    let meter = UsageMeter::default();
    let r = |cost| LlmResponse {
        text: String::new(),
        model: "m".to_string(),
        backend: "api".to_string(),
        cost_usd: Some(cost),
        input_tokens: Some(100),
        output_tokens: Some(10),
        cache_read_input_tokens: 40,
        cache_creation_input_tokens: 20,
        or_cache_discount: None,
        truncated: false,
    };
    meter.record_stage(Stage::ScanPass, &r(1.0));
    meter.record_stage(Stage::ScanPass, &r(1.0));
    meter.record_stage(Stage::Calibration, &r(0.5));

    let agg = meter.snapshot();
    let t = meter.stage_telemetry(5_000);
    let total = t.total();
    assert_eq!(agg.calls, total.calls);
    assert_eq!(agg.input_tokens, total.input_tokens);
    assert_eq!(agg.output_tokens, total.output_tokens);
    assert_eq!(agg.cache_read_input_tokens, total.cache_read_input_tokens);
    assert_eq!(
        agg.cache_creation_input_tokens,
        total.cache_creation_input_tokens
    );
    assert!((agg.cost_usd - total.cost_usd).abs() < 1e-6);
    assert_eq!(t.counted_lines, 5_000);
}

#[test]
fn a_wasted_attempt_is_attributed_to_its_stage_without_changing_the_aggregate() {
    let meter = UsageMeter::default();
    let resp = LlmResponse {
        text: String::new(),
        model: "m".to_string(),
        backend: "api".to_string(),
        cost_usd: Some(0.2),
        input_tokens: Some(700),
        output_tokens: Some(60),
        cache_read_input_tokens: 0,
        cache_creation_input_tokens: 0,
        or_cache_discount: None,
        truncated: false,
    };
    meter.record_stage(Stage::FixGeneration, &resp);
    meter.record_wasted(Stage::FixGeneration, &resp);
    assert_eq!(meter.snapshot().calls, 1);
    let usage = meter.stage_telemetry(1_000).stages[0].usage.clone();
    assert_eq!(usage.wasted_input_tokens, 700);
    assert_eq!(usage.wasted_output_tokens, 60);
}

#[tokio::test]
async fn a_scan_pass_is_recorded_under_the_scan_stage() {
    let llm = UsageStub {
        text: r#"{"findings":[]}"#.to_string(),
    };
    let meter = UsageMeter::default();
    audit_pass(
        &llm,
        None,
        "prompt".to_string(),
        Vec::new(),
        "o/r",
        &std::collections::HashSet::new(),
        None,
        "s",
        Some(&meter),
    )
    .await
    .expect("the stub answers");
    assert_eq!(stages_seen(&meter), vec![Stage::ScanPass]);
    assert_eq!(meter.snapshot().calls, 1);
}

#[tokio::test]
async fn calibration_is_recorded_under_the_calibration_stage() {
    let llm = UsageStub {
        text: r#"{"verdicts":[]}"#.to_string(),
    };
    let meter = UsageMeter::default();
    let _ = verify_findings(
        &llm,
        "o/r",
        vec![fixture_finding()],
        None,
        None,
        Some(&meter),
        false,
        "",
    )
    .await;
    assert_eq!(stages_seen(&meter), vec![Stage::Calibration]);
}

#[tokio::test]
async fn fix_generation_is_recorded_under_the_fix_stage() {
    let llm = UsageStub {
        text: r#"{"fixes":[{"index":0,"fix":"Use `neverInEvidence` here."}]}"#.to_string(),
    };
    let meter = UsageMeter::default();
    let _ = generate_fix_specifics(
        &llm,
        "o/r",
        vec![fixture_finding()],
        &[],
        None,
        Some(&meter),
        None,
    )
    .await;
    assert_eq!(stages_seen(&meter), vec![Stage::FixGeneration]);
}

#[tokio::test]
async fn the_alternatives_pass_is_recorded_under_its_own_stage() {
    let llm = UsageStub {
        text: r#"{"recommendations":[]}"#.to_string(),
    };
    let meter = UsageMeter::default();
    let alternatives = vec![RuleAlternatives {
        rule_id: "ARCH-ALT-1".to_string(),
        options: vec![crate::onboard::RuleOptionView {
            id: "opt-a".to_string(),
            label: "A".to_string(),
            directive: "Do A.".to_string(),
            why: String::new(),
            violates: false,
        }],
        selected_option_id: None,
        kind: None,
    }];
    let files = vec![("a.rs".to_string(), "fn a() {}".to_string())];
    let _ = recommend_alternatives(
        &llm,
        "o/r",
        &files,
        &files,
        &alternatives,
        None,
        None,
        Some(&meter),
    )
    .await;
    assert_eq!(stages_seen(&meter), vec![Stage::Alternatives]);
}

#[tokio::test]
async fn the_deep_tier_is_recorded_under_the_deep_stage() {
    let llm = UsageStub {
        text: "{}".to_string(),
    };
    let meter = UsageMeter::default();
    let files = vec![("a.rs".to_string(), "fn a() {}\n".to_string())];
    let _ = run_deep_tier(
        &llm,
        "o/r",
        &files,
        None,
        ScanMode::Parallel,
        None,
        Some(&meter),
        true,
    )
    .await;
    assert!(meter.snapshot().calls > 0, "the deep tier made calls");
    assert_eq!(stages_seen(&meter), vec![Stage::DeepTier]);
}

/// The fix give-up tally (rank 6) and the routing counts (rank 7) are separate fields of the same
/// snapshot: recording both must leave both in one `stage_telemetry`, and each keeps its own
/// numbers (the two merged branches each added one field to this struct).
#[test]
fn fix_outcome_and_routing_both_survive_one_snapshot() {
    use crate::stage_telemetry::{FixOutcome, RoutingTelemetry};
    let meter = UsageMeter::default();
    let outcome = FixOutcome {
        sent_to_model: 7,
        answered: 6,
        gave_up: 2,
        cap_stopped: 1,
        transport_failed: 0,
        authored_without_model: 3,
    };
    meter.record_fix_outcome(outcome);
    meter.record_routing(40, 25);
    meter.record_routing(10, 5);

    let t = meter.stage_telemetry(1_000);
    assert_eq!(t.fix_outcome, Some(outcome));
    assert_eq!(
        t.routing,
        Some(RoutingTelemetry {
            pairs_total: 50,
            pairs_skipped: 30,
        })
    );
    // And neither disappears when the snapshot is taken again.
    let again = meter.stage_telemetry(1_000);
    assert_eq!(again.fix_outcome, t.fix_outcome);
    assert_eq!(again.routing, t.routing);
}
