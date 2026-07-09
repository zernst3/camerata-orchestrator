//! Launch (or reuse) the `camerata-server` BFF as a SUBPROCESS (Phase G, GAP-1 severance).
//!
//! Before this module existed, `main.rs` called `camerata_server::serve(BFF_ADDR)` on a
//! background Tokio runtime — the UI crate compile-depended on the entire server. This
//! module replaces that with a spawned `camerata-server` binary plus an HTTP health poll,
//! so the UI's only relationship to the backend is the wire contract.
//!
//! The launch sequence (see [`ensure_server_running`]):
//!   1. Probe `GET {base}/api/health`. If something is already serving a HEALTHY camerata
//!      BFF on the port (a standalone `cargo run -p camerata-server`, or a server left by
//!      another cockpit instance), REUSE it — do not spawn a duplicate.
//!   2. Otherwise resolve the server binary ([`resolve_server_bin`]) and spawn it with the
//!      bind address in `CAMERATA_SERVER_ADDR` (the env the server's `main.rs` reads).
//!   3. Poll health until ready (bounded). If the child dies (classically: `AddrInUse`
//!      because an UNHEALTHY process is squatting on the port), reclaim the port —
//!      `lsof -ti:<port>` + `kill -9`, same takeover the embedded flow used — and retry.
//!
//! PORT-TAKEOVER SEMANTICS vs the old embedded flow: the old flow ALWAYS killed whatever
//! held :8787 (newest launch wins), because the squatter was, by construction, a stale
//! embedded server running old code. Now that the server is a separate binary, a healthy
//! responder is more likely deliberate (standalone dev server), so a healthy BFF is
//! REUSED and only unhealthy squatters are killed. The residual risk — reusing a healthy
//! server built from older code — is called out in the Phase G notes.
//!
//! LIFECYCLE: [`ServerGuard`] terminates the spawned child on Drop (SIGTERM first so the
//! server's own shutdown hook can reap in-flight `claude` subprocesses, then SIGKILL).
//! Drop alone is NOT enough: the Dioxus/tao event loop exits the process via
//! `std::process::exit`, which skips Rust destructors. So each spawn also starts a tiny
//! Unix WATCHDOG (`sh` loop, see [`spawn_watchdog`]) that SIGTERMs the server the moment
//! the cockpit process disappears — covering `process::exit` and even a SIGKILL of the
//! cockpit. The workspace forbids `unsafe`, so signals go through `/bin/kill`, not libc.
//! On non-Unix there is no watchdog (Drop-only); a leaked healthy server is then found
//! and REUSED by the next launch, so the failure mode is a stray process, not a broken app.

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// The server binary's file name on this platform.
#[cfg(windows)]
pub const SERVER_BIN_NAME: &str = "camerata-server.exe";
#[cfg(not(windows))]
pub const SERVER_BIN_NAME: &str = "camerata-server";

// ── binary resolution ───────────────────────────────────────────────────────

/// The ordered candidate list for locating the `camerata-server` binary. PURE — takes its
/// inputs explicitly so tests can exercise the ordering without touching the process env:
///
///   1. `env_override` — the `CAMERATA_SERVER_BIN` value, if set. Always first; an explicit
///      override is trusted verbatim (a bad path fails loudly at spawn, not silently).
///   2. Sibling of the running executable (packaged layout: the app bundle ships
///      `camerata-server` next to `camerata-ui`).
///   3. Dev fallback: `<workspace>/target/debug/camerata-server`, derived from the ui
///      crate's compile-time `CARGO_MANIFEST_DIR` (`crates/ui` → workspace is `../..`).
///      Covers `cargo run -p camerata-ui` when the exe-sibling copy doesn't exist.
pub fn server_bin_candidates(
    env_override: Option<&str>,
    current_exe: Option<&Path>,
    ui_manifest_dir: Option<&Path>,
) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(over) = env_override {
        if !over.trim().is_empty() {
            out.push(PathBuf::from(over));
        }
    }
    if let Some(exe) = current_exe {
        if let Some(dir) = exe.parent() {
            out.push(dir.join(SERVER_BIN_NAME));
        }
    }
    if let Some(manifest) = ui_manifest_dir {
        // crates/ui -> crates -> <workspace root>
        if let Some(workspace) = manifest.parent().and_then(Path::parent) {
            out.push(workspace.join("target").join("debug").join(SERVER_BIN_NAME));
        }
    }
    out
}

/// Resolve the server binary for THIS process: first existing candidate wins; an explicit
/// `CAMERATA_SERVER_BIN` override wins unconditionally (even if the path doesn't exist —
/// the spawn error then names the override instead of silently falling back). If nothing
/// exists, returns the first candidate so the caller's error message shows a real path.
pub fn resolve_server_bin() -> PathBuf {
    let env_override = std::env::var("CAMERATA_SERVER_BIN").ok();
    let exe = std::env::current_exe().ok();
    let manifest: Option<&Path> = Some(Path::new(env!("CARGO_MANIFEST_DIR")));
    let candidates =
        server_bin_candidates(env_override.as_deref(), exe.as_deref(), manifest);
    if let Some(over) = env_override.as_deref() {
        if !over.trim().is_empty() {
            return PathBuf::from(over);
        }
    }
    candidates
        .iter()
        .find(|c| c.exists())
        .cloned()
        .or_else(|| candidates.first().cloned())
        .unwrap_or_else(|| PathBuf::from(SERVER_BIN_NAME))
}

// ── health probing + the reuse-vs-spawn decision ────────────────────────────

/// What the startup sequence should do about the BFF port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchDecision {
    /// A healthy camerata BFF already answers on the port — use it, spawn nothing.
    ReuseExisting,
    /// Nothing healthy there — spawn our own server.
    SpawnNew,
}

/// One health probe: `GET {base}/api/health` must return 200 AND identify itself as
/// `camerata-server` (the handler returns `{"status":"ok","service":"camerata-server"}`).
/// The identity check keeps us from "reusing" some unrelated dev server that happens to
/// answer 200 on the port. Any network/timeout/parse failure is simply "not healthy".
async fn probe_health(client: &reqwest::Client, base_url: &str) -> bool {
    let url = format!("{}/api/health", base_url.trim_end_matches('/'));
    match client.get(&url).send().await {
        Ok(resp) if resp.status().is_success() => match resp.json::<serde_json::Value>().await {
            Ok(body) => {
                body.get("service").and_then(|s| s.as_str()) == Some("camerata-server")
            }
            Err(_) => false,
        },
        _ => false,
    }
}

/// A short-timeout client for health probes, so a wedged socket can't hang startup.
fn probe_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_millis(750))
        .build()
        .unwrap_or_default()
}

/// The reuse-vs-spawn DECISION, isolated so it's testable against a mock server: probe
/// the base URL once; healthy → [`LaunchDecision::ReuseExisting`], anything else →
/// [`LaunchDecision::SpawnNew`].
pub async fn decide_reuse_or_spawn(base_url: &str) -> LaunchDecision {
    if probe_health(&probe_client(), base_url).await {
        LaunchDecision::ReuseExisting
    } else {
        LaunchDecision::SpawnNew
    }
}

/// Outcome of waiting for a just-spawned child to become healthy.
#[derive(Debug, PartialEq, Eq)]
enum WaitOutcome {
    /// Health probe succeeded — the server is up.
    Ready,
    /// The child exited before ever becoming healthy (classically `AddrInUse`).
    ChildExited,
    /// The deadline passed with the child still running but never healthy.
    TimedOut,
}

/// Poll `{base}/api/health` until it answers healthy, the child exits, or `timeout`
/// elapses. ~150ms cadence — fast enough that startup feels instant once the server binds.
async fn wait_for_health(base_url: &str, child: &mut Child, timeout: Duration) -> WaitOutcome {
    let client = probe_client();
    let deadline = Instant::now() + timeout;
    loop {
        if probe_health(&client, base_url).await {
            return WaitOutcome::Ready;
        }
        // Child died (e.g. bind failed) — no point polling out the full deadline.
        if let Ok(Some(_status)) = child.try_wait() {
            return WaitOutcome::ChildExited;
        }
        if Instant::now() >= deadline {
            return WaitOutcome::TimedOut;
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}

// ── the spawned-child guard ─────────────────────────────────────────────────

/// Keeps the spawned `camerata-server` child alive and TERMINATES it when dropped.
/// The reused-existing case holds no child (dropping it does nothing).
///
/// Termination order: SIGTERM first (the server installs a shutdown hook that reaps
/// in-flight `claude` audit subprocesses on SIGTERM — a straight SIGKILL would orphan
/// them), a short grace poll, then SIGKILL. On non-Unix only `Child::kill` is available.
#[derive(Debug)]
pub struct ServerGuard {
    child: Option<Child>,
}

impl ServerGuard {
    /// Guard for the reuse case: nothing to own, Drop is a no-op.
    pub fn reused() -> Self {
        Self { child: None }
    }

    fn spawned(child: Child) -> Self {
        Self { child: Some(child) }
    }

    /// True when this guard owns a spawned child (false = reused an existing server).
    /// Introspection for the test suite; the app itself only holds the guard.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn owns_child(&self) -> bool {
        self.child.is_some()
    }

    /// The spawned child's PID, if this guard owns one. Introspection for the test
    /// suite; the app itself only holds the guard.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn pid(&self) -> Option<u32> {
        self.child.as_ref().map(Child::id)
    }
}

impl Drop for ServerGuard {
    fn drop(&mut self) {
        let Some(mut child) = self.child.take() else { return };

        #[cfg(unix)]
        {
            // Graceful first: SIGTERM triggers the server's shutdown hook (reaps its own
            // `claude` children), then poll briefly before escalating to SIGKILL. The
            // workspace forbids `unsafe`, so the signal goes through `/bin/kill`.
            // `.output()` (not `.status()`) so a "no such process" gripe from `kill`
            // is captured instead of splattering the app console.
            let _ = Command::new("kill")
                .arg("-TERM")
                .arg(child.id().to_string())
                .output();
            let deadline = Instant::now() + Duration::from_millis(2000);
            while Instant::now() < deadline {
                match child.try_wait() {
                    Ok(Some(_)) => return, // exited and reaped
                    Ok(None) => std::thread::sleep(Duration::from_millis(50)),
                    Err(_) => break, // try_wait errored — escalate
                }
            }
        }
        let _ = child.kill();
        let _ = child.wait();
    }
}

// ── the exit watchdog (Unix) ────────────────────────────────────────────────

/// Start a detached `sh` watchdog that SIGTERMs the spawned server if THIS process
/// dies while the server is still running.
///
/// Why it exists: tao/winit exits the app with `std::process::exit`, which never runs
/// Rust destructors, so `ServerGuard::drop` alone would leak the server on a normal
/// quit. The watchdog polls both PIDs (`kill -0`) every half-second:
///   - server exits first (the guard's own Drop killed it) → watchdog just exits;
///   - cockpit exits first (normal quit, `process::exit`, even SIGKILL) → watchdog
///     SIGTERMs the server (its shutdown hook reaps in-flight `claude` children).
/// Best-effort by design: if the watchdog can't spawn, the next cockpit launch still
/// reuses/reclaims whatever is on the port.
#[cfg(unix)]
fn spawn_watchdog(server_pid: u32) {
    let parent_pid = std::process::id();
    let script = format!(
        "while kill -0 {parent_pid} 2>/dev/null && kill -0 {server_pid} 2>/dev/null; do \
             sleep 0.5; \
         done; \
         kill -0 {parent_pid} 2>/dev/null || kill -TERM {server_pid} 2>/dev/null"
    );
    match Command::new("sh").arg("-c").arg(script).spawn() {
        // Detach: the watchdog is meant to outlive us; never wait on it.
        Ok(_) => {}
        Err(e) => eprintln!(
            "[camerata-ui] could not start the server watchdog (pid {server_pid} may \
             outlive the app if it is quit via process::exit): {e}"
        ),
    }
}

/// Non-Unix: no watchdog. Drop still kills the child; a child leaked through
/// `process::exit` is reused (if healthy) or reclaimed by the next launch.
#[cfg(not(unix))]
fn spawn_watchdog(_server_pid: u32) {}

// ── port takeover (moved verbatim from main.rs) ─────────────────────────────

/// Kill whatever process(es) are currently holding `port`, so a fresh launch can take
/// the socket over. Used by the takeover retry loop in [`ensure_server_running`]: when
/// the port is held by something that does NOT answer the camerata health check (a
/// wedged/stale server), the newest launch must win — otherwise the cockpit silently
/// talks to dead air.
///
/// On Unix (the app targets macOS) we ask `lsof -ti:<port>` for the holder PID(s) and
/// `kill -9` each one. These are instant probes, not long-running processes, so plain
/// `std::process::Command` is fine. On non-Unix we can't reliably enumerate the holder,
/// so this is a no-op and the caller simply retries / gives up loudly.
#[cfg(unix)]
pub fn reclaim_port(port: &str) {
    let out = match Command::new("lsof").arg("-ti").arg(format!(":{port}")).output() {
        Ok(out) => out,
        Err(e) => {
            eprintln!("[camerata-ui] could not run lsof to reclaim :{port}: {e}");
            return;
        }
    };

    let pids = String::from_utf8_lossy(&out.stdout);
    for pid in pids.split_whitespace() {
        match Command::new("kill").arg("-9").arg(pid).status() {
            Ok(_) => eprintln!("[camerata-ui] killed stale :{port} holder pid {pid}"),
            Err(e) => eprintln!("[camerata-ui] failed to kill pid {pid} on :{port}: {e}"),
        }
    }
}

/// Non-Unix fallback: we have no portable way to enumerate the port holder, so we can't
/// reclaim it. The caller just retries and ultimately gives up loudly.
#[cfg(not(unix))]
pub fn reclaim_port(_port: &str) {}

// ── orchestration ───────────────────────────────────────────────────────────

/// Everything [`ensure_server_running`] needs, gathered into one struct so tests can
/// inject a scratch binary/addr/env without touching process-global state.
#[derive(Debug, Clone)]
pub struct ServerLaunchConfig {
    /// The bind address handed to the server via `CAMERATA_SERVER_ADDR` (e.g. `127.0.0.1:8787`).
    pub addr: String,
    /// The server binary to spawn when nothing healthy is on the port.
    pub bin: PathBuf,
    /// Extra env vars set on the child (tests: redirect `HOME` so the spawned server's
    /// persistence lands in a temp dir instead of the real per-user data dir).
    pub extra_env: Vec<(String, String)>,
    /// How long each spawn attempt waits for the health endpoint before giving up.
    pub health_timeout: Duration,
}

impl ServerLaunchConfig {
    /// The production config for `addr`: resolved binary, inherited env, 10s readiness.
    pub fn for_addr(addr: &str) -> Self {
        Self {
            addr: addr.to_string(),
            bin: resolve_server_bin(),
            extra_env: Vec::new(),
            health_timeout: Duration::from_secs(10),
        }
    }

    /// `http://{addr}` — what the health poll (and the cockpit) fetches from.
    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }
}

/// Make sure a camerata BFF is serving on `cfg.addr`, spawning one if needed. Returns a
/// [`ServerGuard`] the caller must keep alive for the app's lifetime (dropping it kills
/// a spawned server; the reuse case is a no-op guard).
///
/// Decision flow per the module docs: healthy → reuse; else spawn → wait for health;
/// child died or never got healthy → maybe someone else won a startup race (re-probe,
/// reuse if healthy) → otherwise reclaim the port from the unhealthy squatter and retry,
/// up to 3 attempts, then fail loudly.
pub async fn ensure_server_running(cfg: &ServerLaunchConfig) -> anyhow::Result<ServerGuard> {
    let base = cfg.base_url();

    if decide_reuse_or_spawn(&base).await == LaunchDecision::ReuseExisting {
        eprintln!(
            "[camerata-ui] healthy camerata BFF already on {} — reusing it (no spawn)",
            cfg.addr
        );
        return Ok(ServerGuard::reused());
    }

    const MAX_ATTEMPTS: u32 = 3;
    for attempt in 1..=MAX_ATTEMPTS {
        let mut command = Command::new(&cfg.bin);
        command.env("CAMERATA_SERVER_ADDR", &cfg.addr);
        for (k, v) in &cfg.extra_env {
            command.env(k, v);
        }
        // stdout/stderr inherit by default — server logs land in the app console,
        // exactly where the embedded BFF's logs used to go.
        let mut child = command.spawn().map_err(|e| {
            anyhow::anyhow!(
                "could not spawn camerata-server at {}: {e}\n\
                 (set CAMERATA_SERVER_BIN to the server binary, or `cargo build -p camerata-server`)",
                cfg.bin.display()
            )
        })?;
        eprintln!(
            "[camerata-ui] spawned camerata-server (pid {}) from {} on {}",
            child.id(),
            cfg.bin.display(),
            cfg.addr
        );
        spawn_watchdog(child.id());

        match wait_for_health(&base, &mut child, cfg.health_timeout).await {
            WaitOutcome::Ready => return Ok(ServerGuard::spawned(child)),
            outcome @ (WaitOutcome::ChildExited | WaitOutcome::TimedOut) => {
                // Reap/terminate our failed child before touching the port.
                drop(ServerGuard::spawned(child));

                // Startup race: another cockpit instance may have spawned a server that
                // just became healthy (our child then died with AddrInUse). Reuse it
                // rather than killing it.
                if probe_health(&probe_client(), &base).await {
                    eprintln!(
                        "[camerata-ui] another camerata BFF became healthy on {} — reusing it",
                        cfg.addr
                    );
                    return Ok(ServerGuard::reused());
                }

                if attempt < MAX_ATTEMPTS {
                    // PORT TAKEOVER — the newest launch must win. Something unhealthy is
                    // squatting on the port (a wedged/stale server that no longer answers
                    // health). Kill it and retry, exactly like the embedded flow did.
                    let port = cfg.addr.rsplit(':').next().unwrap_or("8787");
                    eprintln!(
                        "[camerata-ui] server not healthy on :{port} ({outcome:?}, attempt \
                         {attempt}/{MAX_ATTEMPTS}); reclaiming the port and retrying"
                    );
                    reclaim_port(port);
                    // Give the OS a moment to release the socket after the kill.
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }
    }

    // Out of attempts: the port is still held by something unhealthy and we could not
    // take it over. Fail loudly so it's obvious the cockpit has NO working BFF.
    anyhow::bail!(
        "\n\
         ╔══════════════════════════════════════════════════════════════╗\n\
         ║  [camerata-ui] FATAL: no healthy BFF on {}        ║\n\
         ║                                                              ║\n\
         ║  The port is held by a process that does not answer the     ║\n\
         ║  camerata health check and could not be reclaimed after     ║\n\
         ║  retries. The cockpit has NO working backend.                ║\n\
         ║                                                              ║\n\
         ║  Fix: quit ALL Camerata instances, free the port, relaunch.  ║\n\
         ╚══════════════════════════════════════════════════════════════╝\n",
        cfg.addr
    )
}

/// Park a guard for the remainder of the process lifetime. `main.rs` calls this from the
/// launcher thread so the spawned server stays owned somewhere; actual termination on a
/// normal quit rides the exit watchdog (statics are never dropped).
pub fn hold_for_app_lifetime(guard: ServerGuard) {
    static HELD: Mutex<Option<ServerGuard>> = Mutex::new(None);
    if let Ok(mut slot) = HELD.lock() {
        // Replacing an earlier guard drops (and terminates) the old child — in practice
        // this fires at most once per process.
        *slot = Some(guard);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── binary resolution (pure candidate ordering) ─────────────────────────

    #[test]
    fn candidates_env_override_comes_first() {
        let c = server_bin_candidates(
            Some("/custom/bin/camerata-server"),
            Some(Path::new("/apps/camerata/camerata-ui")),
            Some(Path::new("/ws/crates/ui")),
        );
        assert_eq!(c[0], PathBuf::from("/custom/bin/camerata-server"));
        assert_eq!(c.len(), 3);
    }

    #[test]
    fn candidates_sibling_of_exe_when_no_override() {
        let c = server_bin_candidates(
            None,
            Some(Path::new("/apps/camerata/camerata-ui")),
            Some(Path::new("/ws/crates/ui")),
        );
        assert_eq!(c[0], Path::new("/apps/camerata").join(SERVER_BIN_NAME));
    }

    #[test]
    fn candidates_dev_fallback_is_workspace_target_debug() {
        let c = server_bin_candidates(None, None, Some(Path::new("/ws/crates/ui")));
        assert_eq!(
            c,
            vec![Path::new("/ws").join("target").join("debug").join(SERVER_BIN_NAME)]
        );
    }

    #[test]
    fn candidates_blank_override_is_ignored() {
        let c = server_bin_candidates(Some("  "), None, Some(Path::new("/ws/crates/ui")));
        assert_eq!(c.len(), 1, "blank override must not produce a candidate");
    }

    #[test]
    #[serial_test::serial(server_bin_env)]
    fn resolve_honors_env_override_even_if_missing() {
        std::env::set_var("CAMERATA_SERVER_BIN", "/nonexistent/override/camerata-server");
        let resolved = resolve_server_bin();
        std::env::remove_var("CAMERATA_SERVER_BIN");
        assert_eq!(resolved, PathBuf::from("/nonexistent/override/camerata-server"));
    }

    #[test]
    #[serial_test::serial(server_bin_env)]
    fn resolve_without_override_prefers_an_existing_candidate() {
        std::env::remove_var("CAMERATA_SERVER_BIN");
        let resolved = resolve_server_bin();
        // Regardless of which candidate wins, it must be a concrete path ending in the
        // platform binary name — and if any candidate exists on this machine, the
        // resolved one must exist too (first-existing-wins).
        assert_eq!(
            resolved.file_name().and_then(|n| n.to_str()),
            Some(SERVER_BIN_NAME)
        );
        let exe = std::env::current_exe().ok();
        let candidates = server_bin_candidates(
            None,
            exe.as_deref(),
            Some(Path::new(env!("CARGO_MANIFEST_DIR"))),
        );
        if candidates.iter().any(|c| c.exists()) {
            assert!(resolved.exists(), "an existing candidate was available but not chosen");
        }
    }

    // ── reuse-vs-spawn decision against a mock BFF ──────────────────────────

    #[tokio::test]
    async fn decide_reuses_when_health_is_a_camerata_bff() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/api/health"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(
                serde_json::json!({ "status": "ok", "service": "camerata-server" }),
            ))
            .mount(&server)
            .await;
        assert_eq!(decide_reuse_or_spawn(&server.uri()).await, LaunchDecision::ReuseExisting);
    }

    #[tokio::test]
    async fn decide_spawns_when_health_absent() {
        // Mock server with NO /api/health mount — the probe gets a 404.
        let server = wiremock::MockServer::start().await;
        assert_eq!(decide_reuse_or_spawn(&server.uri()).await, LaunchDecision::SpawnNew);
    }

    #[tokio::test]
    async fn decide_spawns_when_health_is_not_a_camerata_bff() {
        // 200, but some OTHER service answers — must NOT be treated as reusable.
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/api/health"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "status": "ok", "service": "other" })),
            )
            .mount(&server)
            .await;
        assert_eq!(decide_reuse_or_spawn(&server.uri()).await, LaunchDecision::SpawnNew);
    }

    #[tokio::test]
    async fn decide_spawns_when_nothing_listens() {
        // A port with no listener at all (bind, learn the port, drop the listener).
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        drop(listener);
        assert_eq!(
            decide_reuse_or_spawn(&format!("http://{addr}")).await,
            LaunchDecision::SpawnNew
        );
    }

    #[tokio::test]
    async fn ensure_reuses_existing_healthy_bff_without_spawning() {
        // With a healthy mock BFF on the addr, ensure_server_running must return a
        // no-child guard even though cfg.bin points at a nonexistent binary (proof it
        // never tried to spawn).
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/api/health"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(
                serde_json::json!({ "status": "ok", "service": "camerata-server" }),
            ))
            .mount(&server)
            .await;
        let addr = server.uri().trim_start_matches("http://").to_string();
        let cfg = ServerLaunchConfig {
            addr,
            bin: PathBuf::from("/nonexistent/never-spawned"),
            extra_env: Vec::new(),
            health_timeout: Duration::from_secs(1),
        };
        let guard = ensure_server_running(&cfg).await.expect("reuse path");
        assert!(!guard.owns_child());
    }

    // ── the real thing: spawn the built camerata-server binary ──────────────

    /// Integration proof for the whole ladder: resolve the REAL built server binary,
    /// spawn it on a scratch port, wait for health, hit an endpoint, drop the guard,
    /// and assert the process is gone. Skips (loudly) if the binary hasn't been built
    /// (`cargo build -p camerata-server` first).
    ///
    /// The child's HOME is redirected to a temp dir so the server's persistence
    /// (`dirs::data_dir()` → `$HOME/Library/Application Support` on macOS) never
    /// touches the real per-user camerata data.
    #[tokio::test]
    async fn spawns_real_server_waits_healthy_and_kills_on_drop() {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        let candidates = server_bin_candidates(None, None, Some(manifest));
        let Some(bin) = candidates.into_iter().find(|c| c.exists()) else {
            eprintln!(
                "SKIP spawns_real_server_waits_healthy_and_kills_on_drop: \
                 camerata-server binary not built (run `cargo build -p camerata-server`)"
            );
            return;
        };

        // A scratch HOME so the child's persistence never touches real user data.
        let scratch = std::env::temp_dir().join(format!(
            "camerata-ui-server-proc-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&scratch).expect("scratch HOME");

        // An ephemeral free port (bind :0, read it back, release it).
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr").to_string();
        drop(listener);

        let cfg = ServerLaunchConfig {
            addr,
            bin,
            extra_env: vec![
                ("HOME".to_string(), scratch.display().to_string()),
                // Keep the child fully offline/inert: no GitHub polling from a test.
                ("CAMERATA_GITHUB_TOKEN".to_string(), String::new()),
            ],
            health_timeout: Duration::from_secs(15),
        };

        let guard = ensure_server_running(&cfg).await.expect("server should come up");
        assert!(guard.owns_child(), "nothing was on the scratch port; must have spawned");
        let pid = guard.pid().expect("spawned guard has a pid");

        // The server answers a real endpoint, not just /api/health.
        let rules: serde_json::Value = probe_client()
            .get(format!("{}/api/rules", cfg.base_url()))
            .send()
            .await
            .expect("GET /api/rules")
            .json()
            .await
            .expect("rules JSON");
        assert!(rules.is_array(), "expected the rules list, got: {rules}");

        // Drop the guard → the child must be terminated (SIGTERM path) and reaped.
        drop(guard);
        // Guard::drop reaps the child itself, so by now the PID must be gone (modulo
        // PID reuse, which a fresh scratch machine won't hit in milliseconds).
        #[cfg(unix)]
        {
            let alive = Command::new("kill")
                .arg("-0")
                .arg(pid.to_string())
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
            assert!(!alive, "spawned server pid {pid} still alive after guard drop");
        }
        // And the port must answer nothing.
        assert_eq!(
            decide_reuse_or_spawn(&cfg.base_url()).await,
            LaunchDecision::SpawnNew
        );

        let _ = std::fs::remove_dir_all(&scratch);
    }
}
