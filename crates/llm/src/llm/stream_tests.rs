//! The API leg streams (`stream: true`, server-sent events): the idle timeout governs instead of a
//! fixed total timeout, text that arrived before a late failure is salvaged, and a reply cut off
//! at its cap comes back as one (and is recorded once). Every test runs against a local mock
//! server; nothing reaches the network.

use std::sync::{Arc, Mutex};

use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::sse::fixtures::{event, finish, start, text, thinking};

/// Records the waits a retry asks for and returns at once.
#[derive(Default)]
struct Waits(Mutex<Vec<u64>>);

impl crate::retry::Sleeper for Waits {
    fn sleep<'a>(
        &'a self,
        d: std::time::Duration,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        self.0.lock().unwrap().push(d.as_secs());
        Box::pin(async {})
    }
}

fn sse(body: String) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_raw(body.into_bytes(), "text/event-stream")
}

async fn server_with(responses: Vec<ResponseTemplate>) -> MockServer {
    let server = MockServer::start().await;
    for r in responses {
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(r)
            .up_to_n_times(1)
            .mount(&server)
            .await;
    }
    server
}

fn llm_at(server: &MockServer, waits: &Arc<Waits>) -> (Llm, Arc<crate::usage_ledger::UsageLedger>) {
    let ledger = Arc::new(crate::usage_ledger::UsageLedger::new());
    let llm = Llm::for_backend(Backend::Api, Some("sk-test".to_string()))
        .with_messages_url(format!("{}/v1/messages", server.uri()))
        .with_sleeper(waits.clone())
        .with_ledger(ledger.clone());
    (llm, ledger)
}

fn request(max_tokens: u32) -> LlmRequest {
    LlmRequest::new("scan this")
        .with_model("claude-sonnet-5-5")
        .with_max_tokens(max_tokens)
}

/// The request is a stream request, its text arrives as it is generated, and the folded reply
/// carries the usage of the whole call (thinking included) and its price.
#[tokio::test]
async fn a_streamed_reply_is_requested_forwarded_in_order_and_billed_for_its_thinking() {
    let body = format!(
        "{}{}{}{}{}",
        start(1_000),
        thinking("working it out"),
        text("{\"findings\":"),
        text("[]}"),
        finish("end_turn", 2_500),
    );
    let server = server_with(vec![sse(body)]).await;
    let waits = Arc::new(Waits::default());
    let (llm, ledger) = llm_at(&server, &waits);

    let mut seen: Vec<String> = Vec::new();
    let resp = llm
        .complete_streaming(request(8_000), &mut |t: &str| seen.push(t.to_string()))
        .await
        .expect("the stream completes");

    assert_eq!(seen, vec!["{\"findings\":", "[]}"], "deltas, as they came");
    assert_eq!(resp.text, "{\"findings\":[]}");
    assert!(!resp.truncated);
    assert_eq!(
        resp.output_tokens,
        Some(2_500),
        "the thinking is billed output"
    );
    assert_eq!(resp.input_tokens, Some(1_000 + 7 + 3));
    // $2 / $10 per million: 1,000 input + 2,500 output (the cache split adds a little).
    assert!(resp.cost_usd.unwrap() > 0.025);
    assert_eq!(ledger.snapshot().calls, 1);

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let sent: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(sent["stream"], true, "the API leg streams: {sent}");
    assert_eq!(sent["max_tokens"], 8_000);
}

/// A streamed reply that stops at its cap is a reply cut off at the limit: its complete entries
/// are salvageable, it is flagged `truncated`, and the call is recorded exactly once.
#[tokio::test]
async fn a_streamed_reply_cut_at_its_cap_is_salvaged_and_recorded_once() {
    let body = format!(
        "{}{}{}",
        start(500),
        text("{\"findings\":[{\"title\":\"a\"},{\"title\":\"b\"},{\"tit"),
        finish("max_tokens", 8_000),
    );
    let server = server_with(vec![sse(body)]).await;
    let waits = Arc::new(Waits::default());
    let (llm, ledger) = llm_at(&server, &waits);

    let resp = llm
        .complete_streaming(request(8_000), &mut |_| {})
        .await
        .expect("a cut-off reply is still a reply");

    assert!(resp.truncated, "stop_reason max_tokens");
    assert_eq!(resp.output_tokens, Some(8_000));
    // The complete entries inside the cut-off text can be read one by one.
    let objects = complete_array_objects_of(&resp.text);
    assert_eq!(objects, 2, "{}", resp.text);
    assert_eq!(ledger.snapshot().calls, 1, "recorded once");
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        1,
        "sent once"
    );
    assert!(waits.0.lock().unwrap().is_empty(), "no retry, no wait");
}

/// How many complete `{...}` objects the findings array of `text` holds.
fn complete_array_objects_of(text: &str) -> usize {
    let start = text.find('[').unwrap_or(0);
    let mut depth = 0;
    let mut n = 0;
    for ch in text[start + 1..].chars() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    n += 1;
                }
            }
            _ => {}
        }
    }
    n
}

/// A stream that dies AFTER delivering text (the body closes with no stop reason, as a dropped
/// connection does) keeps what arrived: it is returned as a reply cut off at the limit, billed at
/// the request's cap because the real output is unknown, and it is not retried.
#[tokio::test]
async fn a_stream_that_dies_late_keeps_the_text_that_arrived() {
    let body = format!(
        "{}{}",
        start(300),
        text("{\"findings\":[{\"title\":\"a\"},")
    );
    let server = server_with(vec![sse(body)]).await;
    let waits = Arc::new(Waits::default());
    let (llm, ledger) = llm_at(&server, &waits);

    let resp = llm
        .complete_streaming(request(9_600), &mut |_| {})
        .await
        .expect("the text that arrived is salvaged");

    assert!(resp.truncated);
    assert_eq!(resp.text, "{\"findings\":[{\"title\":\"a\"},");
    assert_eq!(
        resp.output_tokens,
        Some(9_600),
        "unknown output is recorded at the cap: a spend limit never under-counts"
    );
    assert_eq!(ledger.snapshot().calls, 1);
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        1,
        "no retry"
    );
}

/// A stream that breaks before any text arrived is a transport failure: retried on the standard
/// schedule, and the retry's reply is the one returned.
#[tokio::test]
async fn a_stream_that_breaks_before_any_text_is_retried() {
    let broken = format!("{}", start(300));
    let good = format!(
        "{}{}{}",
        start(300),
        text("{\"findings\":[]}"),
        finish("end_turn", 20)
    );
    let server = server_with(vec![sse(broken), sse(good)]).await;
    let waits = Arc::new(Waits::default());
    let (llm, _) = llm_at(&server, &waits);

    let resp = llm
        .complete_streaming(request(4_096), &mut |_| {})
        .await
        .expect("the retry answers");

    assert_eq!(resp.text, "{\"findings\":[]}");
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
    assert_eq!(*waits.0.lock().unwrap(), vec![2]);
}

/// An overloaded answer before the stream begins is retried like any other status.
#[tokio::test]
async fn an_overloaded_status_before_the_stream_is_retried_on_the_longer_schedule() {
    let overloaded = ResponseTemplate::new(529).set_body_json(
        json!({"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}),
    );
    let good = format!("{}{}{}", start(10), text("{}"), finish("end_turn", 2));
    let server = server_with(vec![overloaded, sse(good)]).await;
    let waits = Arc::new(Waits::default());
    let (llm, _) = llm_at(&server, &waits);

    let resp = llm
        .complete_streaming(request(4_096), &mut |_| {})
        .await
        .expect("the second try answers");
    assert_eq!(resp.text, "{}");
    assert_eq!(*waits.0.lock().unwrap(), vec![5], "the overloaded schedule");
}

/// A terminal status is not a stream: it is formatted as the transport always formatted it.
#[tokio::test]
async fn a_bad_request_is_a_plain_http_failure_and_is_not_retried() {
    let bad = ResponseTemplate::new(400).set_body_json(
        json!({"type":"error","error":{"type":"invalid_request_error","message":"max_tokens: too large"}}),
    );
    let server = server_with(vec![bad]).await;
    let waits = Arc::new(Waits::default());
    let (llm, _) = llm_at(&server, &waits);

    let err = llm
        .complete_streaming(request(4_096), &mut |_| {})
        .await
        .expect_err("400 is terminal");
    assert!(err.to_string().contains("HTTP 400"), "{err}");
    assert!(err.to_string().contains("max_tokens: too large"), "{err}");
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

/// An error event with no text is a provider failure on the standard schedule.
#[tokio::test]
async fn an_error_event_with_no_text_is_a_retried_provider_failure() {
    let err = format!(
        "{}{}",
        start(10),
        event(
            "error",
            json!({"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}})
        )
    );
    let good = format!("{}{}{}", start(10), text("{}"), finish("end_turn", 2));
    let server = server_with(vec![sse(err), sse(good)]).await;
    let waits = Arc::new(Waits::default());
    let (llm, _) = llm_at(&server, &waits);
    let resp = llm
        .complete_streaming(request(4_096), &mut |_| {})
        .await
        .expect("the retry answers");
    assert_eq!(resp.text, "{}");
    assert_eq!(*waits.0.lock().unwrap(), vec![5]);
}

/// The `max_tokens` on the wire never exceeds what the model can write: a stage's runaway
/// ceiling is a constant, the model's limit is the registry's.
#[test]
fn max_tokens_is_clamped_to_what_the_model_can_write() {
    let big = LlmRequest::new("p").with_max_tokens(500_000);
    let body = messages_request_body(&big, "claude-sonnet-5-5", CacheTtl::FiveMinutes);
    assert_eq!(body["max_tokens"], 128_000);
    let item = build_batch_item("c0-b0", &big, "claude-sonnet-5-5", BATCH_CACHE_TTL);
    assert_eq!(item.params.max_tokens, 128_000);
    // A model listed without a limit keeps the request's own.
    let body = messages_request_body(&big, "claude-haiku-4-5-20251001", CacheTtl::FiveMinutes);
    assert_eq!(body["max_tokens"], 500_000);
}
