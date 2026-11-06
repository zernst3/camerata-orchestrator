//! Nothing that leaves for a model carries a raw secret: every wire builder redacts the system
//! prompt and the prompt through [`crate::model_input::guarded`], the prompt-cache breakpoints
//! follow the redaction, and a request that held no secret goes out byte for byte as it was.
//! Secret values are built at runtime (`camerata_secrets::fixtures`); this file holds none.

use std::sync::Arc;

use camerata_secrets::fixtures::{planted, Planted};
use camerata_secrets::SecretShape;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::model_input::{guarded, guarded_with};
use crate::sse::fixtures::{finish, start, text};

fn stripe() -> Planted {
    planted(SecretShape::StripeLiveSecretKey)
}

fn supabase() -> Planted {
    planted(SecretShape::SupabaseSecretKey)
}

/// A request whose system prompt, prompt and chat history-style context each carry a secret.
fn leaky() -> (LlmRequest, Planted, Planted) {
    let (a, b) = (stripe(), supabase());
    let req = LlmRequest::new(format!(
        "Project context:\nfinding detail mentions {}\nUser asks: why is this held?",
        a.text
    ))
    .with_system(format!("You are the assistant. Env dump: {}", b.text))
    .with_model("claude-sonnet-5-5");
    (req, a, b)
}

fn assert_clean(what: &str, wire: &str, secrets: &[&Planted]) {
    for p in secrets {
        assert!(
            !wire.contains(&p.value),
            "{what}: a {:?} value reached the wire",
            p.shape
        );
    }
}

#[test]
fn the_messages_body_carries_no_raw_secret_in_the_system_prompt_or_the_prompt() {
    let (req, a, b) = leaky();
    let body = messages_request_body(&req, "claude-sonnet-5-5", CacheTtl::FiveMinutes);
    assert_clean("messages body", &body.to_string(), &[&a, &b]);
    let sent = body["messages"][0]["content"]
        .as_str()
        .expect("plain prompt");
    assert!(sent.contains("User asks: why is this held?"), "{sent}");
}

#[test]
fn the_batch_item_the_token_count_and_the_openrouter_body_carry_no_raw_secret() {
    let (req, a, b) = leaky();
    let item = serde_json::to_string(&build_batch_item(
        "c0",
        &req,
        "claude-sonnet-5-5",
        BATCH_CACHE_TTL,
    ))
    .unwrap();
    assert_clean("batch item", &item, &[&a, &b]);
    assert_clean(
        "count_tokens body",
        &count_tokens_body(&req, "claude-sonnet-5-5").to_string(),
        &[&a, &b],
    );
    assert_clean(
        "openrouter body",
        &build_openrouter_chat_body("some/model", &req, "session", None).to_string(),
        &[&a, &b],
    );
}

#[test]
fn a_batch_submitted_with_hand_built_items_is_redacted_at_the_upload() {
    let (a, b) = (stripe(), supabase());
    let item = BatchItem {
        custom_id: "c0".to_string(),
        params: BatchItemParams {
            model: "claude-sonnet-5-5".to_string(),
            max_tokens: 100,
            system: Some(b.text.clone()),
            messages: vec![BatchMessage {
                role: "user".to_string(),
                content: json!([{ "type": "text", "text": a.text }]),
            }],
            output_config: None,
            temperature: None,
        },
    };
    let body = batch_submit_body(&[item]);
    assert_clean("batch upload", &body.to_string(), &[&a, &b]);
}

#[test]
fn the_claude_cli_argv_carries_no_raw_secret() {
    let (req, a, b) = leaky();
    let cmd = build_cli_command(
        &req,
        "claude-sonnet-5-5",
        "11111111-2222-4333-8444-555555555555",
    );
    let argv: Vec<String> = cmd
        .as_std()
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    assert_clean("cli argv", &argv.join("\u{1}"), &[&a, &b]);
    assert!(
        argv.iter()
            .any(|arg| arg.contains("User asks: why is this held?")),
        "the question itself still goes: {argv:?}"
    );
}

#[tokio::test]
async fn a_streamed_api_call_puts_no_raw_secret_on_the_wire() {
    let server = MockServer::start().await;
    let reply = format!(
        "{}{}{}",
        start(10),
        text("{\"ok\":true}"),
        finish("end_turn", 5)
    );
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(reply.into_bytes(), "text/event-stream"),
        )
        .mount(&server)
        .await;
    let llm = Llm::for_backend(Backend::Api, Some("sk-test".to_string()))
        .with_messages_url(format!("{}/v1/messages", server.uri()))
        .with_ledger(Arc::new(crate::usage_ledger::UsageLedger::new()));

    let (req, a, b) = leaky();
    llm.complete(req).await.expect("the call completes");

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let wire = String::from_utf8_lossy(&requests[0].body).into_owned();
    assert_clean("recorded transport", &wire, &[&a, &b]);
}

/// The cache breakpoints are byte offsets into the prompt. The prompt gets shorter or longer when
/// a value is replaced, so each breakpoint moves with the text it marked the end of: the segments
/// the model sees are the same segments, redacted.
#[test]
fn cache_breakpoints_still_mark_the_end_of_the_same_segments_after_redaction() {
    let a = stripe();
    let map = "REPO MAP\nsrc/a.rs\nsrc/b.rs\n";
    let digest = format!("DIGEST\nconst k = {};\nlet x = 1;\n", a.text);
    let tail = "ASK: review the digest above.";
    let prompt = format!("{map}{digest}{tail}");
    let req = LlmRequest::new(prompt)
        .with_model("claude-sonnet-5-5")
        .with_cache_breakpoints([map.len(), map.len() + digest.len()]);

    let body = messages_request_body(&req, "claude-sonnet-5-5", CacheTtl::FiveMinutes);
    let blocks = body["messages"][0]["content"].as_array().expect("blocks");
    assert_eq!(blocks.len(), 3, "{body}");
    assert_eq!(blocks[0]["text"], map);
    let middle = blocks[1]["text"].as_str().unwrap();
    assert!(middle.starts_with("DIGEST\n"), "{middle}");
    assert!(middle.ends_with("let x = 1;\n"), "{middle}");
    assert!(!middle.contains(&a.value), "{middle}");
    assert_eq!(blocks[2]["text"], tail);
    assert!(blocks[0].get("cache_control").is_some());
    assert!(blocks[1].get("cache_control").is_some());
    assert!(blocks[2].get("cache_control").is_none());
}

/// SALVAGE CRITICAL. A prompt that is already redacted (every inspection prompt is: its files went
/// through `files_for_model`) passes the guard as the same request, so the body and the digest the
/// run store files a stored reply under do not change, and every saved run still replays.
#[test]
fn an_already_redacted_request_passes_the_guard_unchanged() {
    let a = stripe();
    let redacted = camerata_secrets::redact_str(&a.text).into_owned();
    assert_ne!(redacted, a.text, "the fixture is a real secret");
    let req = LlmRequest::new(format!("REPO MAP\nFILE\n{redacted}\nASK"))
        .with_system(format!("system {redacted}"))
        .with_model("claude-sonnet-5-5")
        .with_max_tokens(4096)
        .with_cache_breakpoints([9, 14]);

    let out = guarded(&req);
    assert!(
        matches!(out, std::borrow::Cow::Borrowed(_)),
        "an already redacted request is not copied"
    );
    assert_eq!(out.prompt, req.prompt);
    assert_eq!(out.system, req.system);
    assert_eq!(out.cache_breakpoints, req.cache_breakpoints);
    assert_eq!(
        messages_request_body(&req, "claude-sonnet-5-5", CacheTtl::FiveMinutes),
        messages_request_body(&out, "claude-sonnet-5-5", CacheTtl::FiveMinutes)
    );
}

#[test]
fn guarding_twice_is_the_same_as_guarding_once() {
    let (req, _, _) = leaky();
    let once = guarded(&req).into_owned();
    let twice = guarded(&once);
    assert!(matches!(twice, std::borrow::Cow::Borrowed(_)));
    assert_eq!(twice.prompt, once.prompt);
    assert_eq!(twice.system, once.system);
}

#[test]
fn with_the_switch_off_the_request_goes_as_it_is() {
    let (req, a, _) = leaky();
    let out = guarded_with(&req, false);
    assert!(out.prompt.contains(&a.value));
}
