//! The system prompt reaches the `claude` CLI in a private file (`--system-prompt-file`), not in
//! the argument list. The chat's system prompt carries the whole manuals (about 900 KB), and as
//! an argument it failed the spawn with "Argument list too long (os error 7)". A stub `claude` on
//! a temporary PATH records what it was started with and what it read.

use super::*;
use camerata_secrets::fixtures::planted;
use camerata_secrets::SecretShape;

/// Saves its argument list and stdin, and, when given a system prompt file, that file's text, its
/// path and the modes of the file and its directory; then answers (or exits with `$FAKE_EXIT`).
const STUB: &str = r#"#!/bin/sh
dir="$(dirname "$0")"
printf '%s\n' "$@" > "$dir/args.txt"
cat > "$dir/stdin.txt"
prev=''
for a in "$@"; do
  if [ "$prev" = "--system-prompt-file" ]; then
    cp "$a" "$dir/system.txt"
    printf '%s\n' "$a" > "$dir/system-path.txt"
    mode() { stat -c %a "$1" 2>/dev/null || stat -f %Lp "$1"; }
    printf '%s %s\n' "$(mode "$a")" "$(mode "$(dirname "$a")")" > "$dir/system-modes.txt"
  fi
  prev="$a"
done
[ -n "$FAKE_EXIT" ] && { echo 'API Error: boom' >&2; exit "$FAKE_EXIT"; }
printf '%s\n' '{"type":"result","is_error":false,"result":"OK","total_cost_usd":0.01,"usage":{"input_tokens":1,"output_tokens":1}}'
"#;

struct Stub {
    dir: std::path::PathBuf,
}

/// Exits at once without reading its stdin, but leaves a background `sleep` holding the read end
/// of that pipe open (and its output redirected away so the call still sees the end of the
/// stream): the prompt writer is stuck on a full pipe after the child has exited.
const LINGERING_STUB: &str = r#"#!/bin/sh
sleep 8 <&0 >/dev/null 2>&1 &
printf '%s\n' '{"type":"result","is_error":false,"result":"OK","total_cost_usd":0.01,"usage":{"input_tokens":1,"output_tokens":1}}'
exit 0
"#;

impl Stub {
    fn new(tag: &str) -> Self {
        Self::with_script(tag, STUB)
    }

    fn with_script(tag: &str, script: &str) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!(
            "camerata-llm-system-claude-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create stub dir");
        let bin = dir.join("claude");
        std::fs::write(&bin, script).expect("write stub");
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        Self { dir }
    }
}

impl Drop for Stub {
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

fn patched_path(dir: &std::path::Path) -> camerata_test_env::EnvGuard {
    let joined = match std::env::var("PATH") {
        Ok(p) => format!("{}:{p}", dir.display()),
        Err(_) => dir.display().to_string(),
    };
    camerata_test_env::EnvGuard::set("PATH", joined)
}

fn read(stub: &Stub, name: &str) -> String {
    std::fs::read_to_string(stub.dir.join(name)).unwrap_or_else(|e| panic!("{name}: {e}"))
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_system_prompt_over_the_os_limit_arrives_intact_and_redacted_in_a_private_file() {
    let stub = Stub::new("big");
    let _path = patched_path(&stub.dir);
    let _state = camerata_test_env::EnvGuard::set("CAMERATA_STATE_DIR", stub.dir.join("state"));
    let secret = planted(SecretShape::StripeLiveSecretKey);
    let mut system = String::from("FIRST-LINE-OF-THE-SYSTEM-PROMPT\n");
    let filler = "a line of the technical manual that stands in for the real one\n";
    while system.len() < 1024 * 1024 {
        system.push_str(filler);
    }
    system.push_str(&format!("The config says {}\n", secret.text));
    system.push_str("LAST-LINE-OF-THE-SYSTEM-PROMPT\n");

    let resp = cli_llm()
        .complete(
            LlmRequest::new("the question")
                .with_system(system.clone())
                .with_model("claude-sonnet-5-5"),
        )
        .await
        .expect("a 1 MB system prompt must not fail the spawn");
    assert_eq!(resp.text, "OK");

    let args = read(&stub, "args.txt");
    assert!(args.len() < 4096, "argv stays small: {} bytes", args.len());
    assert!(!args.contains("FIRST-LINE-OF-THE-SYSTEM-PROMPT"));
    let lines: Vec<&str> = args.lines().collect();
    assert!(
        !lines.contains(&"--system-prompt"),
        "the system prompt is not an argument: {lines:?}"
    );
    assert!(lines.contains(&"--system-prompt-file"), "{lines:?}");

    let received = read(&stub, "system.txt");
    assert!(received.len() >= 1024 * 1024, "the whole text arrived");
    assert!(received.starts_with("FIRST-LINE-OF-THE-SYSTEM-PROMPT\n"));
    assert!(received.ends_with("LAST-LINE-OF-THE-SYSTEM-PROMPT\n"));
    assert!(
        !received.contains(&secret.value),
        "a secret value reached the system prompt file"
    );
    assert!(received.contains("The config says"), "the text itself goes");
    assert_eq!(read(&stub, "stdin.txt"), "the question");
    assert_eq!(read(&stub, "system-modes.txt").trim(), "600 700");

    let path = read(&stub, "system-path.txt");
    assert!(
        !std::path::Path::new(path.trim()).exists(),
        "the file is gone once the call is over: {path}"
    );
    // It lived in the engine's own folder under the state dir, not in the temp folder.
    assert!(
        path.trim()
            .starts_with(stub.dir.join("state").to_str().unwrap()),
        "{path}"
    );
    assert!(path.contains("tmp/system-prompts/"), "{path}");
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn the_system_prompt_file_is_removed_when_the_call_fails() {
    let stub = Stub::new("fail");
    let _path = patched_path(&stub.dir);
    let _state = camerata_test_env::EnvGuard::set("CAMERATA_STATE_DIR", stub.dir.join("state"));
    let _exit = camerata_test_env::EnvGuard::set("FAKE_EXIT", "1");

    let outcome = cli_llm()
        .complete(
            LlmRequest::new("q")
                .with_system("a system prompt")
                .with_model("claude-sonnet-5-5"),
        )
        .await;
    assert!(outcome.is_err(), "the stub exits non-zero");
    let path = read(&stub, "system-path.txt");
    assert!(
        !std::path::Path::new(path.trim()).exists(),
        "the file is gone after a failed call: {path}"
    );
}

#[test]
fn the_command_line_names_the_system_prompt_file_and_never_the_text() {
    let call = build_cli_command(
        &LlmRequest::new("q").with_system("SYSTEM-TEXT"),
        "claude-haiku-4-5",
        "sid",
    )
    .unwrap();
    let args: Vec<String> = call
        .cmd
        .as_std()
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let at = args
        .iter()
        .position(|a| a == "--system-prompt-file")
        .expect("the file flag is there");
    assert!(std::path::Path::new(&args[at + 1]).is_file());
    assert!(!args.iter().any(|a| a.contains("SYSTEM-TEXT")), "{args:?}");
    assert!(!args.iter().any(|a| a == "--system-prompt"));
}

/// The quit path runs no destructors: the shutdown hook removes the file of a call in flight.
#[test]
#[serial_test::serial(process_env)]
fn the_shutdown_hook_removes_the_system_prompt_file_of_a_call_in_flight() {
    let state = std::env::temp_dir().join(format!("camerata-llm-shutdown-{}", std::process::id()));
    let _state = camerata_test_env::EnvGuard::set("CAMERATA_STATE_DIR", &state);
    let call = build_cli_command(
        &LlmRequest::new("q").with_system("client project layers"),
        "claude-haiku-4-5",
        "sid",
    )
    .unwrap();
    let path = call.system_file.as_ref().unwrap().path().to_path_buf();
    assert!(path.is_file());
    kill_inflight_claude();
    assert!(!path.exists(), "removed by the shutdown hook");
    drop(call);
    let _ = std::fs::remove_dir_all(&state);
}

/// A child that exits without reading its stdin, with something else still holding the pipe open,
/// leaves the prompt writer stuck on a full pipe. The call must not wait on it for long, and it
/// fails: the child cannot have read the whole prompt, so its answer is not the answer.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_writer_stuck_after_the_child_exited_is_given_a_short_time_then_the_call_fails() {
    let stub = Stub::with_script("lingering", LINGERING_STUB);
    let _path = patched_path(&stub.dir);
    let _state = camerata_test_env::EnvGuard::set("CAMERATA_STATE_DIR", stub.dir.join("state"));
    // Well over the pipe buffer, so the write cannot complete into it.
    let prompt = "a line of prompt text that nobody reads\n".repeat(100_000);
    let started = std::time::Instant::now();
    let outcome = cli_llm()
        .complete(LlmRequest::new(prompt).with_model("claude-sonnet-5-5"))
        .await;
    let took = started.elapsed();
    let err = outcome.expect_err("the prompt was not fully written, so the call fails");
    assert!(
        err.to_string().contains("not fully written"),
        "the error says why: {err}"
    );
    assert!(
        took < std::time::Duration::from_secs(6),
        "the call waited on a stuck writer for {took:?}"
    );
}
