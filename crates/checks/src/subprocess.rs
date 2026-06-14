//! Subprocess invocation layer.
//!
//! Each function is a thin async wrapper around `tokio::process::Command` that
//! returns the combined stdout+stderr text. The caller (the parse layer) is
//! responsible for interpreting the output.
//!
//! `cargo fmt --check` exits non-zero when files would be reformatted; that is
//! not a spawn error, so we capture it and hand the text back to the caller.
//! `cargo clippy` follows the same pattern: non-zero exit means lints fired.

use std::path::Path;
use tokio::process::Command;

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
