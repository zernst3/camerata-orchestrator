//! In-app terminal: `GET /api/terminal/ws?cwd=<abs-path>` — a WebSocket endpoint
//! that spawns a PTY-backed shell and bridges it bidirectionally to the client.
//!
//! Architecture (issue #38)
//! ─────────────────────────
//! One PTY per WebSocket connection; multiple open terminals = multiple connections.
//! The client (xterm.js in the Dioxus desktop webview) connects, the server spawns
//! `$SHELL` (else `/bin/bash`) in the requested `cwd`, and bytes flow both ways:
//!
//!   PTY master reader ──(mpsc)──▶ ws sink   (PTY output → terminal screen)
//!   ws receiver       ──────────▶ PTY writer (keystrokes → shell)
//!
//! The PTY reader is blocking (`portable_pty` gives us a `Box<dyn Read>`), so it
//! runs on a dedicated `spawn_blocking` thread, forwarding bytes through a channel.
//!
//! Control messages (JSON): `{"resize":{"cols":N,"rows":M}}` — forwarded to
//! `pty.resize(...)` to keep the PTY geometry in sync with the xterm.js viewport.
//!
//! RUNTIME-TODO: This module requires a real PTY-capable OS (macOS / Linux) and an
//! actual WebSocket client to exercise.  `cargo build` + `cargo clippy` are clean
//! (verified); the actual PTY bridge path is tested at runtime only.

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Query,
    },
    response::IntoResponse,
};
use futures::SinkExt;
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use serde::Deserialize;
use tokio::sync::mpsc;

/// Query parameters accepted by the WebSocket upgrade endpoint.
#[derive(Debug, Deserialize)]
pub struct TerminalQuery {
    /// Absolute path to use as the working directory for the shell.
    /// Falls back to `$HOME` if absent or the path does not exist.
    #[serde(default)]
    pub cwd: Option<String>,
}

/// JSON control message the client may send instead of raw terminal bytes.
#[derive(Debug, Deserialize)]
struct ControlMsg {
    resize: Option<ResizeMsg>,
}

#[derive(Debug, Deserialize)]
struct ResizeMsg {
    cols: u16,
    rows: u16,
}

/// `GET /api/terminal/ws?cwd=<abs-path>`
///
/// Upgrades to WebSocket, spawns the user's shell in a PTY, and bridges
/// PTY ↔ ws bidirectionally until the client disconnects.
pub async fn ws_handler(
    ws: WebSocketUpgrade,
    Query(params): Query<TerminalQuery>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, params.cwd))
}

async fn handle_socket(socket: WebSocket, cwd: Option<String>) {
    if let Err(e) = run_terminal(socket, cwd).await {
        eprintln!("[terminal] session error: {e}");
    }
}

async fn run_terminal(socket: WebSocket, cwd: Option<String>) -> anyhow::Result<()> {
    use futures::StreamExt;

    // ── Resolve cwd ──────────────────────────────────────────────────────────
    let work_dir = resolve_cwd(cwd.as_deref());

    // ── Spawn the PTY + shell ────────────────────────────────────────────────
    //
    // RUNTIME-TODO: `native_pty_system()` works on macOS/Linux with a real TTY.
    // In a headless CI environment without a PTY device this will return an error;
    // the connection closes gracefully via the `?` propagation above.
    let pty_system = native_pty_system();

    let pair = pty_system.openpty(PtySize {
        rows: 24,
        cols: 80,
        pixel_width: 0,
        pixel_height: 0,
    })?;

    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
    let mut cmd = CommandBuilder::new(&shell);
    cmd.cwd(&work_dir);
    // Mark this as an interactive login shell so .bashrc/.zshrc load.
    cmd.arg("--login");

    // `_child` keeps the shell alive until it is dropped at the end of this fn.
    let _child = pair.slave.spawn_command(cmd)?;

    // `take_writer()` gives a `Box<dyn Write + Send>` we can use for ws→pty writes.
    // It can only be called once per master; keep `pair.master` alive for resize() calls.
    let mut pty_writer = pair.master.take_writer()?;

    // The PTY reader is blocking; run it on a blocking thread and pipe bytes
    // into the async world via an mpsc channel.
    // RUNTIME-TODO: channel capacity 64 is arbitrary; tune if backpressure builds.
    let (tx, mut rx) = mpsc::channel::<Vec<u8>>(64);
    let mut pty_reader = pair.master.try_clone_reader()?;
    std::thread::spawn(move || {
        use std::io::Read;
        let mut buf = [0u8; 4096];
        loop {
            match pty_reader.read(&mut buf) {
                Ok(0) => break, // EOF — shell exited
                Ok(n) => {
                    if tx.blocking_send(buf[..n].to_vec()).is_err() {
                        break; // ws closed
                    }
                }
                Err(_) => break,
            }
        }
    });

    // ── Bridge bidirectionally ───────────────────────────────────────────────
    let (mut ws_sink, mut ws_stream) = socket.split();

    loop {
        tokio::select! {
            // PTY → ws (terminal output)
            Some(bytes) = rx.recv() => {
                // RUNTIME-TODO: Text is the safer choice for xterm.js (`term.write(data)`)
                // since it handles UTF-8. Switch to Binary if you see mojibake on high-byte
                // sequences.
                if ws_sink.send(Message::Text(
                    String::from_utf8_lossy(&bytes).to_string()
                )).await.is_err() {
                    break;
                }
            }

            // ws → PTY (keystrokes + control messages)
            msg = ws_stream.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        // Try to parse as a JSON control message first.
                        if let Ok(ctrl) = serde_json::from_str::<ControlMsg>(&text) {
                            if let Some(r) = ctrl.resize {
                                // RUNTIME-TODO: resize() keeps the PTY geometry in sync with
                                // xterm.js.  Errors here are non-fatal (the session continues).
                                let _ = pair.master.resize(PtySize {
                                    rows: r.rows,
                                    cols: r.cols,
                                    pixel_width: 0,
                                    pixel_height: 0,
                                });
                            }
                        } else {
                            // Plain text → write bytes to the PTY (keystrokes / paste).
                            use std::io::Write;
                            let _ = pty_writer.write_all(text.as_bytes());
                        }
                    }
                    Some(Ok(Message::Binary(data))) => {
                        use std::io::Write;
                        let _ = pty_writer.write_all(&data);
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    // Ping/Pong handled by axum's ws layer automatically.
                    Some(Ok(_)) => {}
                    Some(Err(_)) => break,
                }
            }

            else => break,
        }
    }

    Ok(())
}

/// Resolve the `cwd` query param to a real directory.
/// Falls back to `$HOME` then `/tmp` if the param is absent or invalid.
fn resolve_cwd(cwd: Option<&str>) -> std::path::PathBuf {
    if let Some(p) = cwd {
        let path = std::path::Path::new(p);
        if path.is_absolute() && path.is_dir() {
            return path.to_path_buf();
        }
    }
    // Fallback: $HOME → /tmp
    std::env::var("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("/tmp"))
}
