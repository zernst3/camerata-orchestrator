//! Tests for the import of a command line run into the app's store (#562). Every run folder and
//! data folder is a temporary one; every secret is built at runtime by
//! `camerata_secrets::fixtures`.

use std::collections::BTreeMap;
use std::path::Path;

use camerata_secrets::fixtures::{planted, Planted};
use camerata_secrets::SecretShape;

use super::*;
use crate::onboard::{AuditedRef, CoverageNote, Finding, ScanReport};

mod parity;

pub(super) const RUN: &str = "run-acme-20261126-0001";
pub(super) const REPO: &str = "acme/app";
pub(super) const DIGEST: &str = "corpus-digest-acme-1";
const FINGERPRINT: &str = "rules-fp-acme-1";
const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

fn secret() -> Planted {
    planted(SecretShape::StripeLiveSecretKey)
}

fn row(rule: &str, path: &str, line: usize, severity: &str) -> Finding {
    Finding {
        repo: REPO.to_string(),
        path: path.to_string(),
        line,
        rule_id: rule.to_string(),
        severity: severity.to_string(),
        snippet: format!("run(\"{rule}\", {line})"),
        detail: format!("Title of {rule} at {line}. The call trusts the caller here."),
        kind: Some(camerata_api_types::audit::FindingKind::Defect),
        ..Finding::default()
    }
}

/// The report a command line run of the acme fixture makes: rows of two rules, one of them
/// carrying a secret no producer redacted, a coverage note and the run's usage.
pub(super) fn fixture_report() -> ScanReport {
    let mut report = ScanReport::gated(&[REPO.to_string()]);
    report.gated = false;
    report.message = None;
    report.run_id = Some(RUN.to_string());
    report.files_scanned = 12;
    report.code_lines = 4_000;
    report.provenance.started_at = "2026-11-26T20:00:00Z".to_string();
    report.provenance.finished_at = "2026-11-26T20:40:00Z".to_string();
    report.provenance.camerata_version = env!("CARGO_PKG_VERSION").to_string();
    report.provenance.rules_fingerprint = FINGERPRINT.to_string();
    report.provenance.audit_model = Some("claude-sonnet-5-5".to_string());
    report.provenance.audited_refs = vec![AuditedRef {
        repo: REPO.to_string(),
        sha: Some(COMMIT.to_string()),
        ..AuditedRef::default()
    }];
    for i in 0..4 {
        report.findings.push(row(
            "SEC-NO-RAW-SQL-CONCAT-1",
            &format!("src/q{i}.rs"),
            10 + i,
            "high",
        ));
    }
    let mut leaky = row("SEC-NO-VENDOR-TOKEN-1", ".env", 3, "critical");
    leaky.snippet = secret().text.clone();
    leaky.detail = format!("The line `{}` holds a credential.", secret().text);
    report.findings.push(leaky);
    report.coverage_notes.push(CoverageNote {
        tool: "eslint".to_string(),
        message: "eslint is not installed, so its rules were not run.".to_string(),
        repo: Some(REPO.to_string()),
        tool_failed: false,
    });
    report.actual_usage = Some(crate::ai_audit::ActualUsage {
        input_tokens: 120_000,
        output_tokens: 9_000,
        cost_usd: 1.25,
        calls: 14,
        cost_complete: true,
        ..Default::default()
    });
    report
}

/// The run record `camerata inspect` saves for a complete run of the fixture.
pub(super) fn run_record_json(run_id: &str) -> serde_json::Value {
    serde_json::json!({
        "schema": crate::run_store::STORE_SCHEMA_VERSION,
        "run_id": run_id,
        "created_at": "2026-11-26T20:00:00Z",
        "updated_at": "2026-11-26T20:40:00Z",
        "status": "complete",
        "origin": "cli",
        "attempt": 1,
        "engine_version": crate::run_store::engine_version(),
        "pin": {
            "build": {
                "version": env!("CARGO_PKG_VERSION"),
                "commit": "b".repeat(40),
                "dirty": false,
                "prompt_schema": crate::ai_audit::AUDIT_PROMPT_SCHEMA_VERSION,
                "clone_prompt_schema": 1
            },
            "corpus_version": DIGEST,
            "clone_judge_model": "claude-haiku-5-5"
        },
        "project_key": "acme-app-1234abcd",
        "repos": [{
            "name": REPO,
            "dir": "/Users/auditor/clients/acme-app",
            "commit": COMMIT,
            "clean": true,
            "tree_at_start": "tree-digest"
        }],
        "models": {
            "audit": "claude-sonnet-5-5",
            "calibration": "claude-sonnet-5-5",
            "fix": "claude-haiku-5-5",
            "backend": "api",
            "mode": "parallel",
            "resolved": true
        },
        "rules_fingerprint": FINGERPRINT,
        "ai_scan_key": {
            "audit_model": "claude-sonnet-5-5",
            "calibration_model": "",
            "prompt_schema_version": crate::ai_audit::AUDIT_PROMPT_SCHEMA_VERSION,
            "directives_fingerprint": FINGERPRINT
        },
        "nonce_seed": "00",
        "args": {},
        "plan": [{
            "id": "merge",
            "kind": "merge",
            "status": "done"
        }]
    })
}

/// A run folder as `camerata inspect` leaves it, holding `report` as its saved report: the
/// record, the saved report, a stored reply and a file of the run's export. Returns the runs root
/// and the run folder.
pub(super) struct RunFolder {
    pub _root: tempfile::TempDir,
    pub dir: PathBuf,
}

pub(super) fn run_folder_with(
    record: &serde_json::Value,
    report_bytes: Option<&[u8]>,
) -> RunFolder {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("runs").join(RUN);
    std::fs::create_dir_all(dir.join("calls")).unwrap();
    std::fs::write(
        dir.join("run.json"),
        serde_json::to_vec_pretty(record).unwrap(),
    )
    .unwrap();
    std::fs::write(dir.join("calls").join("k1.json"), br#"{"replies":[]}"#).unwrap();
    std::fs::write(dir.join("ledger.json"), br#"{"stages":[]}"#).unwrap();
    if let Some(bytes) = report_bytes {
        std::fs::write(dir.join(REPORT_FILE), bytes).unwrap();
    }
    RunFolder { _root: root, dir }
}

/// The fixture's run folder with its report saved the way the command line saves it (sealed,
/// then [`crate::run_store::write_report`]).
pub(super) fn saved_run(report: ScanReport) -> RunFolder {
    let folder = run_folder_with(&run_record_json(RUN), None);
    crate::run_store::write_report(&folder.dir.join(REPORT_FILE), &report.seal()).unwrap();
    folder
}

/// The app's data folder with one project holding the fixture's repo.
pub(super) struct App {
    pub _dir: tempfile::TempDir,
    pub state: AppState,
    pub project_id: String,
}

pub(super) fn app() -> App {
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::from_data_dir(dir.path());
    let project = state
        .projects()
        .create("Acme", vec![REPO.to_string()])
        .expect("project");
    App {
        _dir: dir,
        state,
        project_id: project.id,
    }
}

fn request(dir: &Path) -> ImportRequest<'_> {
    ImportRequest {
        run_dir: dir,
        project_id: None,
        rule_digest: DIGEST,
    }
}

/// Every file under `dir` with its bytes and its modification time.
fn snapshot(dir: &Path) -> BTreeMap<PathBuf, (Vec<u8>, std::time::SystemTime)> {
    let mut out = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else {
                let meta = std::fs::metadata(&p).unwrap();
                out.insert(
                    p.strip_prefix(dir).unwrap().to_path_buf(),
                    (std::fs::read(&p).unwrap(), meta.modified().unwrap()),
                );
            }
        }
    }
    out
}

/// The refusal of an import that must not happen, and proof that it wrote nothing.
fn refused(app: &App, req: &ImportRequest<'_>) -> ImportRefusal {
    let got = import_cli_run(&app.state, req);
    let refusal = match got {
        Err(r) => r,
        Ok(done) => panic!("the import should be refused, it did: {done:?}"),
    };
    assert!(
        app.state.inspections.list(&app.project_id).is_empty(),
        "a refused import writes nothing to the store ({})",
        refusal.code()
    );
    let text = refusal.to_string();
    for dash in ['\u{2014}', '\u{2013}'] {
        assert!(!text.contains(dash), "no dashes in operator text: {text}");
    }
    refusal
}

// ── The import ──────────────────────────────────────────────────────────────────────────────

#[test]
fn a_complete_run_is_imported_into_the_project_holding_its_repos_and_survives_a_restart() {
    let folder = saved_run(fixture_report());
    let app = app();
    let done = import_cli_run(&app.state, &request(&folder.dir));
    assert!(done.is_ok(), "the import succeeds: {done:?}");
    let done = done.unwrap();
    assert_eq!(done.project_id, app.project_id);
    assert_eq!(done.run_id, RUN);
    assert!(!done.already_imported);
    assert_eq!(done.findings, 5);
    let entries = app.state.inspections.list(&app.project_id);
    assert_eq!(entries.len(), 1, "one entry");
    assert_eq!(entries[0].id, done.entry_id);
    assert_eq!(entries[0].report.run_id.as_deref(), Some(RUN));
    assert_eq!(entries[0].repos[0].sha.as_deref(), Some(COMMIT));
    assert_eq!(entries[0].rules_fingerprint, FINGERPRINT);
    // A new app process on the same data folder sees it.
    let reopened = AppState::from_data_dir(app._dir.path());
    let latest = reopened.inspections.latest(&app.project_id);
    assert_eq!(latest.map(|e| e.id), Some(done.entry_id.clone()));
    assert!(
        done.message().contains(&done.entry_id),
        "{}",
        done.message()
    );
}

#[test]
fn the_import_applies_the_app_secret_sweep_to_a_report_saved_unswept() {
    // A report.json that reached the folder with a raw secret (written without the seal).
    let bytes = serde_json::to_vec(&fixture_report()).unwrap();
    assert!(
        String::from_utf8_lossy(&bytes).contains(&secret().value),
        "the fixture carries the raw value"
    );
    let folder = run_folder_with(&run_record_json(RUN), Some(&bytes));
    let app = app();
    let done = import_cli_run(&app.state, &request(&folder.dir));
    assert!(done.is_ok(), "the import succeeds: {done:?}");
    let stored = std::fs::read_to_string(
        app.state
            .inspections
            .file_for(&app.project_id)
            .expect("file backed"),
    )
    .unwrap();
    assert!(
        !stored.contains(&secret().value),
        "the stored entry carries only redacted text"
    );
}

#[test]
fn importing_twice_does_not_create_a_duplicate() {
    let folder = saved_run(fixture_report());
    let app = app();
    let first = import_cli_run(&app.state, &request(&folder.dir)).expect("first import");
    let second = import_cli_run(&app.state, &request(&folder.dir));
    assert!(
        second.is_ok(),
        "a second import is not an error: {second:?}"
    );
    let second = second.unwrap();
    assert!(second.already_imported, "the second import changes nothing");
    assert_eq!(second.entry_id, first.entry_id);
    assert_eq!(app.state.inspections.list(&app.project_id).len(), 1);
    // Also from a new process, which reads the store from disk.
    let reopened = AppState::from_data_dir(app._dir.path());
    let third = import_cli_run(&reopened, &request(&folder.dir)).expect("third import");
    assert!(third.already_imported);
    assert_eq!(reopened.inspections.list(&app.project_id).len(), 1);
    assert!(second.message().contains("already"), "{}", second.message());
}

#[test]
fn a_run_imported_into_one_project_is_not_imported_into_another() {
    let folder = saved_run(fixture_report());
    let app = app();
    import_cli_run(&app.state, &request(&folder.dir)).expect("first import");
    let other = app
        .state
        .projects()
        .create("Acme copy", vec![REPO.to_string()])
        .unwrap();
    let got = import_cli_run(
        &app.state,
        &ImportRequest {
            project_id: Some(&other.id),
            ..request(&folder.dir)
        },
    );
    assert_eq!(
        got.as_ref().map_err(ImportRefusal::code).err(),
        Some("imported_elsewhere")
    );
    assert!(app.state.inspections.list(&other.id).is_empty());
}

#[test]
fn the_run_folder_is_byte_identical_before_and_after_an_import() {
    let folder = saved_run(fixture_report());
    let before = snapshot(&folder.dir);
    let app = app();
    let done = import_cli_run(&app.state, &request(&folder.dir));
    assert!(done.is_ok(), "the import succeeds: {done:?}");
    import_cli_run(&app.state, &request(&folder.dir)).unwrap();
    assert_eq!(
        snapshot(&folder.dir),
        before,
        "no file of the run folder was written, added or removed"
    );
}

#[test]
fn a_project_named_with_project_is_used_when_it_holds_the_repos() {
    let folder = saved_run(fixture_report());
    let app = app();
    let second = app
        .state
        .projects()
        .create(
            "Acme second",
            vec![REPO.to_string(), "acme/web".to_string()],
        )
        .unwrap();
    let done = import_cli_run(
        &app.state,
        &ImportRequest {
            project_id: Some(&second.id),
            ..request(&folder.dir)
        },
    );
    assert_eq!(done.map(|d| d.project_id), Ok(second.id));
}

// ── Each refusal names its reason ───────────────────────────────────────────────────────────

#[test]
fn an_incomplete_run_is_refused_with_its_reasons() {
    let mut record = run_record_json(RUN);
    record["status"] = "incomplete".into();
    record["incomplete_reasons"] = serde_json::json!(["the fix stage of acme/app failed"]);
    let folder = run_folder_with(&record, None);
    crate::run_store::write_report(&folder.dir.join(REPORT_FILE), &fixture_report().seal())
        .unwrap();
    let app = app();
    let r = refused(&app, &request(&folder.dir));
    assert_eq!(r.code(), "incomplete");
    let text = r.to_string();
    assert!(text.contains("is not complete"), "{text}");
    assert!(text.contains("the fix stage of acme/app failed"), "{text}");
    assert!(
        text.contains(&format!("camerata inspect --resume {RUN}")),
        "{text}"
    );
}

#[test]
fn a_run_still_marked_running_is_refused_as_incomplete() {
    let mut record = run_record_json(RUN);
    record["status"] = "running".into();
    let folder = run_folder_with(&record, None);
    crate::run_store::write_report(&folder.dir.join(REPORT_FILE), &fixture_report().seal())
        .unwrap();
    let app = app();
    assert_eq!(refused(&app, &request(&folder.dir)).code(), "incomplete");
}

#[test]
fn a_run_with_no_engine_pin_is_refused() {
    let mut record = run_record_json(RUN);
    record.as_object_mut().unwrap().remove("pin");
    let bytes = serde_json::to_vec(&fixture_report()).unwrap();
    let folder = run_folder_with(&record, Some(&bytes));
    let app = app();
    let r = refused(&app, &request(&folder.dir));
    assert_eq!(r.code(), "pin_missing");
    assert!(r.to_string().contains("recorded no engine pin"), "{r}");
}

#[test]
fn a_run_whose_pin_names_no_build_commit_is_refused() {
    let mut record = run_record_json(RUN);
    record["pin"]["build"]["commit"] = serde_json::Value::Null;
    let bytes = serde_json::to_vec(&fixture_report()).unwrap();
    let folder = run_folder_with(&record, Some(&bytes));
    let app = app();
    let r = refused(&app, &request(&folder.dir));
    assert_eq!(r.code(), "pin_missing");
    assert!(r.to_string().contains("did not record its commit"), "{r}");
}

#[test]
fn a_report_made_by_another_version_than_the_pinned_build_is_refused() {
    let mut report = fixture_report();
    report.provenance.camerata_version = "0.0.0-acme-other".to_string();
    let folder = saved_run(report);
    let app = app();
    let r = refused(&app, &request(&folder.dir));
    assert_eq!(r.code(), "engine_pin_mismatch");
    assert!(r.to_string().contains("0.0.0-acme-other"), "{r}");
}

#[test]
fn a_report_of_another_run_is_refused() {
    let mut report = fixture_report();
    report.run_id = Some("run-acme-other".to_string());
    let folder = saved_run(report);
    let app = app();
    let r = refused(&app, &request(&folder.dir));
    assert_eq!(r.code(), "engine_pin_mismatch");
    assert!(r.to_string().contains("run-acme-other"), "{r}");
}

#[test]
fn a_report_of_another_rule_selection_is_refused() {
    let mut report = fixture_report();
    report.provenance.rules_fingerprint = "rules-fp-acme-other".to_string();
    let folder = saved_run(report);
    let app = app();
    let r = refused(&app, &request(&folder.dir));
    assert_eq!(r.code(), "engine_pin_mismatch");
    assert!(r.to_string().contains("rule selection"), "{r}");
}

#[test]
fn a_run_pinned_to_another_rule_library_is_refused() {
    let folder = saved_run(fixture_report());
    let app = app();
    let r = refused(
        &app,
        &ImportRequest {
            rule_digest: "corpus-digest-acme-2",
            ..request(&folder.dir)
        },
    );
    assert_eq!(r.code(), "rule_digest_mismatch");
    let text = r.to_string();
    assert!(text.contains(DIGEST), "{text}");
    assert!(text.contains("corpus-digest-acme-2"), "{text}");
}

#[test]
fn a_run_is_refused_when_no_rule_library_is_loaded() {
    let folder = saved_run(fixture_report());
    let app = app();
    let r = refused(
        &app,
        &ImportRequest {
            rule_digest: "",
            ..request(&folder.dir)
        },
    );
    assert_eq!(r.code(), "rule_digest_mismatch");
    assert!(r.to_string().contains("no rule library is loaded"), "{r}");
}

#[test]
fn a_run_with_no_saved_report_is_refused() {
    let folder = run_folder_with(&run_record_json(RUN), None);
    let app = app();
    let r = refused(&app, &request(&folder.dir));
    assert_eq!(r.code(), "report_missing");
    assert!(r.to_string().contains("has no saved report"), "{r}");
}

#[test]
fn a_saved_report_that_cannot_be_read_is_refused() {
    // A folder where the file should be: reading it fails whoever runs the test (root included).
    let folder = run_folder_with(&run_record_json(RUN), None);
    std::fs::create_dir(folder.dir.join(REPORT_FILE)).unwrap();
    let app = app();
    let r = refused(&app, &request(&folder.dir));
    assert_eq!(r.code(), "report_unreadable");
    assert!(r.to_string().contains("cannot be read"), "{r}");
}

#[test]
fn a_saved_report_that_does_not_parse_is_refused() {
    let folder = run_folder_with(&run_record_json(RUN), Some(b"{\"repos\": [truncated"));
    let app = app();
    let r = refused(&app, &request(&folder.dir));
    assert_eq!(r.code(), "report_unreadable");
    assert!(
        r.to_string().contains("does not read back as a report"),
        "{r}"
    );
}

#[test]
fn a_report_with_no_inspected_repo_is_refused() {
    let mut report = fixture_report();
    report.provenance.audited_refs.clear();
    let bytes = serde_json::to_vec(&report).unwrap();
    let folder = run_folder_with(&run_record_json(RUN), Some(&bytes));
    let app = app();
    assert_eq!(
        refused(&app, &request(&folder.dir)).code(),
        "not_an_inspection"
    );
}

#[test]
fn a_run_no_project_holds_is_refused_and_names_its_repos() {
    let folder = saved_run(fixture_report());
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::from_data_dir(dir.path());
    state
        .projects()
        .create("Other", vec!["acme/web".to_string()])
        .unwrap();
    let got = import_cli_run(&state, &request(&folder.dir));
    let r = got.expect_err("refused");
    assert_eq!(r.code(), "no_project");
    assert!(r.to_string().contains(REPO), "{r}");
}

#[test]
fn a_run_two_projects_hold_is_refused_until_one_is_named() {
    let folder = saved_run(fixture_report());
    let app = app();
    let twin = app
        .state
        .projects()
        .create("Acme twin", vec![REPO.to_string()])
        .unwrap();
    let r = refused(&app, &request(&folder.dir));
    assert_eq!(r.code(), "project_ambiguous");
    let text = r.to_string();
    assert!(
        text.contains(&app.project_id) && text.contains(&twin.id),
        "{text}"
    );
    assert!(text.contains("--project"), "{text}");
}

#[test]
fn a_project_that_does_not_exist_is_refused() {
    let folder = saved_run(fixture_report());
    let app = app();
    let r = refused(
        &app,
        &ImportRequest {
            project_id: Some("proj-acme-missing"),
            ..request(&folder.dir)
        },
    );
    assert_eq!(r.code(), "project_unknown");
}

#[test]
fn a_project_without_every_repo_of_the_run_is_refused() {
    let folder = saved_run(fixture_report());
    let app = app();
    let web = app
        .state
        .projects()
        .create("Acme web", vec!["acme/web".to_string()])
        .unwrap();
    let got = import_cli_run(
        &app.state,
        &ImportRequest {
            project_id: Some(&web.id),
            ..request(&folder.dir)
        },
    );
    let r = got.expect_err("refused");
    assert_eq!(r.code(), "project_repos_differ");
    assert!(r.to_string().contains(REPO), "{r}");
    assert!(app.state.inspections.list(&web.id).is_empty());
}

#[test]
fn a_missing_run_folder_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let app = app();
    let dir = root.path().join("runs").join(RUN);
    let r = refused(&app, &request(&dir));
    assert_eq!(r.code(), "run_not_found");
}

#[test]
fn a_run_record_of_another_layout_is_refused() {
    let mut record = run_record_json(RUN);
    record["schema"] = 99.into();
    let folder = run_folder_with(&record, None);
    let app = app();
    let r = refused(&app, &request(&folder.dir));
    assert_eq!(r.code(), "run_unreadable");
    assert!(r.to_string().contains("layout 99"), "{r}");
}

#[test]
fn a_run_id_that_is_not_one_folder_name_is_refused() {
    let root = Path::new("/Users/auditor/state/runs");
    for bad in ["", ".", "..", "../x", "a/b", "a\\b", " "] {
        assert_eq!(
            run_dir_in(root, bad).map_err(|e| e.code()),
            Err("invalid_run_id"),
            "{bad:?}"
        );
    }
    assert_eq!(run_dir_in(root, RUN), Ok(root.join(RUN)));
}

#[test]
fn every_refusal_text_avoids_dashes_and_serial_commas() {
    let repos = vec![
        "acme/a".to_string(),
        "acme/b".to_string(),
        "acme/c".to_string(),
    ];
    let all = [
        ImportRefusal::InvalidRunId("x".into()),
        ImportRefusal::NoProject {
            run_id: RUN.into(),
            repos: repos.clone(),
        },
        ImportRefusal::ProjectAmbiguous {
            run_id: RUN.into(),
            repos: repos.clone(),
            projects: repos.clone(),
        },
        ImportRefusal::NoDataDir,
    ];
    for r in all {
        let text = r.to_string();
        assert!(!text.contains(", and "), "no serial comma: {text}");
        assert!(
            !text.contains('\u{2014}') && !text.contains('\u{2013}'),
            "{text}"
        );
    }
    assert_eq!(plain_list(&repos), "acme/a, acme/b and acme/c");
}
