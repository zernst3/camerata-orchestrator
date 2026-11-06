#![allow(clippy::unwrap_used)]
//! A test must never reach the real OS keychain. A freshly built `camerata` binary has a new
//! code signature, so the first Keychain read makes macOS ask "wants to use your confidential
//! information" and the binary blocks on that dialog; a gate that runs such a test unattended
//! hangs there (the overnight run of 2026-10-28 stalled this way in the kill-and-resume tests).
//!
//! The seam is `CAMERATA_NO_KEYCHAIN=1` (`credentials::NO_KEYCHAIN_ENV`): the production
//! credential store then holds credentials in process memory and never opens the keychain, so a
//! credential resolves from its env var alone. It removes a credential SOURCE and adds none, so it
//! cannot hand a run a key it does not have: the client lock (a client inspection needs the
//! Anthropic API and a key) is as strict with the seam as without it.
//!
//! Two guards:
//! 1. Mechanical: every test file in the workspace that spawns the real `camerata` binary sets
//!    the seam. A new test that forgets it fails here, before it can hang a gate.
//! 2. Behavioural: with the seam and no `ANTHROPIC_API_KEY`, a client inspection on the API
//!    backend is refused for a missing key, whatever the machine's keychain holds.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const SEAM: &str = "CAMERATA_NO_KEYCHAIN";

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

#[test]
fn the_seam_name_is_the_one_the_credential_store_reads() {
    assert_eq!(SEAM, camerata_server::credentials::NO_KEYCHAIN_ENV);
}

#[test]
fn every_test_that_spawns_the_camerata_binary_sets_the_no_keychain_seam() {
    let mut spawning = 0;
    let mut offenders = Vec::new();
    for path in workspace_test_sources() {
        let text = fs::read_to_string(&path).unwrap();
        // The exact `camerata` binary (not `camerata-gateway`, which has no credential store).
        if !text.contains("CARGO_BIN_EXE_camerata\"") {
            continue;
        }
        spawning += 1;
        // By its literal name or by the constant that holds it.
        if !(text.contains(SEAM) || text.contains("NO_KEYCHAIN_ENV")) {
            offenders.push(path.display().to_string());
        }
    }
    assert!(
        spawning >= 2,
        "the scan saw {spawning} spawning test file(s); it should at least see this file and \
         inspect_kill_resume_e2e.rs"
    );
    assert!(
        offenders.is_empty(),
        "these tests spawn the real `camerata` binary without `{SEAM}=1`, so a fresh build would \
         block on a macOS Keychain prompt:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn with_the_seam_a_client_inspection_sees_only_the_env_key() {
    let dir = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_camerata"))
        .current_dir(dir.path())
        .args([
            "inspect",
            "--backend",
            "api",
            "--client",
            "Example Client Ltd",
        ])
        .arg("--repo")
        .arg(dir.path().join("missing-repo"))
        .arg("--export")
        .arg(dir.path().join("out.zip"))
        .env(SEAM, "1")
        .env_remove("ANTHROPIC_API_KEY")
        .env("CAMERATA_STATE_DIR", dir.path().join("state"))
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{stderr}");
    assert!(
        stderr.contains("no API key is configured"),
        "with the keychain out of reach and no ANTHROPIC_API_KEY the client lock must refuse the \
         run for a missing key:\n{stderr}"
    );
}
