#![allow(clippy::unwrap_used)]
//! `model_prompt_redaction` is switched off by the operator's ENVIRONMENT only, and the CLI says so
//! on stderr when it is. A `.env` file in the working directory (a client repo, say) is not the
//! operator's environment: it can neither switch redaction off nor stay silent about it.

use std::path::Path;
use std::process::{Command, Output};

const VAR: &str = "CAMERATA_FEATURE_MODEL_PROMPT_REDACTION";
const WARNING: &str = "model-prompt redaction is OFF";

/// Run a command that exits immediately (`release-gate` on a path that does not exist) with
/// `cwd` as the working directory and `env` as the operator's environment for the variable.
fn run_in(cwd: &Path, env: Option<&str>) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_camerata"));
    cmd.current_dir(cwd)
        .arg("release-gate")
        .arg(cwd.join("missing.zip"))
        .env(camerata_server::credentials::NO_KEYCHAIN_ENV, "1")
        .env_remove(VAR);
    if let Some(value) = env {
        cmd.env(VAR, value);
    }
    cmd.output().unwrap()
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn the_cli_warns_on_stderr_when_the_operator_turns_redaction_off() {
    let dir = tempfile::tempdir().unwrap();
    let stderr = stderr_of(&run_in(dir.path(), Some("false")));
    assert!(stderr.contains(WARNING), "{stderr}");
    assert!(stderr.contains(VAR), "{stderr}");
}

#[test]
fn the_cli_is_silent_when_redaction_is_on() {
    let dir = tempfile::tempdir().unwrap();
    let stderr = stderr_of(&run_in(dir.path(), None));
    assert!(!stderr.contains(WARNING), "{stderr}");
}

#[test]
fn a_dot_env_in_the_working_directory_cannot_turn_redaction_off() {
    let client_repo = tempfile::tempdir().unwrap();
    std::fs::write(client_repo.path().join(".env"), format!("{VAR}=false\n")).unwrap();
    let stderr = stderr_of(&run_in(client_repo.path(), None));
    assert!(
        !stderr.contains(WARNING),
        "a client repo's .env switched redaction off: {stderr}"
    );
}
