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
