#![allow(clippy::unwrap_used)]
//! No test, and no gate step, may download from the internet (#587, review round 1).
//!
//! Three layers keep every test path away from the fetcher:
//! 1. In a cargo test binary the official fetcher refuses outright
//!    (`pinned_tools::tests::a_test_process_cannot_reach_the_official_fetcher`); in-process tests
//!    hand in a fixture source (`pinned_tools::fixtures::FakeTools`).
//! 2. A test that starts a Camerata binary cannot rely on that (the binary is not a test
//!    binary), so EACH spawn sets `CAMERATA_OFFLINE=1` in its own statement, or is made by the
//!    one shared helper `camerata_test_env::offline_command`: a missing tool is then a refusal,
//!    never a download. This file holds every spawn in every test to it, mechanically.
//! 3. The rehearsal, which runs the whole pipeline in process, hands in a fixture tool source and
//!    sets the offline switch as well.

use std::fs;
use std::path::{Path, PathBuf};

const OFFLINE: &str = "CAMERATA_OFFLINE";

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Every `tests/*.rs` under `crates/`.
fn workspace_test_sources() -> Vec<PathBuf> {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut files = Vec::new();
    for entry in fs::read_dir(&crates).unwrap() {
        let tests = entry.unwrap().path().join("tests");
        if tests.is_dir() {
            rust_files(&tests, &mut files);
        }
    }
    files.sort();
    files
}

fn sets_the_offline_switch(text: &str) -> bool {
    text.contains(OFFLINE) || text.contains("OFFLINE_ENV")
}

#[test]
fn the_switch_name_is_the_one_the_engine_reads() {
    assert_eq!(OFFLINE, camerata_server::pinned_tools::OFFLINE_ENV);
    assert_eq!(OFFLINE, camerata_test_env::OFFLINE_ENV);
}

/// Programs a test may start without the offline switch: fakes that are not a Camerata binary.
/// Each entry is the exact argument text of its `Command::new(...)`, with its file.
const NOT_CAMERATA: &[(&str, &str)] = &[
    // The fake `claude` the kill-and-resume test warms before the run.
    ("inspect_kill_resume_e2e.rs", r#"bin_dir.join("claude")"#),
    // `cargo` itself (it builds the binaries; it never runs a scan).
    ("build_watch_list.rs", r#"env!("CARGO")"#),
    ("support.rs", "cargo"),
    // The browser the visual tests drive.
    ("visual.rs", "&chrome"),
    ("visual.rs", "chrome"),
    // Inside a fixture string continued over lines (the scan cannot see the opening quote).
    ("anthropic_key_resolution.rs", "p"),
];

/// Every `Command::new(<arg>)` in `text` outside a string literal, with its argument text and the
/// text of its statement (up to the `;` or block `{` that ends it).
fn spawns(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut from = 0;
    while let Some(i) = text[from..].find("Command::new(") {
        let at = from + i;
        from = at + 1;
        // Skip an occurrence inside a string literal on its line (a test fixture's source).
        let line_start = text[..at].rfind('\n').map_or(0, |n| n + 1);
        let before = &text[line_start..at];
        let quotes = before.matches('"').count() - before.matches("\\\"").count();
        if quotes % 2 == 1 || before.trim_start().starts_with("//") {
            continue;
        }
        let open = at + "Command::new(".len();
        let mut depth = 1usize;
        let mut j = open;
        while j < bytes.len() && depth > 0 {
            match bytes[j] {
                b'(' => depth += 1,
                b')' => depth -= 1,
                _ => {}
            }
            j += 1;
        }
        let arg = text[open..j - 1].trim().to_string();
        // The statement: on to the `;` or `{` at nesting depth 0 that ends it.
        let mut k = j;
        let mut nest = 0i32;
        while k < bytes.len() {
            match bytes[k] {
                b'(' | b'[' => nest += 1,
                b')' | b']' => nest -= 1,
                b';' | b'{' if nest <= 0 => break,
                _ => {}
            }
            k += 1;
        }
        out.push((arg, text[at..k].to_string()));
    }
    out
}

/// Every spawn of a program a test cannot name as a literal is a possible Camerata binary, and
/// each one, on its own, must run offline: its statement sets `CAMERATA_OFFLINE`, or it is made by
/// the one shared helper (`camerata_test_env::offline_command`), which sets it. A literal
/// program (`git`, `sh`, `ps`) is never a Camerata binary. A fake that is not one is listed in
/// [`NOT_CAMERATA`].
#[test]
fn every_spawn_of_a_camerata_binary_in_any_test_runs_offline() {
    let mut seen = 0;
    let mut offenders = Vec::new();
    for path in workspace_test_sources() {
        let text = fs::read_to_string(&path).unwrap();
        let file = path.file_name().unwrap().to_string_lossy().into_owned();
        for (arg, statement) in spawns(&text) {
            if arg.starts_with('"') || NOT_CAMERATA.contains(&(file.as_str(), arg.as_str())) {
                continue;
            }
            seen += 1;
            if !sets_the_offline_switch(&statement) {
                offenders.push(format!("{}: Command::new({arg})", path.display()));
            }
        }
    }
    assert!(
        seen >= 5,
        "the scan saw only {seen} spawn(s) of a computed program"
    );
    assert!(
        offenders.is_empty(),
        "these spawns start a program that may be a Camerata binary without `{OFFLINE}=1` (set \
         it in the statement, or use camerata_test_env::offline_command):\n{}",
        offenders.join("\n")
    );
}

/// The shared helper sets the switch, so a spawn made with it runs offline.
#[test]
fn the_shared_helper_sets_the_offline_switch() {
    let cmd = camerata_test_env::offline_command(env!("CARGO_BIN_EXE_camerata"));
    assert!(
        cmd.get_envs()
            .any(|(k, v)| k == OFFLINE && v == Some(std::ffi::OsStr::new("1"))),
        "{cmd:?}"
    );
}

#[test]
fn the_rehearsal_runs_offline_on_fixture_tools() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/scan_rehearsal_e2e.rs");
    let text = fs::read_to_string(path).unwrap();
    assert!(
        sets_the_offline_switch(&text),
        "the rehearsal sets {OFFLINE}"
    );
    assert!(
        text.contains("FakeTools"),
        "the rehearsal hands in a fixture tool source, so its previews run offline"
    );
    assert!(
        text.contains("the ruff preview ran on the fake ruff")
            && text.contains("the eslint preview ran on the fake Node.js"),
        "the rehearsal asserts its ruff and eslint previews ran on the fixture tools"
    );
}
