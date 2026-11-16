//! The agentic API loop talks to the provider directly (it sends tool schemas, which the bare
//! `LlmPort` cannot carry), so it does not pass the transports in `camerata-llm`. It passes the
//! same chokepoint on its own: the system prompt and every message of the conversation (user
//! turns, assistant turns with their tool calls, tool results) are redacted before a body is made.
//! Secret values are built at runtime (`camerata_secrets::fixtures`).

use camerata_secrets::fixtures::planted;
use camerata_secrets::SecretShape;
use serde_json::{json, Value};

use super::{build_anthropic_request_body, build_openrouter_agent_body};

/// A conversation in which a secret appears in a user turn, in the arguments of a tool call the
/// assistant made, and in a tool result.
fn conversation() -> (Vec<Value>, [camerata_secrets::fixtures::Planted; 3]) {
    let user = planted(SecretShape::StripeLiveSecretKey);
    let call = planted(SecretShape::SupabaseSecretKey);
    let result = planted(SecretShape::GitHubToken);
    let messages = vec![
        json!({ "role": "user", "content": format!("Rotate this: {}", user.text) }),
        json!({ "role": "assistant", "content": [
            { "type": "text", "text": "Reading the file." },
            { "type": "tool_use", "id": "toolu_1", "name": "read_file",
              "input": { "path": ".env", "note": call.text } }
        ]}),
        json!({ "role": "user", "content": [
            { "type": "tool_result", "tool_use_id": "toolu_1", "content": result.text }
        ]}),
    ];
    (messages, [user, call, result])
}

fn assert_none_of(what: &str, wire: &str, secrets: &[camerata_secrets::fixtures::Planted]) {
    for p in secrets {
        assert!(
            !wire.contains(&p.value),
            "{what}: a {:?} value reached the wire",
            p.shape
        );
    }
}

#[test]
fn the_anthropic_agent_body_carries_no_raw_secret_in_any_turn() {
    let (messages, secrets) = conversation();
    let system = planted(SecretShape::AnthropicKey);
    let (body, _) = build_anthropic_request_body(
        "claude-opus-5",
        Some(&format!("You are governed. {}", system.text)),
        &messages,
        &[],
    );
    let wire = body.to_string();
    assert_none_of("anthropic agent body", &wire, &secrets);
    assert!(!wire.contains(&system.value), "the system prompt leaked");
    assert!(wire.contains("toolu_1"), "ids and structure are untouched");
    assert!(wire.contains("Reading the file."));
}

#[test]
fn the_openrouter_agent_body_carries_no_raw_secret_in_any_turn() {
    let (messages, secrets) = conversation();
    let system = planted(SecretShape::AnthropicKey);
    let body = build_openrouter_agent_body(
        "some/model",
        Some(&format!("You are governed. {}", system.text)),
        &messages,
        &[json!({ "type": "function", "function": { "name": "read_file" } })],
        "session",
        None,
    );
    let wire = body.to_string();
    assert_none_of("openrouter agent body", &wire, &secrets);
    assert!(!wire.contains(&system.value), "the system prompt leaked");
    assert_eq!(body["tool_choice"], "auto", "the tools are still sent");
}

#[test]
fn a_conversation_with_no_secret_makes_the_same_body_as_before() {
    let messages = vec![
        json!({ "role": "user", "content": "Add a test for the parser." }),
        json!({ "role": "assistant", "content": "Done." }),
        json!({ "role": "user", "content": "Now the lexer." }),
    ];
    let (body, _) =
        build_anthropic_request_body("claude-opus-5", Some("You are governed."), &messages, &[]);
    assert_eq!(body["messages"], json!(messages));
}

// ---- a thinking block is never sent back after an edit to the conversation ----
//
// Fable 5.2 answers a request that returns a thinking block after an edit to `system`, `tools` or
// an earlier message with HTTP 400 ("The block is bound to a different conversation"), and a
// retry of the same body does not clear it. The loop replays each assistant turn exactly as the
// model wrote it, and the redaction above rewrites a secret the model put in that turn; the
// rewritten turn can no longer carry its thinking block.

fn thinking_turn(tool_note: &str) -> Value {
    json!({ "role": "assistant", "content": [
        { "type": "thinking", "thinking": "plan the read", "signature": "sig-abc" },
        { "type": "redacted_thinking", "data": "opaque" },
        { "type": "text", "text": "Reading the file." },
        { "type": "tool_use", "id": "toolu_1", "name": "read_file",
          "input": { "path": ".env", "note": tool_note } }
    ]})
}

fn tool_result() -> Value {
    json!({ "role": "user", "content": [
        { "type": "tool_result", "tool_use_id": "toolu_1", "content": "ok" }
    ]})
}

fn block_types(message: &Value) -> Vec<&str> {
    message["content"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["type"].as_str().unwrap())
        .collect()
}

#[test]
fn a_turn_redaction_rewrites_goes_back_without_its_thinking_blocks() {
    let leaked = planted(SecretShape::SupabaseSecretKey);
    let messages = vec![
        json!({ "role": "user", "content": "Read the file." }),
        thinking_turn(&leaked.text),
        tool_result(),
    ];
    let (body, _) = build_anthropic_request_body(
        "claude-fable-5-2",
        Some("You are governed."),
        &messages,
        &[],
    );
    assert!(!body.to_string().contains(&leaked.value), "still redacted");
    assert_eq!(
        block_types(&body["messages"][1]),
        ["text", "tool_use"],
        "the rewritten turn keeps its text and tool call and drops the thinking blocks"
    );
    assert_eq!(body["messages"][1]["content"][1]["id"], "toolu_1");
}

#[test]
fn every_turn_after_an_edited_one_drops_its_thinking_blocks_too() {
    let leaked = planted(SecretShape::GitHubToken);
    let messages = vec![
        json!({ "role": "user", "content": "Read the file." }),
        thinking_turn(&leaked.text),
        tool_result(),
        thinking_turn("clean"),
        tool_result(),
    ];
    let (body, _) = build_anthropic_request_body("claude-fable-5-2", None, &messages, &[]);
    assert_eq!(block_types(&body["messages"][1]), ["text", "tool_use"]);
    assert_eq!(block_types(&body["messages"][3]), ["text", "tool_use"]);
}

#[test]
fn a_turn_before_the_edit_keeps_its_thinking_blocks() {
    let leaked = planted(SecretShape::GitHubToken);
    let messages = vec![
        json!({ "role": "user", "content": "Read the file." }),
        thinking_turn("clean"),
        tool_result(),
        thinking_turn(&leaked.text),
        tool_result(),
    ];
    let (body, _) = build_anthropic_request_body("claude-fable-5-2", None, &messages, &[]);
    assert_eq!(
        block_types(&body["messages"][1]),
        ["thinking", "redacted_thinking", "text", "tool_use"]
    );
    assert_eq!(block_types(&body["messages"][3]), ["text", "tool_use"]);
}

#[test]
fn an_unedited_conversation_sends_its_thinking_blocks_back_untouched() {
    let messages = vec![
        json!({ "role": "user", "content": "Read the file." }),
        thinking_turn("clean"),
        tool_result(),
    ];
    let (body, _) = build_anthropic_request_body("claude-fable-5-2", None, &messages, &[]);
    assert_eq!(body["messages"], json!(messages));
}
