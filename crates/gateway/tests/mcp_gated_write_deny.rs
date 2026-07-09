//! Phase T — the highest-fidelity gate-denial proof: drive the REAL `camerata-gateway`
//! binary over its actual stdio MCP transport (not the in-process `Gateway::gated_write`
//! method call, not the pure `evaluate_call` function) and assert the wire-level tool
//! result says DENIED, with no file created on disk.
//!
//! # Why raw JSON-RPC lines instead of an `rmcp` client transport
//!
//! `rmcp` 1.7 ships a `TokioChildProcess` client transport, but it lives behind the
//! crate's `client` feature, which `camerata-gateway`'s `Cargo.toml` does not enable (this
//! crate is a SERVER only — `features = ["server", "transport-io"]`). Enabling `client`
//! just for one integration test would grow the crate's dependency surface for every
//! consumer, not just this test. An MCP client is, at the wire level, nothing more than
//! newline-delimited JSON-RPC 2.0 messages over stdin/stdout (rmcp's own stdio transport
//! is exactly that framing), so writing the two request lines by hand and reading the two
//! response lines back proves the real transport just as faithfully, with zero new deps.
//!
//! # Handshake
//!
//! rmcp 1.7's server no longer gates on the client's `notifications/initialized`
//! notification (see `ServerInitializeError::ExpectedInitializedNotification`'s deprecation
//! note in `rmcp::service::server`) — it only requires the `initialize` request/response
//! round-trip before accepting other requests. So: send `initialize`, read the response,
//! then send `tools/call` for `gated_write` directly.
//!
//! # Hermeticity
//!
//! - `CARGO_BIN_EXE_camerata-gateway` is the exact binary `cargo test -p camerata-gateway`
//!   already builds as part of running this test target — no manual pre-build step, no
//!   network, no real `claude`.
//! - The jail root is a fresh tempdir, cleaned up at the end.
//! - Reads happen on a background thread over a channel with a bounded `recv_timeout`, so
//!   a hung or crashed child fails the test with a clear message instead of hanging CI.
//! - The child is wrapped in a kill-on-drop guard, so it is reaped even if an assertion
//!   panics partway through.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

/// Kills and reaps the child on drop, so a panicking assertion never leaks a live
/// subprocess (this test spawns a real MCP server that would otherwise sit blocked on
/// its stdin read forever).
struct KillOnDrop(Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Spawn a background thread that continuously reads lines from `stdout` and forwards
/// each non-empty one over a channel, so the main thread can `recv_timeout` instead of
/// blocking indefinitely on a hung or crashed child.
fn spawn_line_reader(stdout: std::process::ChildStdout) -> Receiver<String> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => break, // EOF — child exited or closed stdout.
                Ok(_) => {
                    if tx.send(line).is_err() {
                        break; // Receiver dropped (test already finished).
                    }
                }
                Err(_) => break,
            }
        }
    });
    rx
}

#[test]
fn mcp_transport_denies_a_forbidden_gated_write_with_no_file_created() {
    let bin = env!("CARGO_BIN_EXE_camerata-gateway");

    let jail = std::env::temp_dir().join(format!(
        "cam-gw-transport-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&jail).expect("create jail dir");

    // Spawn the REAL binary. `CAMERATA_RULES_FILE` is deliberately left unset so the
    // gateway falls back to its verified default subset `[GOV-1]` — the exact rule this
    // test's planted violation (a "forbidden" path) exercises. Stderr is discarded
    // (`Stdio::null()`): the binary traces every decision there, and this test only
    // cares about the stdout MCP transport.
    let child = match Command::new(bin)
        .env("CAMERATA_WORKTREE_ROOT", &jail)
        .env_remove("CAMERATA_RULES_FILE")
        .env_remove("CAMERATA_GATE_EVENTS_FILE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            // Gate cleanly rather than fail: if the binary genuinely isn't built (e.g. a
            // partial/offline build), this test skips instead of red-herring-failing on
            // an environment problem unrelated to gate logic.
            eprintln!(
                "SKIP mcp_transport_denies_a_forbidden_gated_write_with_no_file_created: \
                 could not spawn {bin} ({e}); run `cargo build -p camerata-gateway` first"
            );
            let _ = std::fs::remove_dir_all(&jail);
            return;
        }
    };
    let mut child = KillOnDrop(child);

    let mut stdin = child.0.stdin.take().expect("child stdin must be piped");
    let stdout = child.0.stdout.take().expect("child stdout must be piped");
    let rx = spawn_line_reader(stdout);

    // ── 1. initialize ──────────────────────────────────────────────────────────────
    let init_req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "gate-boundary-test", "version": "0.0.0" }
        }
    });
    writeln!(stdin, "{init_req}").expect("write initialize request");
    stdin.flush().ok();

    let init_line = rx
        .recv_timeout(Duration::from_secs(10))
        .expect("no initialize response within 10s (binary hung, crashed, or wrong protocol)");
    let init_resp: serde_json::Value =
        serde_json::from_str(init_line.trim()).expect("initialize response must be valid JSON");
    assert_eq!(init_resp["id"], serde_json::json!(1));
    assert!(
        init_resp.get("result").is_some(),
        "initialize must succeed: {init_resp}"
    );

    // ── 2. tools/call gated_write with a forbidden path ────────────────────────────
    let target_rel = "sub/forbidden/leak.rs";
    let call_req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "name": "gated_write",
            "arguments": { "path": target_rel, "content": "// should never land on disk" }
        }
    });
    writeln!(stdin, "{call_req}").expect("write tools/call request");
    stdin.flush().ok();

    let call_line = rx
        .recv_timeout(Duration::from_secs(10))
        .expect("no tools/call response within 10s (binary hung, crashed, or wrong protocol)");
    let call_resp: serde_json::Value =
        serde_json::from_str(call_line.trim()).expect("tools/call response must be valid JSON");
    assert_eq!(call_resp["id"], serde_json::json!(2));

    let text = call_resp["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("tool result must carry a text content item: {call_resp}"));
    assert!(
        text.contains("DENIED") && text.contains("GOV-1"),
        "expected a GOV-1 denial over the real MCP transport, got: {text}"
    );

    // ── 3. structural claim: the write never touched disk ──────────────────────────
    assert!(
        !jail.join(target_rel).exists(),
        "a denied write must never create the file on disk"
    );

    // Clean shutdown: closing stdin (EOF) lets the server's stdio loop exit on its own;
    // `KillOnDrop` still force-kills it once this scope ends, belt-and-suspenders.
    drop(stdin);

    let _ = std::fs::remove_dir_all(&jail);
}
