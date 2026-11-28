#![allow(clippy::unwrap_used)]
//! Real SIGINTs to a real process (#583 review): the stop listener is running from the moment it
//! is set up, on a thread of its own, so the first Ctrl+C starts the stop and a second one exits at
//! once with the stop code, even while the pipeline's own thread is blocked.
//!
//! The child is this test binary, run again with an environment variable naming the role it plays.

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const CHILD: &str = "CAMERATA_STOP_SIGNAL_CHILD";

/// The child's half: set the listener up, say so, then block the thread as a stuck pipeline would
/// (`blocked`), or wait for the first signal and exit 3 (`once`).
#[test]
fn stop_signal_child() {
    let Ok(role) = std::env::var(CHILD) else {
        return;
    };
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let stop = camerata::inspect_cmd::operator_stop::spawn_stop_listener();
        println!("READY");
        use std::io::Write;
        std::io::stdout().flush().unwrap();
        if role == "blocked" {
            // The pipeline's thread never yields again.
            std::thread::sleep(Duration::from_secs(60));
            drop(stop);
        } else {
            tokio::select! {
                () = stop => std::process::exit(3),
                () = tokio::time::sleep(Duration::from_secs(60)) => {}
            }
        }
    });
    // Reaching here means no signal was handled.
    std::process::exit(0);
}

fn child(role: &str) -> Child {
    let mut c = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "stop_signal_child",
            "--nocapture",
            "--test-threads",
            "1",
        ])
        .env(CHILD, role)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let out = c.stdout.take().unwrap();
    let mut lines = BufReader::new(out).lines();
    loop {
        let line = lines.next().expect("the child said READY").unwrap();
        if line.contains("READY") {
            break;
        }
    }
    c
}

fn sigint(c: &Child) {
    let ok = Command::new("kill")
        .args(["-INT", &c.id().to_string()])
        .status()
        .unwrap()
        .success();
    assert!(ok, "kill -INT");
}

/// The child's exit status, waiting at most `limit`.
fn exit_within(c: &mut Child, limit: Duration) -> Option<std::process::ExitStatus> {
    let end = Instant::now() + limit;
    while Instant::now() < end {
        if let Some(status) = c.try_wait().unwrap() {
            return Some(status);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = c.kill();
    let _ = c.wait();
    None
}

#[cfg(unix)]
#[test]
fn a_second_ctrl_c_exits_at_once_with_the_stop_code_while_the_pipeline_is_blocked() {
    if std::env::var_os(CHILD).is_some() {
        return;
    }
    let mut c = child("blocked");
    sigint(&c);
    std::thread::sleep(Duration::from_millis(300));
    sigint(&c);
    let status = exit_within(&mut c, Duration::from_secs(10))
        .expect("the second Ctrl+C exited the process within seconds");
    assert_eq!(
        status.code(),
        Some(camerata::inspect_cmd::EXIT_OPERATOR_STOPPED),
        "{status:?}"
    );
}

#[cfg(unix)]
#[test]
fn the_first_ctrl_c_starts_the_stop_and_never_kills_the_process_outright() {
    if std::env::var_os(CHILD).is_some() {
        return;
    }
    let mut c = child("once");
    sigint(&c);
    let status = exit_within(&mut c, Duration::from_secs(10)).expect("the stop came through");
    assert_eq!(
        status.code(),
        Some(3),
        "the stop future resolved: {status:?}"
    );
}
