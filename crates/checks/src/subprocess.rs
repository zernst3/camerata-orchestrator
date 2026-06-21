//! Subprocess invocation layer.
//!
//! Each function is a thin async wrapper around `tokio::process::Command` that
//! returns the combined stdout+stderr text. The caller (the parse layer) is
//! responsible for interpreting the output.
//!
//! `cargo fmt --check` exits non-zero when files would be reformatted; that is
//! not a spawn error, so we capture it and hand the text back to the caller.
//! `cargo clippy` follows the same pattern: non-zero exit means lints fired.
//! `cargo test` follows it too: non-zero exit means a test failed (or the crate
//! did not compile).

use std::path::Path;
use tokio::process::Command;

/// Raw output from an arbitrary check command.
///
/// The multi-language runners share this shape: they shell out to a tool, then
/// the per-language runner interprets `success` + `combined`. `tool_missing` is
/// the load-bearing distinction for the honesty stance (see crate docs): a tool
/// that could not be spawned at all is NOT a clean result, it is "could not
/// verify", which the runner surfaces as an `Err` so the coordinator fails
/// closed instead of falsely reporting a clean worktree.
pub struct CommandOutput {
    /// Combined stdout + stderr text.
    pub combined: String,
    /// True when the command exited 0.
    pub success: bool,
}

/// Run `program args...` in `worktree`, returning combined output + success.
///
/// A non-zero exit is NOT an error here (it is the normal "lint/test failed"
/// signal the caller maps to a RuleId). A failure to SPAWN the program (e.g.
/// the binary is not on PATH) IS an error: it propagates as `std::io::Error`,
/// which the per-language runner turns into an `Err` rather than a false clean.
pub async fn run_command(
    worktree: &Path,
    program: &str,
    args: &[&str],
) -> std::io::Result<CommandOutput> {
    let out = Command::new(program)
        .args(args)
        .current_dir(worktree)
        .output()
        .await?;

    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let combined = format!("{stdout}\n{stderr}");

    Ok(CommandOutput {
        combined,
        success: out.status.success(),
    })
}

/// Raw output from `cargo fmt --check`.
///
/// Contains stdout + stderr concatenated with a newline separator so the parse
/// layer has the full picture regardless of where rustfmt writes its messages.
pub struct FmtOutput {
    /// Combined stdout + stderr text.
    pub combined: String,
    /// True when `cargo fmt --check` exits 0 (everything is formatted).
    pub success: bool,
}

/// Raw output from `cargo clippy`.
pub struct ClippyOutput {
    pub combined: String,
    pub success: bool,
}

/// Raw output from `cargo test`.
pub struct TestOutput {
    pub combined: String,
    /// True when `cargo test` exits 0 (the crate compiled and every test passed).
    pub success: bool,
}

/// Run `cargo fmt --check` in `worktree` and return the raw output.
pub async fn run_fmt_check(worktree: &Path) -> std::io::Result<FmtOutput> {
    let out = Command::new("cargo")
        .args(["fmt", "--check"])
        .current_dir(worktree)
        .output()
        .await?;

    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let combined = format!("{stdout}\n{stderr}");

    Ok(FmtOutput {
        combined,
        success: out.status.success(),
    })
}

/// Run `cargo clippy -- -D warnings` in `worktree` and return the raw output.
pub async fn run_clippy(worktree: &Path) -> std::io::Result<ClippyOutput> {
    let out = Command::new("cargo")
        .args(["clippy", "--", "-D", "warnings"])
        .current_dir(worktree)
        .output()
        .await?;

    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let combined = format!("{stdout}\n{stderr}");

    Ok(ClippyOutput {
        combined,
        success: out.status.success(),
    })
}

/// Run `cargo test` in `worktree` and return the raw output.
///
/// `--no-fail-fast` so the agent gets the full set of failures to fix in one
/// revision pass rather than discovering them one at a time across several
/// bounce-backs. A non-zero exit means a test failed or the crate did not
/// compile; either way the layer-2 gate should bounce the work back.
pub async fn run_test(worktree: &Path) -> std::io::Result<TestOutput> {
    let out = Command::new("cargo")
        .args(["test", "--no-fail-fast"])
        .current_dir(worktree)
        .output()
        .await?;

    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let combined = format!("{stdout}\n{stderr}");

    Ok(TestOutput {
        combined,
        success: out.status.success(),
    })
}
