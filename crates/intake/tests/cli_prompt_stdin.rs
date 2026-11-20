//! The lead engineer and the refinement reviewer hand their prompt to the `claude` CLI on stdin,
//! not in the argument list. The OS caps the arguments of one process (macOS `ARG_MAX`, about
//! 1 MB); a prompt that carries a large grounding block failed the spawn with "Argument list too
//! long (os error 7)". A stub `claude` (named by `CAMERATA_CLAUDE_PROGRAM`) records what it was
//! started with and what it read.

use camerata_intake::{ClaudeLeadEngineer, IntakeForm, LeadEngineer, LeadEngineerError};
use std::os::unix::fs::PermissionsExt;

fn two_megabyte_grounding() -> String {
    let mut g = String::from("FIRST-LINE-OF-THE-GROUNDING\n");
    while g.len() < 2 * 1024 * 1024 {
        g.push_str("a line of repository digest that stands in for the client's source\n");
    }
    g.push_str("LAST-LINE-OF-THE-GROUNDING\n");
    g
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_large_planning_prompt_reaches_the_cli_on_stdin() {
    // Removed when it drops, a failed assertion included.
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().to_path_buf();
    let stub = dir.join("claude");
    std::fs::write(
        &stub,
        r#"#!/bin/sh
dir="$(dirname "$0")"
printf '%s\n' "$@" > "$dir/args.txt"
cat > "$dir/stdin.txt"
printf '%s\n' '{"result":"not a plan"}'
"#,
    )
    .expect("write stub");
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).expect("chmod stub");
    let _program = camerata_test_env::EnvGuard::set("CAMERATA_CLAUDE_PROGRAM", &stub);

    let grounding = two_megabyte_grounding();
    let outcome = ClaudeLeadEngineer::new()
        .with_grounding(grounding.clone())
        .evaluate(&IntakeForm::sample_app())
        .await;
    // The stub does not answer with a plan; what matters is that the spawn itself succeeded.
    assert!(
        !matches!(outcome, Err(LeadEngineerError::Spawn(_))),
        "a large prompt must not fail the spawn: {outcome:?}"
    );

    let received = std::fs::read_to_string(dir.join("stdin.txt")).expect("stub saw stdin");
    assert!(received.len() > 2 * 1024 * 1024, "the whole prompt arrived");
    assert!(received.contains("FIRST-LINE-OF-THE-GROUNDING"));
    assert!(received.contains("LAST-LINE-OF-THE-GROUNDING"));

    let args = std::fs::read_to_string(dir.join("args.txt")).expect("stub saw args");
    assert!(args.len() < 4096, "argv stays small: {} bytes", args.len());
    assert!(!args.contains("GROUNDING"));
    let lines: Vec<&str> = args.lines().collect();
    assert_eq!(lines.first(), Some(&"-p"), "{lines:?}");
    for flag in [
        "--model",
        "--allowedTools",
        "--output-format",
        "--setting-sources",
    ] {
        assert!(lines.contains(&flag), "{flag} is unchanged: {lines:?}");
    }
}
