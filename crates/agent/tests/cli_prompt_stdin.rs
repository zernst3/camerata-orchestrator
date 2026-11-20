//! The governed agent hands its task to the `claude` CLI on stdin, not in the argument list.
//! The OS caps the arguments of one process (macOS `ARG_MAX`, about 1 MB); a task that carries
//! whole documents failed the spawn with "Argument list too long (os error 7)". A stub `claude`
//! (named by `CAMERATA_CLAUDE_PROGRAM`) records what it was started with and what it read.

use camerata_agent::ClaudeCliDriver;
use camerata_core::{AgentDriver, Role, RuleId};
use std::os::unix::fs::PermissionsExt;

fn role() -> Role {
    Role {
        name: "Backend".to_string(),
        rule_subset: vec![RuleId("GOV-1".to_string())],
        allowed_paths: vec!["crates/".to_string()],
    }
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_task_larger_than_the_os_argument_limit_reaches_the_agent_cli_on_stdin() {
    let dir = tempfile::tempdir().expect("temp dir");
    let stub = dir.path().join("claude");
    std::fs::write(
        &stub,
        r#"#!/bin/sh
dir="$(dirname "$0")"
printf '%s\n' "$@" > "$dir/args.txt"
cat > "$dir/stdin.txt"
printf '%s\n' '{"session_id":"s1","result":"DONE","total_cost_usd":0.01,"permission_denials":[]}'
"#,
    )
    .expect("write stub");
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).expect("chmod stub");
    let _program = camerata_test_env::EnvGuard::set("CAMERATA_CLAUDE_PROGRAM", &stub);

    let mut task = String::from("FIRST-LINE-OF-THE-TASK\n");
    while task.len() < 2 * 1024 * 1024 {
        task.push_str("a line of documentation that stands in for the technical manual\n");
    }
    task.push_str("LAST-LINE-OF-THE-TASK\n");

    let outcome = ClaudeCliDriver::new("/tmp/mcp.json")
        .run(&role(), &task)
        .await
        .expect("a 2 MB task must not fail the spawn");
    assert_eq!(outcome.result, "DONE");

    let received = std::fs::read_to_string(dir.path().join("stdin.txt")).expect("stub saw stdin");
    assert_eq!(received, task, "the whole task arrived on stdin, unchanged");

    let args = std::fs::read_to_string(dir.path().join("args.txt")).expect("stub saw args");
    assert!(args.len() < 4096, "argv stays small: {} bytes", args.len());
    assert!(!args.contains("FIRST-LINE-OF-THE-TASK"));
    let lines: Vec<&str> = args.lines().collect();
    assert_eq!(lines.first(), Some(&"-p"), "{lines:?}");
    for flag in [
        "--strict-mcp-config",
        "--mcp-config",
        "--allowedTools",
        "--output-format",
    ] {
        assert!(lines.contains(&flag), "{flag} is unchanged: {lines:?}");
    }
}
