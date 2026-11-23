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
            elapsed_ms: None,
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
                elapsed_ms: None,
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

/// Like [`Spendthrift`], but each reply takes a moment, so concurrent drivers overlap.
struct SlowSpendthrift {
    inner: Arc<Spendthrift>,
}

#[async_trait::async_trait]
impl LlmPort for SlowSpendthrift {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        self.inner.complete(req).await
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

#[tokio::test]
async fn concurrent_children_cannot_each_pass_the_limit_by_one_request() {
    let tmp = wt();
    // A request could cost up to about $0.41 (the 8,192-token output cap at $50 per million), so
    // a $0.20 limit admits only the very first request of the run.
    let meter = SpendMeter::with_limit(0.20).unwrap();
    let completer = Spendthrift::new(1_000, 0);
    let slow = Arc::new(SlowSpendthrift {
        inner: completer.clone(),
    });
    let mut runs = Vec::new();
    for i in 0..6 {
        let driver = ApiAgentDriver::new(slow.clone(), PRICED_MODEL, tmp.path())
            .with_spend_meter(meter.clone());
        runs.push(async move { driver.run(&role(), &format!("child {i}")).await.unwrap() });
    }
    let outcomes = futures::future::join_all(runs).await;

    assert_eq!(
        completer.calls(),
        1,
        "six children against a limit that admits one request sent more than one in total"
    );
    let refused = outcomes
        .iter()
        .filter(|o| o.result.starts_with("INCOMPLETE") && o.result.contains("spend limit"))
        .count();
    assert_eq!(refused, 6, "every child ends on the limit");
}

#[tokio::test]
async fn a_reservation_is_released_when_the_turn_ends() {
    let tmp = wt();
    // A roomy limit: two children one after the other both get through, so a finished
    // request does not keep its reservation.
    let meter = SpendMeter::with_limit(100.0).unwrap();
    for _ in 0..2 {
        let completer = Spendthrift::new(1_000, 0);
        let driver = ApiAgentDriver::new(completer.clone(), PRICED_MODEL, tmp.path())
            .with_spend_meter(meter.clone());
        let _ = driver.run(&role(), "go").await.unwrap();
        assert!(completer.calls() >= 2, "{}", completer.calls());
    }
}

/// Answers every request with a timeout, as a bounded HTTP client does when the provider stalls.
struct TimesOut;

#[async_trait::async_trait]
impl LlmPort for TimesOut {
    async fn complete(&self, _req: LlmRequest) -> anyhow::Result<LlmResponse> {
        Err(anyhow::anyhow!(
            "OpenRouter API request failed: error sending request: operation timed out"
        ))
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

/// A call that timed out may still have been billed, and nothing came back to price it from. It
/// is recorded at its cost cap (the request ceiling), so the limit counts it.
#[tokio::test]
async fn a_call_that_times_out_is_recorded_at_its_cost_cap() {
    let tmp = wt();
    let driver =
        ApiAgentDriver::new(Arc::new(TimesOut), PRICED_MODEL, tmp.path()).with_spend_limit(5.0);

    let err = driver
        .run(&role(), "go")
        .await
        .expect_err("the call failed");
    assert!(format!("{err:#}").contains("timed out"), "{err:#}");

    let turns = driver.spend_meter().turns();
    assert_eq!(
        turns.len(),
        1,
        "the timed-out call is on the record: {turns:?}"
    );
    let cost = turns[0]
        .cost_usd
        .expect("recorded at a figure, not unknown");
    // The 8,192-token output cap at $50 per million is $0.4096; the prompt and tool schemas add a few cents.
    assert!(
        (0.4096..0.5).contains(&cost),
        "recorded at the request ceiling: {cost}"
    );
    assert!((driver.spend_meter().spent_usd() - cost).abs() < 1e-9);
}

/// A failure that is not a timeout (a 400, a refused connection) was not billed: nothing recorded.
#[tokio::test]
async fn a_call_that_fails_without_a_timeout_records_no_spend() {
    struct Refused;
    #[async_trait::async_trait]
    impl LlmPort for Refused {
        async fn complete(&self, _req: LlmRequest) -> anyhow::Result<LlmResponse> {
            Err(anyhow::anyhow!("OpenRouter API HTTP 400: bad request"))
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
    let tmp = wt();
    let driver =
        ApiAgentDriver::new(Arc::new(Refused), PRICED_MODEL, tmp.path()).with_spend_limit(5.0);
    driver
        .run(&role(), "go")
        .await
        .expect_err("the call failed");
    assert!(driver.spend_meter().turns().is_empty());
}

/// A local listener that accepts connections and, after optionally writing `reply`, never
/// answers further. Returns its address; the accepted sockets are held open until the test ends.
async fn stalled_listener(reply: &'static [u8]) -> std::net::SocketAddr {
    use tokio::io::AsyncWriteExt;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((mut sock, _)) = listener.accept().await {
            if !reply.is_empty() {
                // Read the request first: a reply written before it is an unexpected message.
                let mut buf = [0u8; 4096];
                let _ = tokio::io::AsyncReadExt::read(&mut sock, &mut buf).await;
                let _ = sock.write_all(reply).await;
                let _ = sock.flush().await;
            }
            held.push(sock);
        }
    });
    addr
}

fn short_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(400))
        .build()
        .unwrap()
}

/// The REAL error of a stalled provider, wrapped exactly as the posts wrap it, reads as a
/// timeout (reqwest's `Display` omits the source, so only the typed error in the chain carries it).
#[tokio::test]
async fn a_real_request_timeout_is_recognised_through_the_post_wrapping() {
    let addr = stalled_listener(b"").await;
    let builder = short_client().post(format!("http://{addr}/stalled"));
    let err = send_and_read(builder, "Anthropic API")
        .await
        .expect_err("the provider never answers");
    assert!(is_timeout_error(&err), "{err:#}");
}

/// Headers arrive, then the body stalls: the read-phase timeout must also read as a timeout,
/// not as an empty body that fails to parse.
#[tokio::test]
async fn a_real_body_phase_timeout_is_recognised() {
    let addr = stalled_listener(
        b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 500\r\n\r\n{\"a\":",
    )
    .await;
    let builder = short_client().post(format!("http://{addr}/stalled"));
    let err = send_and_read(builder, "OpenRouter API")
        .await
        .expect_err("the body never completes");
    assert!(is_timeout_error(&err), "{err:#}");
}
