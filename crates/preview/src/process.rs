//! `PreviewServer`: spawn + supervise a `dx serve` subprocess for one app, folding its
//! stdout/stderr through [`crate::parser::parse_dx_line`] into a live [`PreviewStatus`].
//!
//! Mirrors the lifecycle pattern in `crates/ui/src/server_process.rs` (`ServerGuard`):
//! spawn, redirect stdout+stderr, SIGTERM-then-SIGKILL on Drop, plus a detached watchdog
//! shell loop so a `process::exit` (or a SIGKILL of the parent) doesn't leak the child --
//! confirmed safe for a `dx serve` process specifically in the spike's Q5. That module lives
//! in a binary crate (`camerata-ui`) and isn't reusable as a library dependency, so the
//! termination helpers are replicated here rather than shared.
//!
//! Difference from `server_process.rs`: that module only needs a health-endpoint POLL (the
//! BFF's own stdout/stderr are inherited straight to the console). Here the whole point is a
//! LIVE, continuously-updated status, so stdout/stderr are piped and tailed by a background
//! task for the process's entire lifetime, not just during startup.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{broadcast, watch};

use crate::parser::{parse_dx_line, PreviewEvent};
use crate::status::{fold, PreviewStatus};

/// Resolve the `dx` CLI binary to shell out to: `CAMERATA_DX_BIN` env override if set and
/// non-blank, else bare `dx` (resolved via `PATH` by the OS at spawn time, same as any other
/// `Command::new("dx")` call would do).
pub fn dx_bin() -> PathBuf {
    match std::env::var("CAMERATA_DX_BIN") {
        Ok(v) if !v.trim().is_empty() => PathBuf::from(v),
        _ => PathBuf::from("dx"),
    }
}

/// Everything needed to launch one `dx serve` preview instance.
#[derive(Debug, Clone)]
pub struct PreviewLaunchConfig {
    /// The Dioxus app's directory (contains `Dioxus.toml`/`Cargo.toml`); `dx serve` is
    /// spawned with this as its cwd.
    pub app_dir: PathBuf,
    /// The port to pass explicitly via `--port` (spike recommendation #1: don't rely on dx's
    /// default, so multiple previews can run concurrently).
    pub port: u16,
    /// The `dx` binary to spawn -- see [`dx_bin`].
    pub dx_bin: PathBuf,
    /// Whether to pass `--verbose` (spike recommendation #4: extra visibility into the
    /// silent-ignore DEBUG lines, even though dx doesn't treat them as build triggers either
    /// way -- the real mitigation is `verify::verify_after_edit`'s timeout+cargo-check
    /// fallback, not scraping these DEBUG lines, but the extra visibility is still useful for
    /// a human tailing the log).
    pub verbose: bool,
}

impl PreviewLaunchConfig {
    /// The production default for `app_dir`/`port`: resolved `dx` binary, `--verbose` on.
    pub fn new(app_dir: impl Into<PathBuf>, port: u16) -> Self {
        Self { app_dir: app_dir.into(), port, dx_bin: dx_bin(), verbose: true }
    }

    /// The URL this config's port maps to. `PreviewServer` uses THIS -- not a parsed
    /// [`PreviewEvent::Serving`] line -- as the source of truth for the preview URL, since the
    /// spike never captured a canonical "serving at <url>" log line to depend on (see
    /// `parser.rs`'s module docs).
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}/", self.port)
    }
}

/// A running (or just-exited) `dx serve` preview instance.
pub struct PreviewServer {
    launch: PreviewLaunchConfig,
    child: Mutex<Option<Child>>,
    pid: u32,
    status_rx: watch::Receiver<PreviewStatus>,
    // Kept alive so subscribe_events() can always hand out a working receiver even after the
    // reader tasks finish (a receiver created after every sender clone were dropped would
    // error immediately on the next recv()).
    events_tx: broadcast::Sender<PreviewEvent>,
}

impl PreviewServer {
    /// Spawn `dx serve --platform web --port <port> --open false --interactive false
    /// [--verbose]` in `launch.app_dir`, per the spike's Recommendation #1. Starts one
    /// background task per stdout/stderr pipe that parses each line and folds it into the
    /// shared status; the initial status is seeded to `Serving{url}` immediately (rather than
    /// waiting on a parsed event) since `PreviewServer` already knows its own URL from the
    /// port it chose -- see [`PreviewLaunchConfig::url`].
    pub fn spawn(launch: PreviewLaunchConfig) -> anyhow::Result<Self> {
        let mut command = Command::new(&launch.dx_bin);
        command
            .current_dir(&launch.app_dir)
            .arg("serve")
            .arg("--platform")
            .arg("web")
            .arg("--port")
            .arg(launch.port.to_string())
            .arg("--open")
            .arg("false")
            .arg("--interactive")
            .arg("false");
        if launch.verbose {
            command.arg("--verbose");
        }
        command
            .kill_on_drop(false) // Drop below owns termination explicitly (SIGTERM-first).
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        let mut child = command.spawn().map_err(|e| {
            anyhow::anyhow!(
                "could not spawn `{} serve` in {}: {e}\n\
                 (set CAMERATA_DX_BIN to the dx CLI binary, or install it: `cargo install dioxus-cli`)",
                launch.dx_bin.display(),
                launch.app_dir.display()
            )
        })?;
        let pid = child.id().ok_or_else(|| anyhow::anyhow!("dx serve child has no pid immediately after spawn"))?;

        let stdout = child.stdout.take().expect("stdout is piped");
        let stderr = child.stderr.take().expect("stderr is piped");

        let (status_tx, status_rx) = watch::channel(PreviewStatus::Serving { url: launch.url() });
        let (events_tx, _) = broadcast::channel(256);

        spawn_line_reader(stdout, status_tx.clone(), events_tx.clone());
        spawn_line_reader(stderr, status_tx, events_tx.clone());

        spawn_watchdog(pid);

        Ok(Self { launch, child: Mutex::new(Some(child)), pid, status_rx, events_tx })
    }

    /// The preview URL, constructed from the configured port (see
    /// [`PreviewLaunchConfig::url`]) -- always available, even before dx has logged anything.
    pub fn url(&self) -> String {
        self.launch.url()
    }

    /// The app directory this instance was launched against.
    pub fn app_dir(&self) -> &Path {
        &self.launch.app_dir
    }

    /// A snapshot of the current folded status.
    pub fn status(&self) -> PreviewStatus {
        self.status_rx.borrow().clone()
    }

    /// A live receiver for status changes (e.g. to drive a UI indicator).
    pub fn watch_status(&self) -> watch::Receiver<PreviewStatus> {
        self.status_rx.clone()
    }

    /// A live receiver for individual raw events -- what
    /// [`crate::verify::verify_after_edit`] watches for the decisive
    /// Hotreload/BuildOk/BuildFailed signal after a fleet-driven edit.
    pub fn subscribe_events(&self) -> broadcast::Receiver<PreviewEvent> {
        self.events_tx.subscribe()
    }

    /// The spawned child's OS pid.
    pub fn pid(&self) -> u32 {
        self.pid
    }
}

/// Spawn a background task that reads `pipe` line-by-line, classifies each line via
/// [`parse_dx_line`], publishes it on `events_tx` (best-effort -- a lagging/no-receiver
/// broadcast channel never blocks or panics the reader), and folds it into `status_tx`.
fn spawn_line_reader<R>(pipe: R, status_tx: watch::Sender<PreviewStatus>, events_tx: broadcast::Sender<PreviewEvent>)
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut lines = BufReader::new(pipe).lines();
        loop {
            match lines.next_line().await {
                Ok(Some(line)) => {
                    if let Some(event) = parse_dx_line(&line) {
                        status_tx.send_modify(|current| *current = fold(current, &event));
                        // No receivers is a normal, expected state (nobody's watching yet);
                        // ignore the SendError rather than treat it as failure.
                        let _ = events_tx.send(event);
                    }
                }
                Ok(None) => break, // EOF: the pipe closed (child exited or closed the fd).
                Err(_) => break,   // I/O error reading the pipe -- stop tailing.
            }
        }
    });
}

/// Start a detached `sh` watchdog that SIGTERMs the `dx serve` child if THIS process dies
/// while it's still running -- mirrors `crates/ui/src/server_process.rs::spawn_watchdog`
/// (see that module's docs for why Drop alone isn't sufficient: a host that exits via
/// `std::process::exit`, e.g. a Dioxus/tao event loop, skips Rust destructors entirely).
#[cfg(unix)]
fn spawn_watchdog(child_pid: u32) {
    let parent_pid = std::process::id();
    let script = format!(
        "while kill -0 {parent_pid} 2>/dev/null && kill -0 {child_pid} 2>/dev/null; do \
             sleep 0.5; \
         done; \
         kill -0 {parent_pid} 2>/dev/null || kill -TERM {child_pid} 2>/dev/null"
    );
    match std::process::Command::new("sh").arg("-c").arg(script).spawn() {
        Ok(_) => {}
        Err(e) => eprintln!(
            "[camerata-preview] could not start the dx-serve watchdog (pid {child_pid} may \
             outlive the host process if it exits via process::exit): {e}"
        ),
    }
}

#[cfg(not(unix))]
fn spawn_watchdog(_child_pid: u32) {}

impl Drop for PreviewServer {
    /// SIGTERM first (matches the spike's Q5 finding: plain SIGTERM was sufficient in every
    /// trial to kill `dx serve` AND its live `cargo`/`rustc` children, freeing the port within
    /// ~2s, no orphans), then poll briefly, then SIGKILL as an escalation if it didn't exit.
    fn drop(&mut self) {
        let Ok(mut guard) = self.child.lock() else { return };
        let Some(mut child) = guard.take() else { return };
        drop(guard);

        #[cfg(unix)]
        {
            let _ = std::process::Command::new("kill").arg("-TERM").arg(self.pid.to_string()).output();
            let deadline = Instant::now() + Duration::from_millis(2000);
            while Instant::now() < deadline {
                match child.try_wait() {
                    Ok(Some(_)) => return,
                    Ok(None) => std::thread::sleep(Duration::from_millis(50)),
                    Err(_) => break,
                }
            }
        }
        let _ = child.start_kill();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[serial_test::serial(camerata_dx_bin_env)]
    fn dx_bin_defaults_to_bare_dx_without_override() {
        std::env::remove_var("CAMERATA_DX_BIN");
        assert_eq!(dx_bin(), PathBuf::from("dx"));
    }

    #[test]
    #[serial_test::serial(camerata_dx_bin_env)]
    fn dx_bin_honors_env_override() {
        std::env::set_var("CAMERATA_DX_BIN", "/opt/homebrew/bin/dx");
        let resolved = dx_bin();
        std::env::remove_var("CAMERATA_DX_BIN");
        assert_eq!(resolved, PathBuf::from("/opt/homebrew/bin/dx"));
    }

    #[test]
    #[serial_test::serial(camerata_dx_bin_env)]
    fn dx_bin_ignores_a_blank_override() {
        std::env::set_var("CAMERATA_DX_BIN", "   ");
        let resolved = dx_bin();
        std::env::remove_var("CAMERATA_DX_BIN");
        assert_eq!(resolved, PathBuf::from("dx"));
    }

    #[test]
    fn launch_config_url_uses_the_configured_port() {
        let cfg = PreviewLaunchConfig::new("/tmp/some-app", 8123);
        assert_eq!(cfg.url(), "http://127.0.0.1:8123/");
    }

    /// Real-process integration smoke check: spawns an ACTUAL `dx serve` against a real
    /// Dioxus app directory (see `itinerary-app`, listed as a working directory for this
    /// session) and asserts a URL/pid come back and Drop cleans it up. Deliberately
    /// `#[ignore]`d -- too slow/flaky/environment-dependent for the normal `cargo test`
    /// suite (needs the `dx` CLI installed, a real Dioxus app on disk, network access for
    /// wasm-bindgen-cli on a cold machine, and 5-20s of real compile time per the spike).
    /// Run manually with `cargo test -p camerata-preview -- --ignored spawns_a_real_dx_serve`.
    #[tokio::test]
    #[ignore]
    async fn spawns_a_real_dx_serve_and_reports_a_url() {
        let app_dir = PathBuf::from("/Users/zacharyernst/Documents/Repos/itinerary-app");
        let cfg = PreviewLaunchConfig::new(app_dir, 8099);
        let server = PreviewServer::spawn(cfg).expect("dx serve should spawn");
        assert_eq!(server.url(), "http://127.0.0.1:8099/");
        tokio::time::sleep(Duration::from_secs(10)).await;
        println!("status after 10s: {:?}", server.status());
        drop(server);
    }
}
