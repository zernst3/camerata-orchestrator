//! The loop prices every turn from the model registry and stops at a driver spend limit.
//! `claude-fable-5` lists at $10 per million input tokens and $50 per million output tokens.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use serde_json::json;

use super::*;

const PRICED_MODEL: &str = "claude-fable-5";

/// Answers every turn with one `LS` tool call (so the loop never finishes by itself) and reports
/// the given token counts but NO dollar figure, like the real transports.
struct Spendthrift {
    calls: AtomicUsize,
    input_tokens: u64,
    output_tokens: u64,
}

impl Spendthrift {
    fn new(input_tokens: u64, output_tokens: u64) -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            input_tokens,
            output_tokens,
        })
    }
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl LlmPort for Spendthrift {
    async fn complete(&self, _req: LlmRequest) -> anyhow::Result<LlmResponse> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        let text = json!({"message": {"role": "assistant", "content": null, "tool_calls": [{
            "id": format!("call_{n}"), "type": "function",
            "function": {"name": "LS", "arguments": "{}"}
        }]}})
        .to_string();
        Ok(LlmResponse {
            reported_model: None,
            cli_cost_usd: None,
            text,
            model: PRICED_MODEL.to_string(),
            backend: "stub".to_string(),
            cost_usd: None,
            input_tokens: Some(self.input_tokens),
            output_tokens: Some(self.output_tokens),
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            or_cache_discount: None,
            truncated: false,
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

fn role() -> camerata_core::Role {
    camerata_core::Role {
        name: "t".to_string(),
        rule_subset: vec![],
        allowed_paths: vec![],
    }
}

fn wt() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

#[tokio::test]
async fn a_run_that_would_exceed_the_limit_stops_at_the_limit_with_a_clear_reason() {
    let tmp = wt();
    // One turn is 1,000,000 input tokens = $10.00 at the list price; the limit is $1.00.
    let completer = Spendthrift::new(1_000_000, 0);
    let driver =
        ApiAgentDriver::new(completer.clone(), PRICED_MODEL, tmp.path()).with_spend_limit(1.0);

    let outcome = driver.run(&role(), "go").await.unwrap();

    assert_eq!(completer.calls(), 1, "no second request after the limit");
    assert!(
        outcome.result.starts_with("INCOMPLETE"),
        "{}",
        outcome.result
    );
    assert!(outcome.result.contains("spend limit"), "{}", outcome.result);
    assert!(
        outcome.result.contains("$1.00"),
        "names the limit: {}",
        outcome.result
    );
    let cost = outcome.cost_usd.expect("the run reports what it spent");
    assert!((cost - 10.0).abs() < 1e-6, "priced from tokens: {cost}");
}

#[tokio::test]
async fn cost_is_recorded_per_turn_from_the_registry_price() {
    let tmp = wt();
    // $10 per million input tokens and $50 per million output: 100k in + 20k out = $2.00.
    let completer = Spendthrift::new(100_000, 20_000);
    let driver =
        ApiAgentDriver::new(completer.clone(), PRICED_MODEL, tmp.path()).with_spend_limit(5.0);

    let outcome = driver.run(&role(), "go").await.unwrap();

    let turns = driver.spend_meter().turns();
    assert_eq!(turns.len(), completer.calls(), "one record per turn");
    assert!(turns.len() >= 2, "{turns:?}");
    for t in &turns {
        assert_eq!(t.model, PRICED_MODEL);
        assert_eq!(t.input_tokens, Some(100_000));
        assert_eq!(t.output_tokens, Some(20_000));
        let c = t.cost_usd.expect("every turn is priced");
        assert!((c - 2.0).abs() < 1e-6, "{c}");
    }
    let total: f64 = turns.iter().filter_map(|t| t.cost_usd).sum();
    assert!((outcome.cost_usd.unwrap() - total).abs() < 1e-6);
    assert!((driver.spend_meter().spent_usd() - total).abs() < 1e-6);
    // $2.00 a turn against a $5.00 limit: the third request would be admitted only if
    // $4.00 spent plus one request's ceiling stayed under $5.00; it does not.
    assert!(
        outcome.result.starts_with("INCOMPLETE"),
        "{}",
        outcome.result
    );
}

#[tokio::test]
async fn a_driver_with_no_limit_is_not_stopped_by_spend() {
    let tmp = wt();
    let completer = Spendthrift::new(1_000_000, 0);
    let driver = ApiAgentDriver::new(completer.clone(), PRICED_MODEL, tmp.path());
    let outcome = driver.run(&role(), "go").await.unwrap();
    assert_eq!(
        completer.calls(),
        MAX_ITERATIONS,
        "only the turn cap bounds it"
    );
    assert!(
        outcome.result.contains("iteration cap"),
        "{}",
        outcome.result
    );
    assert!(outcome.cost_usd.unwrap() > 100.0, "{:?}", outcome.cost_usd);
}

#[tokio::test]
async fn a_limit_on_an_unpriced_model_refuses_before_the_first_request() {
    let tmp = wt();
    let completer = Spendthrift::new(1_000, 0);
    let driver = ApiAgentDriver::new(completer.clone(), "vendor/unpriced-model", tmp.path())
        .with_spend_limit(1.0);
    let outcome = driver.run(&role(), "go").await.unwrap();
    assert_eq!(completer.calls(), 0, "nothing was sent");
    assert!(
        outcome.result.starts_with("INCOMPLETE"),
        "{}",
        outcome.result
    );
    assert!(outcome.result.contains("no price"), "{}", outcome.result);
}

#[tokio::test]
async fn a_turn_whose_cost_cannot_be_known_stops_a_limited_run() {
    let tmp = wt();
    // The reply carries no token counts, so what it cost is unknown.
    struct NoUsage(AtomicUsize);
    #[async_trait::async_trait]
    impl LlmPort for NoUsage {
        async fn complete(&self, _req: LlmRequest) -> anyhow::Result<LlmResponse> {
            self.0.fetch_add(1, Ordering::SeqCst);
            let text = json!({"message": {"role": "assistant", "content": null, "tool_calls": [{
                "id": "c", "type": "function", "function": {"name": "LS", "arguments": "{}"}
            }]}})
            .to_string();
            Ok(LlmResponse {
                reported_model: None,
                cli_cost_usd: None,
                text,
                model: PRICED_MODEL.to_string(),
                backend: "stub".to_string(),
                cost_usd: None,
                input_tokens: None,
                output_tokens: None,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
                or_cache_discount: None,
                truncated: false,
            })
        }
        async fn complete_streaming(
            &self,
            req: LlmRequest,
            _d: &mut (dyn for<'a> FnMut(&'a str) + Send),
        ) -> anyhow::Result<LlmResponse> {
            self.complete(req).await
        }
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }
    let completer = Arc::new(NoUsage(AtomicUsize::new(0)));
    let driver =
        ApiAgentDriver::new(completer.clone(), PRICED_MODEL, tmp.path()).with_spend_limit(1.0);
    let outcome = driver.run(&role(), "go").await.unwrap();
    assert_eq!(
        completer.0.load(Ordering::SeqCst),
        1,
        "stopped after the unknowable turn"
    );
    assert!(
        outcome.result.contains("could not be counted"),
        "{}",
        outcome.result
    );
}

#[tokio::test]
async fn a_child_sharing_the_meter_spends_against_the_parents_limit() {
    let tmp = wt();
    let parent_completer = Spendthrift::new(1_000, 0);
    let parent = ApiAgentDriver::new(parent_completer.clone(), PRICED_MODEL, tmp.path())
        .with_spend_limit(1.0);
    // A child built from the parent's controls spends $10 through the same meter.
    let controls = DriverControls {
        spend: Some(parent.spend_meter()),
        ..DriverControls::default()
    };
    let child_completer = Spendthrift::new(1_000_000, 0);
    let child = apply_controls(
        ApiAgentDriver::new(child_completer.clone(), PRICED_MODEL, tmp.path()),
        &child_controls_from(&controls),
    );
    let _ = child.run(&role(), "child work").await.unwrap();
    assert!(parent.spend_meter().spent_usd() >= 10.0);

    let outcome = parent.run(&role(), "parent work").await.unwrap();
    assert_eq!(
        parent_completer.calls(),
        0,
        "the parent's limit was already spent by its child"
    );
    assert!(
        outcome.result.starts_with("INCOMPLETE"),
        "{}",
        outcome.result
    );
}

#[test]
fn a_limit_must_be_a_positive_dollar_amount() {
    for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        assert!(SpendMeter::with_limit(bad).is_err(), "{bad}");
    }
    assert!(SpendMeter::with_limit(0.01).is_ok());
}
