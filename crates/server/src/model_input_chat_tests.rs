//! The operator chat assistant is model input like every inspection prompt: whatever the cockpit
//! put in its context (the scan results, the draft, an earlier answer) is redacted before it
//! reaches a model, on any backend. The handler runs against a fake `claude` on PATH that records
//! the argv it was given (the recorded transport); nothing reaches the network.
//! Secret values are built at runtime (`camerata_secrets::fixtures`).

use super::*;
use axum::extract::State;
use camerata_secrets::fixtures::planted;
use camerata_secrets::SecretShape;

/// Writes every argument it receives, one per line, to `$FAKE_ARGS_OUT`, and answers.
const RECORDING_CLAUDE: &str = r#"#!/bin/sh
[ -n "$CAMERATA_FAKE_WARM" ] && exit 0
printf '%s\n' "$@" > "$FAKE_ARGS_OUT"
printf '%s\n' '{"type":"result","is_error":false,"result":"OK","total_cost_usd":0.01,"usage":{"input_tokens":1,"output_tokens":1}}'
"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial(process_env)]
async fn the_chat_assistants_context_reaches_the_transport_redacted() {
    let root = tempfile::tempdir().unwrap();
    let bin = root.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        let claude = bin.join("claude");
        std::fs::write(&claude, RECORDING_CLAUDE).unwrap();
        std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
        let _ = std::process::Command::new(&claude)
            .env("CAMERATA_FAKE_WARM", "1")
            .status();
    }
    let out = root.path().join("argv.txt");
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let _path = camerata_test_env::EnvGuard::set("PATH", &path);
    let _out = camerata_test_env::EnvGuard::set("FAKE_ARGS_OUT", &out);
    let _on = camerata_test_env::EnvGuard::remove(crate::feature_flags::MODEL_PROMPT_REDACTION_ENV);

    // A secret in each place the assistant's context is made of: the scan results section the
    // cockpit renders into the system prompt, an earlier assistant answer, and the new question.
    let in_context = planted(SecretShape::StripeLiveSecretKey);
    let in_history = planted(SecretShape::SupabaseSecretKey);
    let in_question = planted(SecretShape::GitHubToken);
    let finding = crate::onboard::Finding {
        severity: "high".to_string(),
        rule_id: "SEC-NO-VENDOR-TOKEN-1".to_string(),
        repo: "acme/api".to_string(),
        path: "src/billing.rs".to_string(),
        line: 12,
        status: "active".to_string(),
        detail: format!("The deploy script prints {}", in_context.text),
        ..crate::onboard::Finding::default()
    };
    let scan_results = render_scan_results_for_chat(&[finding], &[]);
    let system = format!("You are the project assistant.\n\nScan results:\n{scan_results}");

    let state = AppState::new(Arc::new(InMemoryStoryStore::new()));
    let req = ChatReq {
        prompt: format!("Is this the same key? {}", in_question.text),
        model: "claude-sonnet-5-5".to_string(),
        system: Some(system),
        history: vec![
            ChatTurn {
                role: "user".to_string(),
                content: "Why is the billing finding held?".to_string(),
            },
            ChatTurn {
                role: "assistant".to_string(),
                content: format!("It quotes {}", in_history.text),
            },
        ],
    };
    let reply = chat(State(state), axum::Json(req)).await;
    assert!(reply.is_ok(), "the fake claude answers");

    let argv = std::fs::read_to_string(&out).expect("the transport recorded what it was sent");
    for p in [&in_context, &in_history, &in_question] {
        assert!(
            !argv.contains(&p.value),
            "a {:?} value reached the transport",
            p.shape
        );
    }
    assert!(
        argv.contains("The deploy script prints")
            && argv.contains("Why is the billing finding held?"),
        "the context itself still goes: {argv}"
    );
}
