//! The prompt reaches the `claude` CLI on stdin, never in the argument list. The OS caps the
//! arguments of one process (macOS `ARG_MAX`, about 1 MB), and a chat prompt that carries whole
//! documents is larger, so as an argument it failed the spawn with "Argument list too long".
//! These run the real `Llm::complete` against a stub `claude` on a temporary PATH.

use super::*;

/// A scratch directory holding an executable `claude` stub, removed on drop.
struct StubClaude {
    dir: std::path::PathBuf,
}

impl StubClaude {
    fn new(tag: &str, script: &str) -> Self {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!(
            "camerata-llm-stdin-claude-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create stub dir");
        let bin = dir.join("claude");
        let mut f = std::fs::File::create(&bin).expect("create stub");
        f.write_all(script.as_bytes()).expect("write stub");
        drop(f);
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755))
            .expect("chmod stub");
        Self { dir }
    }
}

impl Drop for StubClaude {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn cli_llm() -> Llm {
    Llm {
        vendor: Vendor::Anthropic,
        backend: Backend::Cli,
        api_key: None,
        ledger: None,
        sessions: None,
        sleeper: None,
        messages_url: None,
    }
}

/// A stub that saves its argument list and its whole stdin, then answers with a result record.
const RECORDING_STUB: &str = r#"#!/bin/sh
dir="$(dirname "$0")"
printf '%s\n' "$@" > "$dir/args.txt"
cat > "$dir/stdin.txt"
printf '%s\n' '{"type":"result","is_error":false,"result":"OK","total_cost_usd":0.01,"usage":{"input_tokens":1,"output_tokens":1}}'
"#;

fn patched_path(dir: &std::path::Path) -> camerata_test_env::EnvGuard {
    let joined = match std::env::var("PATH") {
        Ok(p) => format!("{}:{p}", dir.display()),
        Err(_) => dir.display().to_string(),
    };
    camerata_test_env::EnvGuard::set("PATH", joined)
}

/// A prompt of about 2 MB, twice the macOS argument limit, whose first and last lines are
/// distinct so a cut at either end shows.
fn two_megabyte_prompt() -> String {
    let mut prompt = String::from("FIRST-LINE-OF-THE-PROMPT\n");
    let filler = "a line of documentation that stands in for the technical manual\n";
    while prompt.len() < 2 * 1024 * 1024 {
        prompt.push_str(filler);
    }
    prompt.push_str("LAST-LINE-OF-THE-PROMPT\n");
    prompt
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_prompt_larger_than_the_os_argument_limit_reaches_the_cli_on_stdin() {
    let stub = StubClaude::new("big", RECORDING_STUB);
    let _path = patched_path(&stub.dir);
    let prompt = two_megabyte_prompt();

    let resp = cli_llm()
        .complete(LlmRequest::new(prompt.clone()).with_model("claude-sonnet-5-5"))
        .await
        .expect("a 2 MB prompt must not fail the spawn");
    assert_eq!(resp.text, "OK");

    let received = std::fs::read_to_string(stub.dir.join("stdin.txt")).expect("stub saw stdin");
    assert_eq!(received.len(), prompt.len(), "the whole prompt arrived");
    assert_eq!(received, prompt, "the prompt arrived unchanged");

    let args = std::fs::read_to_string(stub.dir.join("args.txt")).expect("stub saw args");
    assert!(
        args.len() < 4096,
        "the argument list stays small, it was {} bytes",
        args.len()
    );
    assert!(!args.contains("FIRST-LINE-OF-THE-PROMPT"));
    let lines: Vec<&str> = args.lines().collect();
    assert_eq!(lines.first(), Some(&"-p"), "the print flag leads: {lines:?}");
    assert!(
        lines.contains(&"--model") && lines.contains(&"claude-sonnet-5-5"),
        "the other flags are unchanged: {lines:?}"
    );
}

#[test]
fn the_command_line_carries_no_prompt_and_the_prompt_is_handed_back_for_stdin() {
    let (cmd, prompt) = build_cli_command(
        &LlmRequest::new("a prompt carrying client source"),
        "claude-haiku-4-5",
        "sid",
    );
    let args: Vec<String> = cmd
        .as_std()
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert!(
        !args.iter().any(|a| a.contains("client source")),
        "the prompt must not ride in the argument list: {args:?}"
    );
    assert_eq!(prompt, "a prompt carrying client source");
}

#[test]
fn a_spawn_failure_that_is_not_not_found_names_the_real_os_error_and_does_not_blame_path() {
    // E2BIG on macOS and Linux is errno 7: "Argument list too long".
    let e2big = std::io::Error::from_raw_os_error(7);
    let os_text = e2big.to_string();
    let msg = map_spawn_error(e2big).to_string();
    assert!(msg.contains(&os_text), "the OS error is named: {msg}");
    assert!(
        !msg.contains("installed") && !msg.contains("on PATH"),
        "a spawn failure that is not not-found must not blame the install or PATH: {msg}"
    );
}
