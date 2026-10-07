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
        verbose: false,
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

/// A bare-minimum synthetic git repo: no planted findings, no SQL-concat pattern anywhere —
/// unlike `e2e_report_repo` (whose `src/db.rs` deliberately plants a `SEC-NO-RAW-SQL-CONCAT-1`
/// hit via the gateway REGEX backstop), this fixture must produce ZERO real evidence for that
/// rule id from the deterministic floor, so the ONLY way it could read "verified clean" is via
/// the commodity taint (Semgrep) pass actually running and finding nothing — which this test
/// deliberately prevents, to prove the pass's ABSENCE is disclosed rather than silently read as
/// clean.
fn stage_minimal_git_repo(dest: &Path) {
    std::fs::create_dir_all(dest).unwrap();
    std::fs::write(
        dest.join("app.py"),
        "def handler(request):\n    return {\"ok\": True}\n",
    )
    .unwrap();

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
    g(&["commit", "-q", "-m", "e2e fixture: no planted findings"]);
}

/// W3 wiring-gap regression guard + the required degradation E2E: the HEADLESS `camerata
/// inspect` path must reach the exact same `scan_tools::run_scan_tools` external-tool pass the
/// server/UI scan path reaches (both go through the shared `camerata_server::merge_scan_preview`
/// — see `crates/cli/src/inspect_cmd.rs`'s call around its own `merge_scan_preview` line, and
/// `crates/server/src/lib.rs`'s `onboard_audit`/`onboard_audit_start` handlers), and when that
/// pass cannot run, the product export must disclose it loudly rather than silently reporting
/// the taint-covered rule as "verified clean".
///
/// `CAMERATA_DISABLE_SEMGREP` forces a deterministic, network-free "tool unavailable" outcome
/// (mirrors `camerata_server::dep_audit::DISABLE_ENV_VAR`'s test-isolation rationale) rather than
/// depending on whether semgrep happens to be installed on the machine running this test. If
/// this test reached `run_scan_tools`'s Semgrep arm at all — the thing this test exists to prove
/// — that env var is what turns the attempt into a clean, deterministic failure instead of a
/// real (and in this environment, network-dependent) provisioning attempt.
#[tokio::test]
async fn headless_inspect_discloses_when_the_commodity_taint_pass_does_not_run() {
    if !typst_on_path() {
        eprintln!(
            "skipping headless_inspect_discloses_when_the_commodity_taint_pass_does_not_run: \
             typst not on PATH"
        );
        return;
    }

    std::env::set_var(camerata_server::scan_tools::DISABLE_SEMGREP_ENV_VAR, "1");
    std::env::set_var("CAMERATA_DISABLE_DEP_AUDIT", "1");

    let repo_dir = tempfile::tempdir().expect("create temp dir for the synthetic git repo");
    stage_minimal_git_repo(repo_dir.path());
    let export_dir = tempfile::tempdir().expect("create temp dir for the export zip");
    let export_path = export_dir.path().join("inspect.zip");

    let args = InspectArgs {
        repo: repo_dir.path().to_path_buf(),
        export: export_path.clone(),
        backend: ProjectBackend::Api,
        batch: false,
        model: None,
        calibration_model: None,
        full: true,
        verbose: true,
    };

    let outcome = run_inspect_with_key_presence(args, false).await;

    std::env::remove_var(camerata_server::scan_tools::DISABLE_SEMGREP_ENV_VAR);
    std::env::remove_var("CAMERATA_DISABLE_DEP_AUDIT");

    let outcome = outcome.expect(
        "a headless inspection must never refuse to export, even when a deterministic pass \
         degrades",
    );

    assert!(
        export_path.is_file(),
        "the product-export zip must be written even when the commodity taint pass can't run"
    );
    assert!(outcome.export_zip_bytes > 0);

    // The headless path must surface this exact defect — an entire detection layer (the
    // external-tool/taint family) never executing — in the human-readable ledger summary, not
    // just buried inside the exported JSON. This is the regression this whole feature exists
    // to close (see `crates/cli/src/inspect_cmd.rs`'s `render_ledger_summary` doc comment).
    assert!(
        outcome.ledger_summary.contains("NOT RUN"),
        "the ledger summary must be populated: {}",
        outcome.ledger_summary
    );
    assert!(
        outcome
            .ledger_summary
            .contains("commodity taint pass did not run"),
        "the ledger summary's NOT-RUN section must name the taint-pass rule(s) that never ran: \
         {}",
        outcome.ledger_summary
    );
    assert!(
        outcome.ledger_summary.contains("FAILED PASSES"),
        "the ledger summary must include the FailedPass disclosure section: {}",
        outcome.ledger_summary
    );
    // `verbose: true` above — the per-rule detail section must also be present.
    assert!(
        outcome.ledger_summary.contains("PER-RULE DETAIL"),
        "--verbose must include the per-rule detail section: {}",
        outcome.ledger_summary
    );

    let bytes = std::fs::read(&export_path).expect("read the written export zip");
    let mut archive =
        zip::ZipArchive::new(std::io::Cursor::new(bytes)).expect("the export must be a valid zip");
    let mut findings_json = String::new();
    {
        use std::io::Read;
        let mut f = archive
            .by_name("findings.json")
            .expect("the export must contain findings.json");
        f.read_to_string(&mut findings_json)
            .expect("findings.json must be valid UTF-8");
    }
    let parsed: serde_json::Value =
        serde_json::from_str(&findings_json).expect("findings.json must be valid JSON");
    let failed_passes = parsed["summary"]["failed_passes"]
        .as_array()
        .expect("findings.json summary must carry a failed_passes array");
    // The exact rule id(s) the commodity taint layer claims to cover for a Python repo (a
    // universal id like `SEC-NO-RAW-SQL-CONCAT-1`, a Python-specific one like
    // `PYTHON-PARAMETERIZED-SQL-1`, or both, depending on the stack-exception mechanism —
    // see `crates/rules/principles/universal/sec-no-raw-sql-concat-1.toml`'s
    // `stack_exceptions`) isn't this test's concern; what matters is that AT LEAST ONE
    // semgrep-covered rule discloses the pass never ran, for EVERY such rule selected this
    // scan — never silence.
    let taint_disclosures: Vec<&str> = failed_passes
        .iter()
        .filter_map(|v| v.as_str())
        .filter(|s| s.contains("commodity taint pass"))
        .collect();
    assert!(
        !taint_disclosures.is_empty(),
        "the exported product must disclose that the commodity taint pass did not run — got \
         failed_passes: {failed_passes:?}"
    );
    for d in &taint_disclosures {
        // Every such disclosure must say the pass did NOT run, never phrase it as a clean
        // result — and must name the real reason it couldn't (the forced test-isolation var),
        // not a vague placeholder.
        assert!(d.contains("did not run"), "{d}");
        assert!(d.contains("CAMERATA_DISABLE_SEMGREP"), "{d}");
    }
}
