//! End-to-end, ZERO model calls: `camerata inspect` wired all the way from a bare repo path
//! through `onboard::audit_repos` to a written export ZIP, over the tiny fixture repo at
//! `crates/server/tests/fixtures/e2e_report_repo/` — the SAME fixture
//! `crates/server/tests/e2e_report_pipeline.rs` uses (not a second copy: the fixture is
//! shared across both suites, reused via a relative path from `CARGO_MANIFEST_DIR`).
//!
//! AI review is forced off via the REAL compliance backend gate, not a test-only bypass:
//! `--backend api` with no `ANTHROPIC_API_KEY` resolves to
//! `camerata_server::llm::BackendResolution::Blocked`, which `audit_repos` handles by running
//! the deterministic floor only (see that function's own gate-handling doc comment) — exactly
//! the "deterministic-only (AI off / floor-only)" path Step 0's spec calls for. This proves
//! `curated_rule_selection` -> `split_scannable_rules` -> `audit_repos` -> the export assembly
//! wires together end-to-end and writes a valid ZIP, without needing a model.

use std::path::Path;

use camerata::inspect_cmd::{run_inspect_with_key_presence, InspectArgs};
use camerata_server::llm::ProjectBackend;

/// Copy the checked-in fixture tree into `dest` (a fresh temp dir), then turn `dest` into a
/// real one-commit git repo so the scan's provenance capture (`git rev-parse HEAD` etc.)
/// succeeds. Mirrors `crates/server/tests/e2e_report_pipeline.rs`'s helper of the same name.
fn stage_fixture_as_git_repo(dest: &Path) {
    let fixture_root =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../server/tests/fixtures/e2e_report_repo");
    copy_dir_recursive(&fixture_root, dest);

    let g = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .current_dir(dest)
            .args(args)
            .output()
            .unwrap_or_else(|e| panic!("failed to spawn git {args:?}: {e}"));
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    g(&["init", "-q", "-b", "main"]);
    g(&["config", "user.email", "inspect-e2e@camerata.local"]);
    g(&["config", "user.name", "Camerata Inspect E2E Test"]);
    g(&["add", "."]);
    g(&[
        "commit",
        "-q",
        "-m",
        "e2e fixture: three planted deterministic findings",
    ]);
}

fn copy_dir_recursive(src: &Path, dest: &Path) {
    std::fs::create_dir_all(dest).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let ty = entry.file_type().unwrap();
        let from = entry.path();
        let to = dest.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_recursive(&from, &to);
        } else {
            std::fs::copy(&from, &to)
                .unwrap_or_else(|e| panic!("failed to copy fixture file {from:?} -> {to:?}: {e}"));
        }
    }
}

/// Best-effort `typst` presence check (mirrors `report_export`'s own internal test gate and
/// `e2e_report_pipeline.rs`'s): the PDF-compile stage of `inspect` skips gracefully rather
/// than hard-failing CI environments without `typst` on PATH.
fn typst_on_path() -> bool {
    std::process::Command::new("typst")
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .is_some()
}

#[tokio::test]
async fn deterministic_only_inspect_runs_end_to_end_and_writes_a_valid_zip() {
    if !typst_on_path() {
        eprintln!("skipping deterministic_only_inspect_runs_end_to_end_and_writes_a_valid_zip: typst not on PATH");
        return;
    }

    let repo_dir = tempfile::tempdir().expect("create temp dir for the fixture git repo");
    stage_fixture_as_git_repo(repo_dir.path());
    let export_dir = tempfile::tempdir().expect("create temp dir for the export zip");
    let export_path = export_dir.path().join("inspect.zip");

    let args = InspectArgs {
        repo: repo_dir.path().to_path_buf(),
        export: export_path.clone(),
        // `Api` + `has_api_key: false` below resolves to `Blocked` — the real compliance
        // gate, not a test-only bypass. See this file's own doc comment.
        backend: ProjectBackend::Api,
        batch: false,
        model: None,
        calibration_model: None,
        full: true,
    };

    let outcome = run_inspect_with_key_presence(args, false)
        .await
        .expect("a floor-only headless inspection must succeed with zero model calls");

    // The floor still ran (see `audit_repos`'s gate-handling doc comment): `actual_usage` is
    // `Some(..)` with zero calls, not `None` — `None` is reserved for the OTHER Blocked branch
    // (AI-only request, nothing local either), which this run never takes.
    let calls = outcome.actual_usage.as_ref().map(|u| u.calls).unwrap_or(0);
    assert_eq!(
        calls, 0,
        "no AI review ran (compliance-blocked), so zero model calls must be recorded: {:?}",
        outcome.actual_usage
    );

    assert!(
        outcome.findings_count >= 3,
        "the fixture's 3 planted deterministic findings must have fired: {outcome:?}"
    );

    assert!(export_path.is_file(), "the export ZIP must exist on disk");
    let bytes = std::fs::read(&export_path).expect("read the written export zip");
    assert_eq!(&bytes[0..2], b"PK", "the export file must be a real zip");

    let mut archive =
        zip::ZipArchive::new(std::io::Cursor::new(bytes)).expect("the export must be a valid zip");
    let names: Vec<String> = (0..archive.len())
        .map(|i| archive.by_index(i).expect("zip entry").name().to_string())
        .collect();
    assert!(names.iter().any(|n| n.ends_with(".pdf")), "{names:?}");
    assert!(
        names.iter().any(|n| n.ends_with("-findings.xlsx")),
        "{names:?}"
    );
    assert!(names.contains(&"findings.json".to_string()), "{names:?}");
    assert!(names.contains(&"README.txt".to_string()), "{names:?}");
}
