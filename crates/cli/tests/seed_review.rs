#![allow(clippy::unwrap_used)]
//! `camerata seed-review`: the command parses, seeds a data directory through the server's stores,
//! prints what it wrote, and refuses a data directory that has something in it.

use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../server/fixtures/review-demo")
}

fn camerata(cwd: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_camerata"))
        .current_dir(cwd)
        .env("CAMERATA_NO_KEYCHAIN", "1")
        .args(args)
        .output()
        .expect("the binary runs")
}

#[test]
fn seed_review_writes_a_project_and_a_second_run_into_the_same_folder_is_refused() {
    let work = tempfile::tempdir().unwrap();
    let data = work.path().join("seed-data");
    let fixture = fixture();
    let (fixture, data_arg) = (fixture.to_str().unwrap(), data.to_str().unwrap());

    let first = camerata(
        work.path(),
        &["seed-review", fixture, "--data-dir", data_arg],
    );
    let stdout = String::from_utf8_lossy(&first.stdout);
    assert!(
        first.status.success(),
        "{stdout}{}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert!(stdout.contains("Seeded project proj-1"), "{stdout}");
    assert!(
        stdout.contains("held 5"),
        "the report carries the counts: {stdout}"
    );
    for store in [
        "projects.json",
        "inspections",
        "decisions",
        "edits",
        "engine-check",
    ] {
        assert!(
            data.join("camerata").join(store).exists(),
            "{store} was written by its own store"
        );
    }
    let before = std::fs::read_to_string(data.join("camerata/projects.json")).unwrap();

    // The folder now has something in it: refused, nothing changed.
    let again = camerata(
        work.path(),
        &["seed-review", fixture, "--data-dir", data_arg],
    );
    assert!(!again.status.success());
    let stderr = String::from_utf8_lossy(&again.stderr);
    assert!(stderr.contains("is not empty"), "{stderr}");
    assert_eq!(
        std::fs::read_to_string(data.join("camerata/projects.json")).unwrap(),
        before
    );
}

#[test]
fn seed_review_moved_variant_writes_two_inspections() {
    let work = tempfile::tempdir().unwrap();
    let data = work.path().join("seed-data");
    let out = camerata(
        work.path(),
        &[
            "seed-review",
            fixture().to_str().unwrap(),
            "--data-dir",
            data.to_str().unwrap(),
            "--variant",
            "moved",
        ],
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{stdout}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("stale decisions 1"), "{stdout}");
    assert!(stdout.contains("stale edits 1"), "{stdout}");
    assert!(stdout.contains("new since decision 1"), "{stdout}");
}

#[test]
fn seed_review_clean_variant_holds_nothing() {
    let work = tempfile::tempdir().unwrap();
    let data = work.path().join("seed-data");
    let out = camerata(
        work.path(),
        &[
            "seed-review",
            fixture().to_str().unwrap(),
            "--data-dir",
            data.to_str().unwrap(),
            "--variant",
            "clean",
        ],
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{stdout}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("held 0"), "{stdout}");
    assert!(stdout.contains("stale decisions 0"), "{stdout}");
}
