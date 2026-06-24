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
//!
//! # CARGO_TARGET_DIR — shared target directory (disk-safety, 2026-06-22)
//!
//! All three cargo commands accept an optional `target_dir: Option<&Path>` and, when
//! Some, set `CARGO_TARGET_DIR` to that path. The caller (the check runners in lib.rs)
//! derives this path from the worktree location:
//!
//! ```text
//! <clone>/.camerata-shared-target
//!   └─ (shared by all UoW worktrees under <clone>/.camerata-worktrees/<branch>)
//! ```
//!
//! Cargo file-locks `target/` during a build, so concurrent builds on the same repo
//! SERIALIZE at the lock — that is the accepted tradeoff (correctness over parallelism).
//! A comment in `workspace::ensure_uow_worktree` documents this.
//!
//! # Liveness / heartbeat (Phase 1b, 2026-06-24)
//!
//! All four commands now accept `on_progress: Option<&HeartbeatFn>`. With `Some(cb)`:
//! - stdout is read line-by-line via `AsyncBufReadExt`; `cb()` is fired on every line.
//! - An mtime probe is started against the cargo target directory for the duration of
//!   the run, firing `cb()` when the target dir is written (covers the cold-compile
//!   window where cargo writes objects but emits no lines).
//!
//! With `None`: falls back to `.output().await` (buffered, unchanged behaviour) —
//! backwards-compatible for all existing callers.

use std::path::Path;

use camerata_liveness::{spawn_mtime_probe, HeartbeatFn, MTIME_PROBE_INTERVAL};
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

// ─── internal streaming helper ────────────────────────────────────────────────

/// Run `program args...` in `worktree` with optional per-line heartbeat.
///
/// With `Some(cb)`: reads stdout line-by-line firing `cb()` per line; also starts an
/// mtime probe against `target_dir` (when `Some`) for the duration so a cold cargo
/// compile (no lines emitted during codegen/linking) still fires heartbeats via disk
/// writes. Stderr is piped but not streamed (it is appended to the combined output
/// after the child exits).
///
/// With `None`: falls back to `.output().await` (buffered) — unchanged behaviour.
async fn run_with_heartbeat(
    mut cmd: Command,
    worktree: &Path,
    target_dir: Option<&Path>,
    on_progress: Option<&HeartbeatFn>,
) -> std::io::Result<(String, bool)> {
    use tokio::io::{AsyncBufReadExt, BufReader};

    match on_progress {
        None => {
            let out = cmd.current_dir(worktree).output().await?;
            let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
            let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
            Ok((format!("{stdout}\n{stderr}"), out.status.success()))
        }
        Some(cb) => {
            cmd.current_dir(worktree)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped());

            let mut child = cmd.spawn()?;
            let stdout = child.stdout.take().expect("stdout is piped");
            let mut lines = BufReader::new(stdout).lines();
            let mut accumulated = String::new();

            // Start the mtime probe against the cargo target dir when present.
            // This fires heartbeats during cold compiles where cargo writes objects
            // but emits no stdout lines.
            let _mtime_probe = target_dir.map(|td| {
                spawn_mtime_probe(td.to_path_buf(), cb.clone(), MTIME_PROBE_INTERVAL)
            });

            while let Ok(Some(line)) = lines.next_line().await {
                cb();
                accumulated.push_str(&line);
                accumulated.push('\n');
            }

            // Collect stderr after stdout is drained.
            let stderr_text = if let Some(mut stderr) = child.stderr.take() {
                use tokio::io::AsyncReadExt;
                let mut buf = String::new();
                let _ = stderr.read_to_string(&mut buf).await;
                buf
            } else {
                String::new()
            };

            let status = child.wait().await?;

            // _mtime_probe is aborted when it goes out of scope here (JoinHandle drops = abort).
            accumulated.push('\n');
            accumulated.push_str(&stderr_text);

            Ok((accumulated, status.success()))
        }
    }
}

// ─── public API ──────────────────────────────────────────────────────────────

/// Run `program args...` in `worktree`, returning combined output + success.
///
/// A non-zero exit is NOT an error here (it is the normal "lint/test failed"
/// signal the caller maps to a RuleId). A failure to SPAWN the program (e.g.
/// the binary is not on PATH) IS an error: it propagates as `std::io::Error`,
/// which the per-language runner turns into an `Err` rather than a false clean.
///
/// `on_progress`: when `Some`, fires once per stdout line so the parent job stays
/// alive during a long-running check. Pass `None` for the buffered silent path.
pub async fn run_command(
    worktree: &Path,
    program: &str,
    args: &[&str],
    on_progress: Option<&HeartbeatFn>,
) -> std::io::Result<CommandOutput> {
    let mut cmd = Command::new(program);
    cmd.args(args);
    let (combined, success) = run_with_heartbeat(cmd, worktree, None, on_progress).await?;
    Ok(CommandOutput { combined, success })
}

/// Run `cargo fmt --check` in `worktree` and return the raw output.
///
/// `target_dir`: when `Some`, `CARGO_TARGET_DIR` is set to that path on the child
/// process so all worktrees for this repo share ONE artifact store rather than each
/// building their own `target/` directory. Pass the value from
/// `camerata_server::workspace::shared_target_dir(&clone)`.
///
/// `on_progress`: when `Some`, fires once per stdout line AND once per mtime advance
/// in the target dir (the cold-compile heartbeat). Pass `None` for buffered behaviour.
pub async fn run_fmt_check(
    worktree: &Path,
    target_dir: Option<&Path>,
    on_progress: Option<&HeartbeatFn>,
) -> std::io::Result<FmtOutput> {
    let mut cmd = Command::new("cargo");
    cmd.args(["fmt", "--check"]);
    if let Some(td) = target_dir {
        cmd.env("CARGO_TARGET_DIR", td);
    }
    let (combined, success) =
        run_with_heartbeat(cmd, worktree, target_dir, on_progress).await?;
    Ok(FmtOutput { combined, success })
}

/// Run `cargo clippy -- -D warnings` in `worktree` and return the raw output.
///
/// `target_dir`: when `Some`, sets `CARGO_TARGET_DIR` on the child process so this
/// invocation shares the repo's single artifact store. See `run_fmt_check` for the
/// full design note.
///
/// `on_progress`: when `Some`, fires once per stdout line AND once per mtime advance
/// in the target dir. Pass `None` for buffered behaviour.
pub async fn run_clippy(
    worktree: &Path,
    target_dir: Option<&Path>,
    on_progress: Option<&HeartbeatFn>,
) -> std::io::Result<ClippyOutput> {
    let mut cmd = Command::new("cargo");
    cmd.args(["clippy", "--", "-D", "warnings"]);
    if let Some(td) = target_dir {
        cmd.env("CARGO_TARGET_DIR", td);
    }
    let (combined, success) =
        run_with_heartbeat(cmd, worktree, target_dir, on_progress).await?;
    Ok(ClippyOutput { combined, success })
}

/// Run `cargo test` in `worktree` and return the raw output.
///
/// `--no-fail-fast` so the agent gets the full set of failures to fix in one
/// revision pass rather than discovering them one at a time across several
/// bounce-backs. A non-zero exit means a test failed or the crate did not
/// compile; either way the layer-2 gate should bounce the work back.
///
/// `target_dir`: when `Some`, sets `CARGO_TARGET_DIR` so builds use the shared
/// artifact store. See `run_fmt_check` for the full design note.
///
/// `on_progress`: when `Some`, fires once per stdout line AND once per mtime advance
/// in the target dir. Pass `None` for buffered behaviour.
pub async fn run_test(
    worktree: &Path,
    target_dir: Option<&Path>,
    on_progress: Option<&HeartbeatFn>,
) -> std::io::Result<TestOutput> {
    let mut cmd = Command::new("cargo");
    cmd.args(["test", "--no-fail-fast"]);
    if let Some(td) = target_dir {
        cmd.env("CARGO_TARGET_DIR", td);
    }
    let (combined, success) =
        run_with_heartbeat(cmd, worktree, target_dir, on_progress).await?;
    Ok(TestOutput { combined, success })
}

// ─── tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use tempfile::TempDir;

    /// The streaming path fires the heartbeat at least once per stdout line emitted.
    #[tokio::test]
    async fn streaming_path_fires_heartbeat_per_line() {
        let tmp = TempDir::new().expect("tmp dir");

        let count = Arc::new(AtomicU64::new(0));
        let count_cb = count.clone();
        let cb: HeartbeatFn = Arc::new(move || {
            count_cb.fetch_add(1, Ordering::Relaxed);
        });

        // Run a command that emits 3 lines on stdout.
        let result = run_command(
            tmp.path(),
            "sh",
            &["-c", "echo line1; echo line2; echo line3"],
            Some(&cb),
        )
        .await
        .expect("command should succeed");

        assert!(result.success, "sh echo should exit 0");
        assert!(result.combined.contains("line1"), "output should contain line1");
        assert!(result.combined.contains("line2"), "output should contain line2");
        assert!(result.combined.contains("line3"), "output should contain line3");
        assert!(
            count.load(Ordering::Relaxed) >= 3,
            "expected at least 3 heartbeat ticks (one per line), got {}",
            count.load(Ordering::Relaxed)
        );
    }

    /// The None path still works correctly (buffered, no heartbeat).
    #[tokio::test]
    async fn none_path_works_without_heartbeat() {
        let tmp = TempDir::new().expect("tmp dir");
        let result = run_command(
            tmp.path(),
            "sh",
            &["-c", "echo hello"],
            None,
        )
        .await
        .expect("command should succeed");
        assert!(result.success);
        assert!(result.combined.contains("hello"));
    }
}
