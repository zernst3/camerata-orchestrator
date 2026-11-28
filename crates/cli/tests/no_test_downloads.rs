#![allow(clippy::unwrap_used)]
//! No test, and no gate step, may download from the internet (#587, review round 1).
//!
//! Three layers keep every test path away from the fetcher:
//! 1. In a cargo test binary the official fetcher refuses outright
//!    (`pinned_tools::tests::a_test_process_cannot_reach_the_official_fetcher`); in-process tests
//!    hand in a fixture source (`pinned_tools::fixtures::FakeTools`).
//! 2. A test that starts the real `camerata` binary cannot rely on that (the binary is not a
//!    test binary), so it sets `CAMERATA_OFFLINE=1`: a missing tool is then a refusal, never a
//!    download. This file holds every such test to it, mechanically.
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
}

#[test]
fn every_test_that_spawns_the_camerata_binary_sets_the_offline_switch() {
    let mut spawning = 0;
    let mut offenders = Vec::new();
    for path in workspace_test_sources() {
        let text = fs::read_to_string(&path).unwrap();
        // The exact `camerata` binary (not `camerata-gateway`).
        if !text.contains("CARGO_BIN_EXE_camerata\"") {
            continue;
        }
        spawning += 1;
        if !sets_the_offline_switch(&text) {
            offenders.push(path.display().to_string());
        }
    }
    assert!(
        spawning >= 3,
        "the scan saw {spawning} spawning test file(s); it should at least see \
         inspect_kill_resume_e2e.rs, inspect_plan_only.rs and spawned_binary_never_reads_keychain.rs"
    );
    assert!(
        offenders.is_empty(),
        "these tests start the real `camerata` binary without `{OFFLINE}=1`, so a missing tool \
         would be downloaded:\n{}",
        offenders.join("\n")
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
}
