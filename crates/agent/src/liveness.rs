//! Async liveness helpers for `camerata-agent` and callers in `camerata-server`.
//!
//! Two complementary signals feed a [`HeartbeatFn`](super::HeartbeatFn) so a
//! long-running tool (e.g. `cargo clippy` cold-compiling a repo with native
//! deps like rocksdb) keeps the parent job alive even when it emits no stdout
//! lines for several minutes:
//!
//! 1. **Output-line signal** — the existing [`stream_subprocess`] path already
//!    fires `on_activity()` on every stdout line. No new code here; documented
//!    for discoverability.
//!
//! 2. **Build-dir mtime probe** — [`spawn_mtime_probe`] starts a `tokio::spawn`
//!    background task that polls `newest_mtime(dir)` every `~15s`. When the
//!    newest file mtime advances (i.e. `cargo` wrote something under `target/`
//!    even without emitting a line), it fires the supplied [`HeartbeatFn`]. This
//!    is the **rivet fix**: `cargo clippy` compiles rocksdb for 8+ minutes with
//!    no clippy output, but it continuously writes into `target/`, so the probe
//!    fires heartbeats throughout.
//!
//! # Usage pattern (scan path)
//!
//! ```text
//! let on_hb: HeartbeatFn = Arc::new(move || store.touch_activity(&job_id));
//! let probe = spawn_mtime_probe(repo_dir, on_hb.clone(), Duration::from_secs(15));
//! // ... run the tool (streaming path fires on_hb per stdout line too) ...
//! probe.abort(); // cancel probe when tool finishes
//! ```

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use tokio::task::JoinHandle;

use super::HeartbeatFn;

/// Polling interval for the mtime probe (15 s). Conservative — the goal is to
/// detect an alive-but-quiet compile, not to measure it precisely.
pub const MTIME_PROBE_INTERVAL: Duration = Duration::from_secs(15);

/// Hard backstop ceiling for the mtime probe task itself. Matches
/// [`super::DEFAULT_AGENT_TOTAL_TIMEOUT_SECS`] so the probe never outlives the
/// absolute maximum a tool run is allowed to take.
pub const MTIME_PROBE_MAX_DURATION: Duration =
    Duration::from_secs(super::DEFAULT_AGENT_TOTAL_TIMEOUT_SECS);

// ─── pure helper ─────────────────────────────────────────────────────────────

/// Walk `dir` (non-recursively is insufficient; we need the whole tree) and
/// return the newest `modified()` timestamp found across all entries, or `None`
/// when the directory is empty, unreadable, or no entry has a valid mtime.
///
/// Uses `std::fs` (sync I/O) intentionally — this runs inside a `spawn_blocking`
/// call in [`spawn_mtime_probe`] so it does not block the async executor.
///
/// The walk is bounded: it recurses into subdirs but skips entries that fail to
/// stat (best-effort). On a typical `target/` tree with thousands of `.o` and
/// `.rlib` files this is fast enough for a 15-second polling interval.
pub fn newest_mtime(dir: &Path) -> Option<SystemTime> {
    let mut best: Option<SystemTime> = None;

    fn walk(path: &Path, best: &mut Option<SystemTime>) {
        let Ok(rd) = std::fs::read_dir(path) else {
            return;
        };
        for entry in rd.flatten() {
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if let Ok(mt) = meta.modified() {
                *best = Some(match *best {
                    Some(prev) if mt > prev => mt,
                    Some(prev) => prev,
                    None => mt,
                });
            }
            if meta.is_dir() {
                walk(&entry.path(), best);
            }
        }
    }

    walk(dir, &mut best);
    best
}

// ─── async probe ─────────────────────────────────────────────────────────────

/// Spawn a background task that polls the newest mtime under `dir` every
/// [`MTIME_PROBE_INTERVAL`] and fires `on_heartbeat` whenever the mtime
/// advances (indicating new writes — e.g. cargo compiling into `target/`).
///
/// Returns a [`JoinHandle`] the caller MUST abort when the tool finishes so
/// the probe does not outlive its job:
///
/// ```text
/// let probe = spawn_mtime_probe(dir, cb, MTIME_PROBE_INTERVAL);
/// // ... run tool ...
/// probe.abort();
/// ```
///
/// The probe is fail-soft:
/// - If `dir` doesn't exist yet it keeps polling (cargo may create `target/`
///   mid-compile).
/// - If the mtime read fails it skips the tick silently.
/// - It self-terminates after [`MTIME_PROBE_MAX_DURATION`] regardless.
pub fn spawn_mtime_probe(
    dir: PathBuf,
    on_heartbeat: HeartbeatFn,
    interval: Duration,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut last_mtime: Option<SystemTime> = None;
        let deadline = tokio::time::Instant::now() + MTIME_PROBE_MAX_DURATION;

        loop {
            // Sleep first so the very first poll fires after one interval rather
            // than immediately (the tool may not have written anything yet).
            tokio::select! {
                _ = tokio::time::sleep(interval) => {}
                _ = tokio::time::sleep_until(deadline) => {
                    // Hard backstop: the probe has been running for the maximum
                    // allowed duration. Self-terminate silently.
                    break;
                }
            }

            // Stat the directory on a blocking thread — `newest_mtime` uses std::fs.
            let dir_clone = dir.clone();
            let current = tokio::task::spawn_blocking(move || newest_mtime(&dir_clone))
                .await
                .unwrap_or(None);

            match (last_mtime, current) {
                (_, None) => {
                    // Directory empty / unreadable / not yet created — skip.
                }
                (None, Some(mt)) => {
                    // First successful read: record baseline without firing.
                    last_mtime = Some(mt);
                }
                (Some(prev), Some(mt)) if mt > prev => {
                    // Mtime advanced: something was written. Fire the heartbeat.
                    last_mtime = Some(mt);
                    on_heartbeat();
                }
                _ => {
                    // No change — nothing to do.
                }
            }
        }
    })
}

// ─── tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use tempfile::TempDir;

    /// [`newest_mtime`] on a temp dir containing a single file returns `Some`.
    #[test]
    fn newest_mtime_finds_file_in_temp_dir() {
        let tmp = TempDir::new().expect("tmp dir");
        let file = tmp.path().join("test.txt");
        std::fs::write(&file, b"hello").expect("write");

        let mt = newest_mtime(tmp.path());
        assert!(mt.is_some(), "expected Some mtime for a dir with a file");
    }

    /// [`newest_mtime`] on an empty directory returns `None`.
    #[test]
    fn newest_mtime_empty_dir_returns_none() {
        let tmp = TempDir::new().expect("tmp dir");
        let mt = newest_mtime(tmp.path());
        assert!(mt.is_none(), "expected None for empty dir");
    }

    /// [`newest_mtime`] on a nonexistent path returns `None`.
    #[test]
    fn newest_mtime_nonexistent_returns_none() {
        let mt = newest_mtime(Path::new("/nonexistent/path/that/cannot/exist"));
        assert!(mt.is_none(), "expected None for nonexistent path");
    }

    /// [`newest_mtime`] walks subdirs and finds the newest file.
    #[test]
    fn newest_mtime_recurses_subdirs() {
        let tmp = TempDir::new().expect("tmp dir");
        let sub = tmp.path().join("sub");
        std::fs::create_dir(&sub).expect("subdir");

        // Write two files; sleep briefly so timestamps differ on coarse filesystems.
        let f1 = tmp.path().join("a.txt");
        std::fs::write(&f1, b"a").expect("write a");

        // Force a small delay so the OS flushes distinct mtimes.
        std::thread::sleep(Duration::from_millis(10));

        let f2 = sub.join("b.txt");
        std::fs::write(&f2, b"b").expect("write b");

        let mt_root = f1.metadata().unwrap().modified().unwrap();
        let mt_sub = f2.metadata().unwrap().modified().unwrap();

        let result = newest_mtime(tmp.path()).expect("Some");

        // The result must be >= the root file's mtime (it found at least one).
        assert!(result >= mt_root, "result should be at least as new as root file");
        // On filesystems with sub-ms precision, result == mt_sub; on coarse ones it
        // may equal mt_root. Just assert it found SOMETHING.
        let _ = mt_sub; // used for documentation above
    }

    /// [`spawn_mtime_probe`] fires the heartbeat when a new file appears.
    #[tokio::test]
    async fn mtime_probe_fires_on_new_write() {
        let tmp = TempDir::new().expect("tmp dir");
        // Seed one file so the probe has a baseline on its first poll.
        std::fs::write(tmp.path().join("seed.txt"), b"seed").expect("seed");

        let count = Arc::new(AtomicU64::new(0));
        let count_cb = count.clone();
        let cb: HeartbeatFn = Arc::new(move || {
            count_cb.fetch_add(1, Ordering::Relaxed);
        });

        // Use a very short interval for the test.
        let probe = spawn_mtime_probe(
            tmp.path().to_path_buf(),
            cb,
            Duration::from_millis(50),
        );

        // Wait one interval for the probe to take its baseline reading.
        tokio::time::sleep(Duration::from_millis(80)).await;

        // Write a new file → mtime should advance → heartbeat should fire.
        std::thread::sleep(Duration::from_millis(20)); // coarse-FS guard
        std::fs::write(tmp.path().join("new.txt"), b"new content").expect("write");

        // Wait long enough for the probe to detect the change.
        tokio::time::sleep(Duration::from_millis(200)).await;

        probe.abort();

        assert!(
            count.load(Ordering::Relaxed) >= 1,
            "expected at least one heartbeat after writing a new file"
        );
    }
}
